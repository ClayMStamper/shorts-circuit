//! Execution ownership on the runner: claiming, releasing and receiving hand-overs.

mod support;

use std::sync::Arc;

use chrono::Duration;
use shorts_adapters::AdapterError;
use shorts_adapters::fake::{Call, FakeAdapter};
use shorts_cloud::HandoverPut;
use shorts_domain::{
    ExecutionOwner, HandoverId, InFlight, JobKind, JobState, Platform, PublicationState,
    TransitionError,
};
use shorts_runner::DbError;
use shorts_scheduler::{Disposition, JobQueue};
use support::{Fixture, record, t0, worker_config};

fn lease() -> Duration {
    Duration::seconds(60)
}

#[tokio::test]
async fn a_hand_over_is_stored_once_with_a_job_at_its_time() {
    let Some(f) = Fixture::new().await else {
        return;
    };
    let at = t0() + Duration::hours(1);
    let (p, handover) = f.hand_over(at).await;
    let held = f.store().publication(p.id()).await.unwrap().unwrap();
    assert_eq!(held.publication.owner(), ExecutionOwner::Cloud);
    assert_eq!(held.publication.state(), PublicationState::Scheduled);
    let job = f
        .store()
        .latest_job(&p.id().to_string())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(job.state(), JobState::Queued);
    assert_eq!(job.run_at(), at);

    // The same hand-over again changes nothing.
    let again = f
        .store()
        .receive_handover(
            f.installation.id,
            &HandoverPut {
                handover,
                publication: record(&p),
                media: held.media.clone(),
            },
            t0(),
        )
        .await
        .unwrap();
    assert!(matches!(again, shorts_runner::db::Received::Unchanged(_)));
    assert!(
        f.store()
            .claim_next("cloud:a", t0(), lease(), &[])
            .await
            .unwrap()
            .is_none(),
        "not due yet"
    );

    // Another hand-over of the same publication while this one is live is refused.
    let mut other = record(&p);
    other.handover = Some(HandoverId::new());
    let err = f
        .store()
        .receive_handover(
            f.installation.id,
            &HandoverPut {
                handover: other.handover.unwrap(),
                publication: other,
                media: held.media.clone(),
            },
            t0(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, DbError::Refused(_)), "{err:?}");
    f.finish().await;
}

#[tokio::test]
async fn hand_overs_need_their_media_and_account_first() {
    let Some(f) = Fixture::new().await else {
        return;
    };
    let (p, handover) = support::handed_over(&f.account, t0());
    let mut media = f.upload(b"video").await;
    media.sha256 = "0".repeat(64);
    let err = f
        .store()
        .receive_handover(
            f.installation.id,
            &HandoverPut {
                handover,
                publication: record(&p),
                media,
            },
            t0(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, DbError::MediaIncomplete), "{err:?}");

    let stranger = support::automatic(Platform::YoutubeShorts, "Unknown");
    let (p, handover) = support::handed_over(&stranger, t0());
    let media = f.upload(b"video 2").await;
    let err = f
        .store()
        .receive_handover(
            f.installation.id,
            &HandoverPut {
                handover,
                publication: record(&p),
                media,
            },
            t0(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, DbError::Refused(_)), "{err:?}");

    // A desktop-owned record is refused outright.
    let mut desktop = record(&p);
    desktop.owner = ExecutionOwner::Desktop;
    desktop.handover = None;
    let err = f
        .store()
        .receive_handover(
            f.installation.id,
            &HandoverPut {
                handover,
                publication: desktop,
                media: f.upload(b"video 3").await,
            },
            t0(),
        )
        .await
        .unwrap_err();
    assert!(
        matches!(err, DbError::Transition(TransitionError::NotOwner { .. })),
        "{err:?}"
    );
    f.finish().await;
}

#[tokio::test]
async fn releasing_gives_it_back_and_the_runner_never_runs_it_again() {
    let Some(f) = Fixture::new().await else {
        return;
    };
    let (p, handover) = f.hand_over(t0()).await;
    let status = f
        .store()
        .release(f.installation.id, p.id(), handover, t0())
        .await
        .unwrap();
    assert_eq!(status.report.owner, ExecutionOwner::Desktop);
    assert_eq!(status.report.state, PublicationState::Scheduled);
    assert!(!status.report.reached_provider);
    assert!(
        f.store()
            .claim_next("cloud:a", t0() + Duration::days(1), lease(), &[])
            .await
            .unwrap()
            .is_none()
    );
    // Releasing again answers the same; re-delivering the released hand-over is gone.
    let again = f
        .store()
        .release(f.installation.id, p.id(), handover, t0())
        .await
        .unwrap();
    assert_eq!(again.report, status.report);
    let held = f.store().publication(p.id()).await.unwrap().unwrap();
    let err = f
        .store()
        .receive_handover(
            f.installation.id,
            &HandoverPut {
                handover,
                publication: record(&p),
                media: held.media.clone(),
            },
            t0(),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, DbError::HandoverGone), "{err:?}");

    // A new hand-over of the same publication is accepted.
    let mut fresh = record(&p);
    let next = HandoverId::new();
    fresh.handover = Some(next);
    f.store()
        .receive_handover(
            f.installation.id,
            &HandoverPut {
                handover: next,
                publication: fresh,
                media: held.media,
            },
            t0(),
        )
        .await
        .unwrap();
    let job = f
        .store()
        .claim_next("cloud:a", t0(), lease(), &[])
        .await
        .unwrap()
        .unwrap();
    assert_eq!(job.subject(), p.id().to_string());
    // Another installation can't release it.
    let (other, _) = f
        .store()
        .create_installation("Other PC", t0())
        .await
        .unwrap();
    assert!(matches!(
        f.store().release(other.id, p.id(), next, t0()).await,
        Err(DbError::NotFound { .. })
    ));
    f.finish().await;
}

#[tokio::test]
async fn releasing_is_refused_while_the_cloud_job_runs() {
    let Some(f) = Fixture::new().await else {
        return;
    };
    let (p, handover) = f.hand_over(t0()).await;
    let job = f
        .store()
        .claim_next("cloud:a", t0(), lease(), &[])
        .await
        .unwrap()
        .unwrap();
    let err = f
        .store()
        .release(f.installation.id, p.id(), handover, t0())
        .await
        .unwrap_err();
    assert_eq!(err.in_flight(), Some(InFlight::JobRunning));
    let held = f.store().publication(p.id()).await.unwrap().unwrap();
    assert_eq!(held.publication.owner(), ExecutionOwner::Cloud);
    let still = f.store().job(job.id()).await.unwrap().unwrap();
    assert_eq!(still.state(), JobState::Running);
    f.finish().await;
}

/// The runner's worker claiming a job and the desktop taking the publication back race;
/// exactly one wins every time.
#[tokio::test]
async fn claiming_and_releasing_never_both_win() {
    let Some(f) = Fixture::new().await else {
        return;
    };
    let (mut claims, mut releases) = (0, 0);
    for _ in 0..25 {
        let (p, handover) = f.hand_over(t0()).await;
        let id = p.id();
        let claimer = {
            let store = f.store().clone();
            tokio::spawn(async move {
                store
                    .claim_next(
                        "cloud:w",
                        t0(),
                        Duration::seconds(60),
                        &[JobKind::PublishPublication],
                    )
                    .await
                    .unwrap()
            })
        };
        let releaser = {
            let store = f.store().clone();
            let inst = f.installation.id;
            tokio::spawn(async move { store.release(inst, id, handover, t0()).await })
        };
        let claimed = claimer.await.unwrap();
        let released = releaser.await.unwrap();
        match (claimed, released) {
            (Some(job), Err(e)) if e.in_flight() == Some(InFlight::JobRunning) => {
                assert_eq!(job.subject(), p.id().to_string());
                claims += 1;
                f.store()
                    .modify_job(job.id(), |j| j.succeed("cloud:w", t0()))
                    .await
                    .unwrap();
            }
            (None, Ok(status)) => {
                assert_eq!(status.report.owner, ExecutionOwner::Desktop);
                releases += 1;
            }
            (claimed, released) => panic!("both or neither won: {claimed:?} / {released:?}"),
        }
    }
    assert_eq!(claims + releases, 25);
    f.finish().await;
}

#[tokio::test]
async fn the_cloud_worker_publishes_through_the_shared_pipeline() {
    let Some(f) = Fixture::new().await else {
        return;
    };
    let fake = Arc::new(FakeAdapter::new(Platform::YoutubeShorts, 2));
    f.runner.register_adapter(fake.clone());
    let (p, _) = f.hand_over(t0()).await;
    let worker = f.runner.worker(worker_config());
    let (_, disposition) = worker.run_once().await.unwrap().unwrap();
    assert_eq!(disposition, Disposition::Succeeded);
    let held = f.store().publication(p.id()).await.unwrap().unwrap();
    assert_eq!(held.publication.state(), PublicationState::Published);
    let url = held.publication.published().unwrap().url.clone().unwrap();
    assert!(url.starts_with("https://example.invalid/"));
    assert_eq!(fake.count(Call::Publish), 1);
    let events: Vec<String> = f
        .store()
        .events(p.id())
        .await
        .unwrap()
        .into_iter()
        .map(|(e, _, _)| e)
        .collect();
    assert_eq!(
        events,
        [
            "received",
            "begin_preparing",
            "advance",
            "advance",
            "advance",
            "published"
        ]
    );
    let status = f
        .store()
        .status(f.installation.id, p.id())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status.report.state, PublicationState::Published);
    assert!(status.report.reached_provider);
    assert!(worker.run_once().await.unwrap().is_none());
    f.finish().await;
}

#[tokio::test]
async fn a_lost_publish_response_becomes_outcome_unknown_and_can_come_back() {
    let Some(f) = Fixture::new().await else {
        return;
    };
    let fake = Arc::new(FakeAdapter::new(Platform::YoutubeShorts, 1));
    *fake.lose_publish_response.lock().unwrap() = true;
    f.runner.register_adapter(fake.clone());
    let (p, handover) = f.hand_over(t0()).await;
    // The fake can't find the post again, so the outcome stays unknown.
    fake.fail_next(
        Call::Reconcile,
        AdapterError::Unsupported {
            message: "no lookup".into(),
        },
    );
    let worker = f.runner.worker(worker_config());
    let (_, disposition) = worker.run_once().await.unwrap().unwrap();
    assert!(
        matches!(disposition, Disposition::Paused { .. }),
        "{disposition:?}"
    );
    let status = f
        .store()
        .status(f.installation.id, p.id())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status.report.state, PublicationState::OutcomeUnknown);
    assert_eq!(fake.count(Call::Publish), 1, "never re-sent");
    assert!(worker.run_once().await.unwrap().is_none());

    // The user resolves it on the desktop, so it can be taken back.
    let released = f
        .store()
        .release(f.installation.id, p.id(), handover, t0())
        .await
        .unwrap();
    assert_eq!(released.report.state, PublicationState::OutcomeUnknown);
    assert_eq!(released.report.owner, ExecutionOwner::Desktop);
    f.finish().await;
}

#[tokio::test]
async fn an_upload_waiting_to_retry_stays_on_the_runner() {
    let Some(f) = Fixture::new().await else {
        return;
    };
    let fake = Arc::new(FakeAdapter::new(Platform::YoutubeShorts, 3));
    fake.fail_next(
        Call::UploadStep,
        AdapterError::Transient {
            message: "503".into(),
            retry_after: None,
        },
    );
    f.runner.register_adapter(fake.clone());
    let (p, handover) = f.hand_over(t0()).await;
    let worker = f.runner.worker(worker_config());
    let (_, disposition) = worker.run_once().await.unwrap().unwrap();
    assert!(
        matches!(disposition, Disposition::RetryAt { .. }),
        "{disposition:?}"
    );
    let status = f
        .store()
        .status(f.installation.id, p.id())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status.report.state, PublicationState::RetryScheduled);
    assert!(status.report.reached_provider);
    let err = f
        .store()
        .release(f.installation.id, p.id(), handover, t0())
        .await
        .unwrap_err();
    assert_eq!(err.in_flight(), Some(InFlight::ReachedProvider));

    // It resumes the same upload session later.
    f.clock.advance(Duration::minutes(10));
    let (_, disposition) = worker.run_once().await.unwrap().unwrap();
    assert_eq!(disposition, Disposition::Succeeded);
    assert_eq!(fake.count(Call::BeginUpload), 1);
    f.finish().await;
}

#[tokio::test]
async fn revisions_are_immutable() {
    let Some(f) = Fixture::new().await else {
        return;
    };
    let (p, _) = f.hand_over(t0()).await;
    let err =
        sqlx::query("UPDATE publication_revisions SET content = '{}' WHERE publication_id = $1")
            .bind(p.id().as_uuid())
            .execute(f.store().pool())
            .await
            .unwrap_err();
    assert!(err.to_string().contains("immutable"), "{err}");
    f.finish().await;
}

/// While a release holds the job row, a claim skips it instead of waiting or taking it.
#[tokio::test]
async fn a_claim_skips_a_job_a_release_is_holding() {
    let Some(f) = Fixture::new().await else {
        return;
    };
    let (p, _) = f.hand_over(t0()).await;
    let mut held = f.store().pool().begin().await.unwrap();
    sqlx::query("SELECT id FROM jobs WHERE subject = $1 FOR UPDATE")
        .bind(p.id().to_string())
        .execute(&mut *held)
        .await
        .unwrap();
    assert!(
        f.store()
            .claim_next("cloud:a", t0(), lease(), &[])
            .await
            .unwrap()
            .is_none()
    );
    held.rollback().await.unwrap();
    assert!(
        f.store()
            .claim_next("cloud:a", t0(), lease(), &[])
            .await
            .unwrap()
            .is_some()
    );
    f.finish().await;
}
