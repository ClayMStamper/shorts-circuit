//! Cloud mode end to end: the desktop app, an in-process cloud runner on PostgreSQL, and fake
//! adapters on both sides that count publishes. Skips (passing) without `DATABASE_URL`; the
//! off-by-default checks run everywhere.

mod support;

use std::str::FromStr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::http::StatusCode;
use axum::response::IntoResponse;
use chrono::Duration;
use shorts_adapters::AdapterRegistry;
use shorts_adapters::fake::{Call, FakeAdapter};
use shorts_desktop::cloud::{CloudSync, TOKEN_KEY};
use shorts_desktop::credentials::{CredentialStore, MemoryCredentials};
use shorts_desktop::views::Action;
use shorts_desktop::{AssetUpdate, ImportOutcome, NewPublication, PermissionInput, ScheduleInput};
use shorts_domain::{
    AccountId, Capabilities, ConnectedAccount, ExecutionOwner, JobKind, JobState,
    OutcomeResolution, PermissionStatus, Platform, PublicationId, PublicationState, PublishAccess,
};
use shorts_runner::{MediaDir, PgStore, Runner, RunnerSetup, Vault};
use shorts_scheduler::{Disposition, JobQueue, ManualClock};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use support::{Harness, harness_with, t0, worker_config};
use tokio_util::sync::CancellationToken;

const CHANNEL: &str = "UCfakechannel";

struct Cloud {
    h: Harness,
    sync: Arc<CloudSync>,
    credentials: Arc<MemoryCredentials>,
    desktop_fake: Arc<FakeAdapter>,
    runner: Option<RunnerSide>,
}

struct RunnerSide {
    runner: Arc<Runner>,
    fake: Arc<FakeAdapter>,
    url: String,
    token: String,
    down: Arc<AtomicBool>,
    shutdown: CancellationToken,
    admin: sqlx::PgPool,
    schema: String,
    _media: tempfile::TempDir,
}

impl RunnerSide {
    async fn start(clock: ManualClock) -> Option<Self> {
        let Ok(url) = std::env::var("DATABASE_URL") else {
            eprintln!("skipping: DATABASE_URL is not set (PostgreSQL is needed for the runner)");
            return None;
        };
        let options = PgConnectOptions::from_str(&url).unwrap();
        let admin = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(options.clone())
            .await
            .unwrap();
        let schema = format!("t_{}", uuid::Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&admin)
            .await
            .unwrap();
        let store = PgStore::connect(
            options.options([("search_path", schema.as_str())]),
            Arc::new(Vault::new(&Vault::generate_key(), &[]).unwrap()),
        )
        .await
        .unwrap();
        let media = tempfile::tempdir().unwrap();
        let runner = Runner::new(
            store.clone(),
            MediaDir::new(media.path()),
            RunnerSetup::default(),
            Arc::new(clock),
        );
        let fake = Arc::new(FakeAdapter::new(Platform::YoutubeShorts, 2));
        runner.register_adapter(fake.clone());
        let (_, token) = store.create_installation("Test PC", t0()).await.unwrap();

        let down = Arc::new(AtomicBool::new(false));
        let gate = down.clone();
        let app = shorts_runner::api::router(runner.clone()).layer(axum::middleware::from_fn(
            move |req, next: axum::middleware::Next| {
                let gate = gate.clone();
                async move {
                    if gate.load(Ordering::SeqCst) {
                        StatusCode::SERVICE_UNAVAILABLE.into_response()
                    } else {
                        next.run(req).await
                    }
                }
            },
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let shutdown = CancellationToken::new();
        let stop = shutdown.clone();
        tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async move { stop.cancelled().await })
                .await
                .unwrap();
        });
        Some(Self {
            runner,
            fake,
            url,
            token,
            down,
            shutdown,
            admin,
            schema,
            _media: media,
        })
    }

    fn store(&self) -> &PgStore {
        self.runner.store()
    }

    async fn run_once(&self) -> Option<Disposition> {
        let worker = self.runner.worker(worker_config());
        worker.run_once().await.unwrap().map(|(_, d)| d)
    }

    async fn stop(self) {
        self.shutdown.cancel();
        self.runner.store().close().await;
        let _ = sqlx::query(&format!("DROP SCHEMA {} CASCADE", self.schema))
            .execute(&self.admin)
            .await;
    }
}

impl Cloud {
    async fn new(with_runner: bool) -> Option<Self> {
        let desktop_fake = Arc::new(FakeAdapter::new(Platform::YoutubeShorts, 2));
        let mut registry = AdapterRegistry::new();
        registry.register(desktop_fake.clone());
        let h = harness_with(registry).await;
        // Both sides share one clock.
        let runner = if with_runner {
            Some(RunnerSide::start(h.clock.clone()).await?)
        } else {
            None
        };
        let credentials = Arc::new(MemoryCredentials::default());
        let sync = CloudSync::new(h.app.clone(), credentials.clone());
        Some(Self {
            h,
            sync,
            credentials,
            desktop_fake,
            runner,
        })
    }

    fn runner(&self) -> &RunnerSide {
        self.runner.as_ref().expect("a runner")
    }

    async fn configure(&self) {
        let r = self.runner();
        self.sync.configure(&r.url, &r.token).await.unwrap();
    }

    /// A connected YouTube channel (signed in, as far as the credential store goes).
    async fn channel(&self) -> ConnectedAccount {
        let mut account = ConnectedAccount::manual(Platform::YoutubeShorts, "Channel", t0());
        account.capabilities = Capabilities {
            publish_access: PublishAccess::Automatic,
            ..Capabilities::manual_only(t0())
        };
        account.credential_ref = Some(format!("youtube:{}", account.id));
        self.h
            .app
            .store()
            .accounts()
            .insert(&account)
            .await
            .unwrap();
        self.credentials
            .set(
                "google_oauth_client",
                r#"{"client_id":"fake-client.apps.googleusercontent.com","client_secret":"fake-secret"}"#,
            )
            .unwrap();
        self.set_refresh_token(account.id, "fake-refresh-1");
        account
    }

    fn set_refresh_token(&self, account: AccountId, token: &str) {
        let grant = serde_json::json!({
            "refresh_token": token,
            "channel_id": CHANNEL,
            "scope": "https://www.googleapis.com/auth/youtube.upload",
            "granted_at": t0(),
            "expires_at": null,
        });
        self.credentials
            .set(&format!("youtube:{account}"), &grant.to_string())
            .unwrap();
    }

    fn refresh_token(&self, account: AccountId) -> String {
        let raw = self
            .credentials
            .get(&format!("youtube:{account}"))
            .unwrap()
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&raw).unwrap();
        v["refresh_token"].as_str().unwrap().to_owned()
    }

    /// An approved publication for `account`, due `at`, with its variant rendered.
    async fn approved(&self, account: &ConnectedAccount, when: ScheduleInput) -> PublicationId {
        let h = &self.h;
        let path = h.video(
            &format!("{}.mp4", uuid::Uuid::new_v4()),
            format!("video bytes {}", uuid::Uuid::new_v4()).as_bytes(),
        );
        let ImportOutcome::Imported { asset_id } = h.app.import_file(&path, None).await.unwrap()
        else {
            panic!("expected an import")
        };
        let granted = || {
            Some(PermissionInput {
                status: PermissionStatus::Granted,
                note: None,
            })
        };
        h.app
            .update_asset(
                asset_id,
                AssetUpdate {
                    video_permission: granted(),
                    audio_permission: granted(),
                    ..Default::default()
                },
            )
            .await
            .unwrap();
        let detail = h.app.asset_detail(asset_id).await.unwrap();
        let variant = detail.variants[0].id.parse().unwrap();
        let view = h
            .app
            .create_publication(NewPublication {
                asset_id,
                variant_id: variant,
                account_id: account.id,
                caption: "Made in the cloud".into(),
                hashtags: vec!["shorts".into()],
                schedule: Some(when),
            })
            .await
            .unwrap();
        let id: PublicationId = view.id.parse().unwrap();
        h.app.submit_for_review(id).await.unwrap();
        h.app.render_variant(variant).await.unwrap();
        h.drain().await;
        h.app.approve(id, 1).await.unwrap();
        id
    }

    async fn desktop_job_state(&self, id: PublicationId) -> Option<JobState> {
        self.h
            .app
            .store()
            .jobs()
            .latest_for(JobKind::PublishPublication, &id.to_string())
            .await
            .unwrap()
            .map(|j| j.state())
    }

    async fn finish(self) {
        if let Some(r) = self.runner {
            r.stop().await;
        }
    }
}

fn later() -> ScheduleInput {
    ScheduleInput::Local {
        local: (t0() + Duration::hours(3))
            .format("%Y-%m-%dT%H:%M")
            .to_string(),
        timezone: Some("UTC".into()),
    }
}

#[tokio::test]
async fn cloud_mode_is_off_by_default_and_changes_nothing() {
    let c = Cloud::new(false).await.unwrap();
    let status = c.sync.status().await.unwrap();
    assert!(!status.enabled);
    assert!(status.url.is_none());
    assert!(status.next_step.is_some());
    // Syncing does nothing (there is no runner to talk to).
    c.sync.sync_once().await.unwrap();

    let account = c.channel().await;
    let id = c.approved(&account, ScheduleInput::Now).await;
    let view = c.h.app.publication(id).await.unwrap();
    assert_eq!(view.owner, ExecutionOwner::Desktop);
    assert!(!view.actions.contains(&Action::SendToCloud));
    assert!(c.sync.send(id).await.is_err());
    assert!(
        c.sync.set_enabled(true).await.is_err(),
        "needs a runner first"
    );

    // The desktop publishes it exactly as before.
    assert_eq!(c.h.drain().await, vec![Disposition::Succeeded]);
    assert_eq!(c.desktop_fake.count(Call::Publish), 1);
    c.finish().await;
}

#[tokio::test]
async fn runner_addresses_must_be_https_and_tokens_valid() {
    let Some(c) = Cloud::new(true).await else {
        return;
    };
    let err = c
        .sync
        .configure("http://runner.example.com", "sc_x")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("https"), "{err}");
    let err = c
        .sync
        .configure(&c.runner().url, "sc_wrong")
        .await
        .unwrap_err();
    assert_eq!(err.kind(), "cloud");
    assert!(!c.sync.status().await.unwrap().enabled);
    assert!(c.credentials.get(TOKEN_KEY).unwrap().is_none());

    c.configure().await;
    let status = c.sync.status().await.unwrap();
    assert!(status.enabled && status.token_saved);
    c.finish().await;
}

#[tokio::test]
async fn the_cloud_runner_publishes_while_the_desktop_never_does() {
    let Some(c) = Cloud::new(true).await else {
        return;
    };
    c.configure().await;
    let account = c.channel().await;
    let id = c.approved(&account, ScheduleInput::Now).await;
    assert!(
        c.h.app
            .publication(id)
            .await
            .unwrap()
            .actions
            .contains(&Action::SendToCloud)
    );

    let view = c.sync.send(id).await.unwrap();
    assert_eq!(view.owner, ExecutionOwner::Cloud);
    assert_eq!(view.actions, vec![Action::TakeBack]);
    assert_eq!(view.cloud_note, None, "delivered");
    assert_eq!(c.desktop_job_state(id).await, Some(JobState::Cancelled));
    // The user can't edit it while the runner has it.
    assert!(c.h.app.cancel_publication(id).await.is_err());

    // The desktop worker finds nothing to do.
    assert!(c.h.drain().await.is_empty());

    assert_eq!(c.runner().run_once().await, Some(Disposition::Succeeded));
    // However late it gets.
    c.h.clock.advance(Duration::hours(1));
    c.sync.sync_once().await.unwrap();
    let view = c.h.app.publication(id).await.unwrap();
    assert_eq!(view.state, PublicationState::Published);
    assert!(
        view.published_url
            .as_deref()
            .unwrap()
            .starts_with("https://example.invalid/")
    );
    assert!(
        view.status_text.contains("cloud runner"),
        "{}",
        view.status_text
    );
    assert_eq!(c.runner().fake.count(Call::Publish), 1);
    assert_eq!(c.desktop_fake.count(Call::Publish), 0);
    assert!(c.h.drain().await.is_empty());
    c.finish().await;
}

#[tokio::test]
async fn taking_back_before_the_posting_time_returns_it_to_the_desktop() {
    let Some(c) = Cloud::new(true).await else {
        return;
    };
    c.configure().await;
    let account = c.channel().await;
    let id = c.approved(&account, later()).await;
    c.sync.send(id).await.unwrap();
    assert_eq!(c.sync.status().await.unwrap().held, 1);
    assert!(
        c.sync.set_enabled(false).await.is_err(),
        "it holds a publication"
    );

    let view = c.sync.take_back(id).await.unwrap();
    assert_eq!(view.owner, ExecutionOwner::Desktop);
    assert_eq!(view.state, PublicationState::Scheduled);
    assert_eq!(c.desktop_job_state(id).await, Some(JobState::Queued));

    // The runner never runs it; the desktop does at the posting time.
    c.h.clock.advance(Duration::hours(3));
    assert_eq!(c.runner().run_once().await, None);
    assert_eq!(c.h.drain().await, vec![Disposition::Succeeded]);
    assert_eq!(c.desktop_fake.count(Call::Publish), 1);
    assert_eq!(c.runner().fake.count(Call::Publish), 0);
    c.sync.set_enabled(false).await.unwrap();
    c.finish().await;
}

#[tokio::test]
async fn taking_back_is_refused_while_the_runner_works_on_it() {
    let Some(c) = Cloud::new(true).await else {
        return;
    };
    c.configure().await;
    let account = c.channel().await;
    let id = c.approved(&account, ScheduleInput::Now).await;
    c.sync.send(id).await.unwrap();
    // A runner worker has claimed it.
    let job = c
        .runner()
        .store()
        .claim_next("cloud:busy", t0(), Duration::seconds(600), &[])
        .await
        .unwrap()
        .unwrap();
    let err = c.sync.take_back(id).await.unwrap_err();
    assert!(err.to_string().contains("can't give it back"), "{err}");
    let view = c.h.app.publication(id).await.unwrap();
    assert_eq!(view.owner, ExecutionOwner::Cloud);
    c.runner()
        .store()
        .modify_job(job.id(), |j| j.release("cloud:busy", t0()))
        .await
        .unwrap();
    c.sync.take_back(id).await.unwrap();
    c.finish().await;
}

#[tokio::test]
async fn a_failed_delivery_is_retried_by_the_next_sync() {
    let Some(c) = Cloud::new(true).await else {
        return;
    };
    c.configure().await;
    let account = c.channel().await;
    let id = c.approved(&account, ScheduleInput::Now).await;
    c.runner().down.store(true, Ordering::SeqCst);
    let view = c.sync.send(id).await.unwrap();
    assert_eq!(view.owner, ExecutionOwner::Cloud, "ownership moved first");
    assert!(view.cloud_note.is_some(), "{view:?}");
    assert!(c.h.drain().await.is_empty(), "the desktop never runs it");
    assert!(c.sync.sync_once().await.is_err());

    c.runner().down.store(false, Ordering::SeqCst);
    c.sync.sync_once().await.unwrap();
    assert_eq!(c.h.app.publication(id).await.unwrap().cloud_note, None);
    assert_eq!(c.runner().run_once().await, Some(Disposition::Succeeded));
    c.sync.sync_once().await.unwrap();
    assert_eq!(
        c.h.app.publication(id).await.unwrap().state,
        PublicationState::Published
    );
    c.finish().await;
}

#[tokio::test]
async fn an_unknown_outcome_is_resolved_on_the_desktop_after_taking_it_back() {
    let Some(c) = Cloud::new(true).await else {
        return;
    };
    c.configure().await;
    let account = c.channel().await;
    let id = c.approved(&account, ScheduleInput::Now).await;
    c.sync.send(id).await.unwrap();
    *c.runner().fake.lose_publish_response.lock().unwrap() = true;
    c.runner().fake.fail_next(
        Call::Reconcile,
        shorts_adapters::AdapterError::Unsupported {
            message: "no lookup".into(),
        },
    );
    assert!(matches!(
        c.runner().run_once().await,
        Some(Disposition::Paused { .. })
    ));
    c.sync.sync_once().await.unwrap();
    let view = c.h.app.publication(id).await.unwrap();
    assert_eq!(view.state, PublicationState::OutcomeUnknown);
    assert_eq!(view.actions, vec![Action::TakeBack]);
    assert!(
        c.h.app
            .resolve_outcome(id, OutcomeResolution::NotPublished)
            .await
            .is_err()
    );

    let view = c.sync.take_back(id).await.unwrap();
    assert_eq!(view.state, PublicationState::OutcomeUnknown);
    assert_eq!(view.actions, vec![Action::ResolveOutcome]);
    // The user checked: it was posted.
    let view =
        c.h.app
            .resolve_outcome(
                id,
                OutcomeResolution::Published {
                    url: Some("https://youtube.com/shorts/fake".into()),
                    remote_id: None,
                },
            )
            .await
            .unwrap();
    assert_eq!(view.state, PublicationState::Published);
    assert_eq!(c.runner().fake.count(Call::Publish), 1);
    assert_eq!(c.desktop_fake.count(Call::Publish), 0);
    c.finish().await;
}

#[tokio::test]
async fn credentials_rotated_by_the_runner_come_back_and_reconnects_go_out() {
    let Some(c) = Cloud::new(true).await else {
        return;
    };
    c.configure().await;
    let account = c.channel().await;
    let id = c.approved(&account, later()).await;
    c.sync.send(id).await.unwrap();

    // The runner refreshed the channel and Google rotated the refresh token.
    let rotated = shorts_cloud::CloudCredential::YoutubeShorts {
        client_id: "fake-client.apps.googleusercontent.com".into(),
        client_secret: shorts_cloud::Secret::new("fake-secret"),
        refresh_token: shorts_cloud::Secret::new("fake-refresh-rotated"),
        channel_id: CHANNEL.into(),
    };
    c.runner()
        .store()
        .save_rotated_credential(account.id, 1, &rotated, t0())
        .await
        .unwrap()
        .unwrap();
    c.sync.sync_once().await.unwrap();
    assert_eq!(c.refresh_token(account.id), "fake-refresh-rotated");

    // Later the runner's sign-in is refused and the owner reconnects on the desktop.
    c.h.clock.advance(Duration::hours(3));
    c.runner().fake.fail_next(
        Call::Validate,
        shorts_adapters::AdapterError::Authentication {
            message: "revoked".into(),
        },
    );
    assert!(matches!(
        c.runner().run_once().await,
        Some(Disposition::Paused { .. })
    ));
    c.sync.sync_once().await.unwrap();
    let view = c.h.app.publication(id).await.unwrap();
    assert_eq!(view.state, PublicationState::NeedsAuthentication);
    assert!(view.actions.contains(&Action::Reconnect));

    c.set_refresh_token(account.id, "fake-refresh-reconnected");
    c.sync.sync_once().await.unwrap();
    let stored = c
        .runner()
        .store()
        .credential(account.id)
        .await
        .unwrap()
        .unwrap();
    let shorts_cloud::CloudCredential::YoutubeShorts { refresh_token, .. } = stored.credential
    else {
        panic!()
    };
    assert_eq!(refresh_token.expose(), "fake-refresh-reconnected");
    assert_eq!(c.runner().run_once().await, Some(Disposition::Succeeded));
    c.sync.sync_once().await.unwrap();
    assert_eq!(
        c.h.app.publication(id).await.unwrap().state,
        PublicationState::Published
    );
    c.finish().await;
}
