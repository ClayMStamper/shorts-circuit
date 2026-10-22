//! Runner test support. Tests need PostgreSQL: they use `DATABASE_URL` and skip (passing,
//! with a message) when it isn't set. Each test gets its own schema, dropped at the end.

#![allow(dead_code)]

use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use shorts_cloud::{AccountPut, CloudCredential, MediaRef, Secret};
use shorts_domain::{
    Capabilities, ConnectedAccount, Destination, Execution, HandoverId, MediaProperties, Platform,
    Publication, PublicationEvent, PublicationRecord, PublishAccess, RevisionContent, Schedule,
    TimeZoneName, VariantId,
};
use shorts_runner::db::Installation;
use shorts_runner::{MediaDir, PgStore, Runner, RunnerSetup, Vault};
use shorts_scheduler::{ManualClock, RetryPolicy, WorkerConfig};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use tempfile::TempDir;

pub fn t0() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-11-02T15:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

/// A database schema of its own, or `None` (after saying why) without `DATABASE_URL`.
pub struct TestDb {
    pub store: PgStore,
    admin: sqlx::PgPool,
    schema: String,
}

impl TestDb {
    pub async fn new() -> Option<Self> {
        let Ok(url) = std::env::var("DATABASE_URL") else {
            eprintln!("skipping: DATABASE_URL is not set (PostgreSQL is needed for runner tests)");
            return None;
        };
        let options = PgConnectOptions::from_str(&url).expect("DATABASE_URL is a Postgres URL");
        let admin = PgPoolOptions::new()
            .max_connections(2)
            .connect_with(options.clone())
            .await
            .expect("connect to DATABASE_URL");
        let schema = format!("t_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&admin)
            .await
            .unwrap();
        let vault = Vault::new(&Vault::generate_key(), &[]).unwrap();
        let store = PgStore::connect(
            options.options([("search_path", schema.as_str())]),
            Arc::new(vault),
        )
        .await
        .expect("migrate the test schema");
        Some(Self {
            store,
            admin,
            schema,
        })
    }

    /// A store over the same schema with another vault (for key rotation tests).
    pub async fn reopen(&self, vault: Vault) -> PgStore {
        let url = std::env::var("DATABASE_URL").unwrap();
        let options = PgConnectOptions::from_str(&url).unwrap();
        PgStore::connect(
            options.options([("search_path", self.schema.as_str())]),
            Arc::new(vault),
        )
        .await
        .unwrap()
    }

    pub async fn drop(self) {
        self.store.close().await;
        let _ = sqlx::query(&format!("DROP SCHEMA {} CASCADE", self.schema))
            .execute(&self.admin)
            .await;
        self.admin.close().await;
    }
}

pub fn props() -> MediaProperties {
    MediaProperties {
        duration_ms: 20_000,
        width: 1080,
        height: 1920,
        audio_streams: 1,
        video_codec: Some("h264".into()),
        audio_codec: Some("aac".into()),
        ..Default::default()
    }
}

pub fn automatic(platform: Platform, name: &str) -> ConnectedAccount {
    let mut account = ConnectedAccount::manual(platform, name, t0());
    account.capabilities = Capabilities {
        publish_access: PublishAccess::Automatic,
        native_scheduling: false,
        analytics: false,
        manual_handoff: true,
        private_only: false,
        notes: Vec::new(),
        checked_at: t0(),
    };
    account.credential_ref = Some(format!("youtube:{}", account.id));
    account
}

pub fn youtube_credential(refresh: &str) -> CloudCredential {
    CloudCredential::YoutubeShorts {
        client_id: "fake-client.apps.googleusercontent.com".into(),
        client_secret: Secret::new("fake-client-secret"),
        refresh_token: Secret::new(refresh),
        channel_id: "UCfakechannel".into(),
    }
}

/// An approved publication for `account` due at `at`, just handed to the cloud.
pub fn handed_over(account: &ConnectedAccount, at: DateTime<Utc>) -> (Publication, HandoverId) {
    let content = RevisionContent::new(
        VariantId::new(),
        1,
        "A caption",
        &["shorts".into()],
        Destination {
            account_id: account.id,
            platform: account.platform,
        },
    )
    .unwrap();
    let mut p = Publication::draft(
        shorts_domain::AssetId::new(),
        content,
        Some(Schedule::new(
            at,
            TimeZoneName::parse("Europe/London").unwrap(),
        )),
        t0(),
    );
    p.apply(PublicationEvent::SubmitForReview, t0()).unwrap();
    p.apply(
        PublicationEvent::Approve {
            revision: 1,
            current_variant_revision: 1,
            by: "owner".into(),
        },
        t0(),
    )
    .unwrap();
    let handover = HandoverId::new();
    p.apply(
        PublicationEvent::HandToCloud {
            handover,
            execution: Execution::IDLE,
        },
        t0(),
    )
    .unwrap();
    (p, handover)
}

pub fn record(p: &Publication) -> PublicationRecord {
    p.record().clone()
}

/// A runner over the test database, with media in a temporary directory.
pub struct Fixture {
    pub db: TestDb,
    pub dir: TempDir,
    pub runner: Arc<Runner>,
    pub clock: ManualClock,
    pub installation: Installation,
    pub token: String,
    pub account: ConnectedAccount,
}

impl Fixture {
    pub async fn new() -> Option<Self> {
        Self::with_setup(RunnerSetup::default()).await
    }

    pub async fn with_setup(setup: RunnerSetup) -> Option<Self> {
        let db = TestDb::new().await?;
        let dir = tempfile::tempdir().unwrap();
        let clock = ManualClock::new(t0());
        let runner = Runner::new(
            db.store.clone(),
            MediaDir::new(dir.path().join("data")),
            setup,
            Arc::new(clock.clone()),
        );
        let (installation, token) = db.store.create_installation("Test PC", t0()).await.unwrap();
        let account = automatic(Platform::YoutubeShorts, "Test channel");
        db.store
            .put_account(
                installation.id,
                &AccountPut {
                    account: account.clone(),
                    credential: youtube_credential("fake-refresh-1"),
                    base_revision: None,
                },
                t0(),
            )
            .await
            .unwrap();
        Some(Self {
            db,
            dir,
            runner,
            clock,
            installation,
            token,
            account,
        })
    }

    pub fn store(&self) -> &PgStore {
        &self.db.store
    }

    /// Stores `bytes` as a finished upload and returns its reference.
    pub async fn upload(&self, bytes: &[u8]) -> MediaRef {
        let sha = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(bytes));
        let inst = self.installation.id;
        let store = self.store();
        store
            .start_upload(inst, &sha, bytes.len() as u64, t0())
            .await
            .unwrap();
        if !bytes.is_empty() {
            self.runner
                .media()
                .write_chunk(inst, &sha, 0, bytes)
                .await
                .unwrap();
            store
                .advance_upload(inst, &sha, 0, bytes.len() as u64, t0())
                .await
                .unwrap();
        }
        assert!(self.runner.media().finish(inst, &sha).await.unwrap());
        store.complete_upload(inst, &sha, t0()).await.unwrap();
        MediaRef {
            sha256: sha,
            size_bytes: bytes.len() as u64,
            properties: props(),
        }
    }

    /// Hands over a new publication due at `at`, with its media uploaded.
    pub async fn hand_over(&self, at: DateTime<Utc>) -> (Publication, HandoverId) {
        let (p, handover) = handed_over(&self.account, at);
        let media = self.upload(format!("video {}", p.id()).as_bytes()).await;
        self.store()
            .receive_handover(
                self.installation.id,
                &shorts_cloud::HandoverPut {
                    handover,
                    publication: record(&p),
                    media,
                },
                t0(),
            )
            .await
            .unwrap();
        (p, handover)
    }

    pub async fn finish(self) {
        self.db.drop().await;
    }
}

pub fn worker_config() -> WorkerConfig {
    WorkerConfig {
        poll_interval: Duration::from_millis(20),
        retry: RetryPolicy {
            base: Duration::from_secs(10),
            max: Duration::from_secs(300),
            max_failures: 6,
        },
        concurrency: 1,
        ..WorkerConfig::default()
    }
}
