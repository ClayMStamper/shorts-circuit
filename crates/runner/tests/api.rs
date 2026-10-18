//! The sync API over HTTP, driven with the same client the desktop uses.

mod support;

use std::sync::Arc;

use chrono::Duration;
use shorts_adapters::fake::{Call, FakeAdapter};
use shorts_cloud::{
    AccountPut, ClientError, CloudClient, CredentialSource, ErrorCode, HandoverPut, MediaRef,
    sha256_file,
};
use shorts_domain::{ExecutionOwner, Platform, PublicationState};
use support::{Fixture, props, record, t0, worker_config, youtube_credential};
use tokio_util::sync::CancellationToken;

struct Served {
    f: Fixture,
    client: CloudClient,
    base: String,
    shutdown: CancellationToken,
}

async fn serve() -> Option<Served> {
    let f = Fixture::new().await?;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let app = shorts_runner::api::router(f.runner.clone());
    let shutdown = CancellationToken::new();
    let stop = shutdown.clone();
    tokio::spawn(async move {
        axum::serve(listener, app)
            .with_graceful_shutdown(async move { stop.cancelled().await })
            .await
            .unwrap();
    });
    let client = CloudClient::new(&base, &f.token)
        .unwrap()
        .with_chunk_bytes(4);
    Some(Served {
        f,
        client,
        base,
        shutdown,
    })
}

impl Served {
    async fn finish(self) {
        self.shutdown.cancel();
        self.f.finish().await;
    }

    fn video(&self, bytes: &[u8]) -> std::path::PathBuf {
        let path = self
            .f
            .dir
            .path()
            .join(format!("{}.mp4", uuid::Uuid::new_v4()));
        std::fs::write(&path, bytes).unwrap();
        path
    }
}

fn code(e: &ClientError) -> Option<ErrorCode> {
    e.code()
}

#[tokio::test]
async fn requests_need_a_valid_token() {
    let Some(s) = serve().await else { return };
    let health = reqwest::get(format!("{}/healthz", s.base)).await.unwrap();
    assert_eq!(health.status(), 200);

    for token in ["sc_not-a-real-token", "nonsense"] {
        let stranger = CloudClient::new(&s.base, token).unwrap();
        let err = stranger.accounts().await.unwrap_err();
        assert_eq!(code(&err), Some(ErrorCode::Unauthorized), "{err:?}");
    }
    let raw = reqwest::get(format!("{}/v1/accounts", s.base))
        .await
        .unwrap();
    assert_eq!(raw.status(), 401);

    s.client.check().await.unwrap();
    s.f.store()
        .revoke_installation(s.f.installation.id, t0())
        .await
        .unwrap();
    let err = s.client.accounts().await.unwrap_err();
    assert_eq!(code(&err), Some(ErrorCode::Unauthorized));
    s.finish().await;
}

#[tokio::test]
async fn uploads_resume_where_they_stopped_and_are_verified() {
    let Some(s) = serve().await else { return };
    let bytes = b"0123456789abcdefghij-a-short-fake-video";
    let path = s.video(bytes);
    let sha = sha256_file(&path).await.unwrap();

    // A first attempt sends one chunk and is interrupted.
    let http = reqwest::Client::new();
    let start = http
        .post(format!("{}/v1/uploads", s.base))
        .bearer_auth(&s.f.token)
        .json(&serde_json::json!({"sha256": sha, "size_bytes": bytes.len()}))
        .send()
        .await
        .unwrap();
    assert_eq!(start.status(), 200);
    let chunk = http
        .put(format!("{}/v1/uploads/{sha}?offset=0", s.base))
        .bearer_auth(&s.f.token)
        .body(bytes[..8].to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(chunk.status(), 200);
    // A chunk at the wrong offset is refused with where the upload is.
    let wrong = http
        .put(format!("{}/v1/uploads/{sha}?offset=3", s.base))
        .bearer_auth(&s.f.token)
        .body(bytes[3..6].to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), 409);
    let body: shorts_cloud::ApiError = wrong.json().await.unwrap();
    assert_eq!(body.code, ErrorCode::OffsetMismatch);
    assert_eq!(body.received_bytes, Some(8));

    let fractions = std::sync::Mutex::new(Vec::new());
    let status = s
        .client
        .upload_file(&path, &sha, &|f| fractions.lock().unwrap().push(f))
        .await
        .unwrap();
    assert!(status.complete);
    assert_eq!(status.received_bytes, bytes.len() as u64);
    let fractions = fractions.into_inner().unwrap();
    assert!(fractions.first().unwrap() > &0.2, "resumed, not restarted");
    assert_eq!(*fractions.last().unwrap(), 1.0);
    let stored = s.f.runner.media().path(s.f.installation.id, &sha);
    assert_eq!(std::fs::read(stored).unwrap(), bytes);

    // Uploading again is a no-op.
    let again = s.client.upload_file(&path, &sha, &|_| {}).await.unwrap();
    assert_eq!(again, status);

    // Bytes that don't match their digest are refused and the upload starts over.
    let liar = s.video(b"these bytes are not what the digest says");
    let err = s
        .client
        .upload_file(&liar, &"a".repeat(64), &|_| {})
        .await
        .unwrap_err();
    assert_eq!(code(&err), Some(ErrorCode::ChecksumMismatch), "{err:?}");
    let reset = s.client.upload_status(&"a".repeat(64)).await.unwrap();
    assert_eq!(reset.received_bytes, 0);
    assert!(!reset.complete);

    // Digests are checked before they name files.
    let err = s
        .client
        .upload_status("../../etc/passwd")
        .await
        .unwrap_err();
    assert!(matches!(
        code(&err),
        Some(ErrorCode::Invalid | ErrorCode::NotFound)
    ));
    s.finish().await;
}

#[tokio::test]
async fn a_handed_over_publication_is_published_and_reported_back() {
    let Some(s) = serve().await else { return };
    let fake = Arc::new(FakeAdapter::new(Platform::YoutubeShorts, 2));
    s.f.runner.register_adapter(fake.clone());

    let path = s.video(b"a fake rendered short");
    let sha = sha256_file(&path).await.unwrap();
    s.client.upload_file(&path, &sha, &|_| {}).await.unwrap();
    let (p, handover) = support::handed_over(&s.f.account, t0());
    let put = HandoverPut {
        handover,
        publication: record(&p),
        media: MediaRef {
            sha256: sha.clone(),
            size_bytes: 21,
            properties: props(),
        },
    };
    let first = s.client.hand_over(&put).await.unwrap();
    assert_eq!(first.report.state, PublicationState::Scheduled);
    assert_eq!(first.report.owner, ExecutionOwner::Cloud);
    assert_eq!(first.report.handover, handover);
    // Delivering again (a lost answer) is idempotent.
    let second = s.client.hand_over(&put).await.unwrap();
    assert_eq!(second.report, first.report);

    let worker = s.f.runner.worker(worker_config());
    worker.run_once().await.unwrap().unwrap();
    assert!(worker.run_once().await.unwrap().is_none(), "published once");
    assert_eq!(fake.count(Call::Publish), 1);

    let status = s.client.publication(p.id()).await.unwrap();
    assert_eq!(status.report.state, PublicationState::Published);
    let url = status
        .report
        .published
        .as_ref()
        .unwrap()
        .url
        .clone()
        .unwrap();
    assert!(url.starts_with("https://example.invalid/"));
    let all = s.client.publications().await.unwrap();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].report, status.report);

    // Its media is no longer needed once it is published.
    s.f.clock.advance(Duration::hours(7));
    s.f.runner.sweep_media().await;
    assert!(!s.f.runner.media().path(s.f.installation.id, &sha).exists());
    s.finish().await;
}

#[tokio::test]
async fn hand_overs_without_media_or_after_take_back_are_refused() {
    let Some(s) = serve().await else { return };
    let (p, handover) = support::handed_over(&s.f.account, t0() + Duration::hours(2));
    let path = s.video(b"another fake short");
    let sha = sha256_file(&path).await.unwrap();
    let put = HandoverPut {
        handover,
        publication: record(&p),
        media: MediaRef {
            sha256: sha.clone(),
            size_bytes: 18,
            properties: props(),
        },
    };
    let err = s.client.hand_over(&put).await.unwrap_err();
    assert_eq!(code(&err), Some(ErrorCode::MediaIncomplete), "{err:?}");

    s.client.upload_file(&path, &sha, &|_| {}).await.unwrap();
    s.client.hand_over(&put).await.unwrap();
    let released = s.client.release(p.id(), handover).await.unwrap();
    assert_eq!(released.report.owner, ExecutionOwner::Desktop);
    assert_eq!(released.report.state, PublicationState::Scheduled);
    let err = s.client.hand_over(&put).await.unwrap_err();
    assert_eq!(code(&err), Some(ErrorCode::Gone), "{err:?}");

    // Releasing something the runner never got is "not found", which the desktop treats as
    // safe to take back.
    let (never, h) = support::handed_over(&s.f.account, t0());
    let err = s.client.release(never.id(), h).await.unwrap_err();
    assert_eq!(code(&err), Some(ErrorCode::NotFound));
    s.finish().await;
}

#[tokio::test]
async fn taking_back_is_refused_while_the_cloud_attempt_runs() {
    let Some(s) = serve().await else { return };
    let (p, handover) = s.f.hand_over(t0()).await;
    let job = shorts_scheduler::JobQueue::claim_next(
        s.f.store(),
        "cloud:other",
        t0(),
        Duration::seconds(60),
        &[],
    )
    .await
    .unwrap()
    .unwrap();
    let err = s.client.release(p.id(), handover).await.unwrap_err();
    assert_eq!(code(&err), Some(ErrorCode::InFlight), "{err:?}");
    // Once the run is over (here: released on shutdown), it can come back.
    s.f.store()
        .modify_job(job.id(), |j| j.release("cloud:other", t0()))
        .await
        .unwrap();
    let back = s.client.release(p.id(), handover).await.unwrap();
    assert_eq!(back.report.owner, ExecutionOwner::Desktop);
    s.finish().await;
}

#[tokio::test]
async fn credentials_are_encrypted_and_rotations_are_not_overwritten() {
    let Some(s) = serve().await else { return };
    let account = s.f.account.clone();
    let statuses = s.client.accounts().await.unwrap();
    assert_eq!(statuses.len(), 1);
    assert_eq!(statuses[0].credential_revision, 1);

    // At rest, the secret isn't readable.
    let (ciphertext, wrapped): (Vec<u8>, Vec<u8>) =
        sqlx::query_as("SELECT ciphertext, wrapped_key FROM credentials WHERE account_id = $1")
            .bind(account.id.as_uuid())
            .fetch_one(s.f.store().pool())
            .await
            .unwrap();
    for haystack in [&ciphertext, &wrapped] {
        assert!(
            !haystack.windows(14).any(|w| w == b"fake-refresh-1"),
            "the refresh token must not be stored in the clear"
        );
    }

    // The same credential again: no new revision.
    let put = |refresh: &str, base: Option<u64>| AccountPut {
        account: account.clone(),
        credential: youtube_credential(refresh),
        base_revision: base,
    };
    let same = s
        .client
        .put_account(&put("fake-refresh-1", Some(1)))
        .await
        .unwrap();
    assert_eq!(same.credential_revision, 1);
    let newer = s
        .client
        .put_account(&put("fake-refresh-2", Some(1)))
        .await
        .unwrap();
    assert_eq!(newer.credential_revision, 2);
    assert_eq!(newer.updated_by, CredentialSource::Desktop);

    // The runner rotates it (as a token refresh would).
    let rotated =
        s.f.store()
            .save_rotated_credential(account.id, 2, &youtube_credential("fake-refresh-3"), t0())
            .await
            .unwrap();
    assert_eq!(rotated, Some(3));
    // A desktop that hasn't seen that is refused, then pulls it.
    let err = s
        .client
        .put_account(&put("fake-refresh-2b", Some(2)))
        .await
        .unwrap_err();
    assert_eq!(code(&err), Some(ErrorCode::CredentialConflict));
    let ClientError::Api(api) = &err else {
        panic!()
    };
    assert_eq!(api.revision, Some(3));
    let pulled = s.client.credential(account.id).await.unwrap();
    assert_eq!(pulled.revision, 3);
    assert_eq!(pulled.credential, youtube_credential("fake-refresh-3"));
    let pushed = s
        .client
        .put_account(&put("fake-refresh-4", Some(3)))
        .await
        .unwrap();
    assert_eq!(pushed.credential_revision, 4);

    // A rotation based on an old read loses to the desktop's newer credential.
    let stale =
        s.f.store()
            .save_rotated_credential(account.id, 3, &youtube_credential("fake-refresh-x"), t0())
            .await
            .unwrap();
    assert_eq!(stale, None);
    assert_eq!(
        s.f.store()
            .credential(account.id)
            .await
            .unwrap()
            .unwrap()
            .credential,
        youtube_credential("fake-refresh-4")
    );

    // Another installation can't read it.
    let (_, other_token) =
        s.f.store()
            .create_installation("Other", t0())
            .await
            .unwrap();
    let other = CloudClient::new(&s.base, &other_token).unwrap();
    let err = other.credential(account.id).await.unwrap_err();
    assert_eq!(code(&err), Some(ErrorCode::NotFound));
    let err = other.put_account(&put("x", None)).await.unwrap_err();
    assert_eq!(code(&err), Some(ErrorCode::NotFound));
    s.finish().await;
}

#[tokio::test]
async fn a_new_credential_resumes_publications_waiting_for_a_reconnect() {
    let Some(s) = serve().await else { return };
    let fake = Arc::new(FakeAdapter::new(Platform::YoutubeShorts, 1));
    fake.fail_next(
        Call::Validate,
        shorts_adapters::AdapterError::Authentication {
            message: "refresh token revoked".into(),
        },
    );
    s.f.runner.register_adapter(fake.clone());
    let (p, _) = s.f.hand_over(t0()).await;
    let worker = s.f.runner.worker(worker_config());
    worker.run_once().await.unwrap().unwrap();
    let status = s.client.publication(p.id()).await.unwrap();
    assert_eq!(status.report.state, PublicationState::NeedsAuthentication);
    assert!(worker.run_once().await.unwrap().is_none(), "paused");

    s.client
        .put_account(&AccountPut {
            account: s.f.account.clone(),
            credential: youtube_credential("fake-refresh-new"),
            base_revision: Some(1),
        })
        .await
        .unwrap();
    let status = s.client.publication(p.id()).await.unwrap();
    assert_eq!(status.report.state, PublicationState::RetryScheduled);
    worker.run_once().await.unwrap().unwrap();
    let status = s.client.publication(p.id()).await.unwrap();
    assert_eq!(status.report.state, PublicationState::Published);
    s.finish().await;
}
