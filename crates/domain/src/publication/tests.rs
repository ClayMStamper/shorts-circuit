use chrono::Duration;

use super::*;
use crate::schedule::TimeZoneName;

fn t0() -> DateTime<Utc> {
    DateTime::parse_from_rfc3339("2026-11-01T12:00:00Z")
        .unwrap()
        .with_timezone(&Utc)
}

fn destination() -> Destination {
    Destination {
        account_id: AccountId::new(),
        platform: Platform::YoutubeShorts,
    }
}

fn content(variant: VariantId, caption: &str) -> RevisionContent {
    RevisionContent::new(variant, 1, caption, &["shorts".into()], destination()).unwrap()
}

fn schedule() -> Schedule {
    Schedule::new(
        t0() + Duration::hours(2),
        TimeZoneName::parse("America/New_York").unwrap(),
    )
}

fn draft() -> Publication {
    Publication::draft(
        AssetId::new(),
        content(VariantId::new(), "First caption"),
        Some(schedule()),
        t0(),
    )
}

fn approve_event(p: &Publication) -> PublicationEvent {
    PublicationEvent::Approve {
        revision: p.revision().number,
        current_variant_revision: p.content().variant_revision,
        by: "owner".into(),
    }
}

/// A publication in `state` with a valid approval wherever the state implies one.
fn in_state(state: PublicationState) -> Publication {
    let mut record = draft().record().clone();
    record.state = state;
    if !matches!(
        state,
        PublicationState::Draft | PublicationState::AwaitingApproval
    ) {
        record.approval = Some(Approval {
            revision: 1,
            approved_at: t0(),
            approved_by: "owner".into(),
            schedule_at_approval: schedule(),
        });
    }
    if state == PublicationState::NeedsUserAction {
        record.user_action = Some(UserAction::Other {
            message: "check".into(),
        });
    }
    if state == PublicationState::RetryScheduled {
        record.retry_at = Some(t0());
    }
    Publication::from_record(record)
}

fn sample_events(variant: VariantId) -> Vec<PublicationEvent> {
    use PublicationEvent as E;
    vec![
        E::SubmitForReview,
        E::Approve {
            revision: 1,
            current_variant_revision: 1,
            by: "owner".into(),
        },
        E::RevokeApproval,
        E::Revise {
            content: RevisionContent {
                destination: Destination {
                    platform: Platform::YoutubeShorts,
                    account_id: AccountId::new(),
                },
                ..content(variant, "Edited caption")
            },
        },
        E::Reschedule {
            schedule: schedule(),
        },
        E::Reauthenticated,
        E::ResolveUserAction,
        E::ResolveOutcome {
            resolution: OutcomeResolution::NotPublished,
        },
        E::Retry,
        E::Cancel,
        E::BeginPreparing,
        E::Advance {
            to: ActiveStage::Uploading,
        },
        E::Resume {
            at: ActiveStage::Uploading,
        },
        E::HandoffReady {
            package_dir: "out".into(),
        },
        E::Published {
            url: None,
            remote_id: None,
        },
        E::RetryScheduled {
            at: t0(),
            reason: "503".into(),
        },
        E::AuthenticationRequired {
            message: "expired".into(),
        },
        E::UserActionRequired {
            action: UserAction::Other {
                message: "x".into(),
            },
        },
        E::ScheduleExpired,
        E::OutcomeUnknown {
            message: "timeout".into(),
        },
        E::Failed {
            message: "boom".into(),
        },
    ]
}

/// The transition table, written out independently of the implementation.
fn expected_allowed(state: PublicationState, event: &str) -> bool {
    use PublicationState::*;
    let active = matches!(state, Preparing | Uploading | Processing | Publishing);
    match event {
        "submit_for_review" => state == Draft,
        "approve" => state == AwaitingApproval,
        "revoke_approval" => matches!(
            state,
            Scheduled | RetryScheduled | NeedsAuthentication | NeedsUserAction | Failed
        ),
        "revise" => matches!(
            state,
            Draft
                | AwaitingApproval
                | Scheduled
                | RetryScheduled
                | NeedsAuthentication
                | NeedsUserAction
                | Failed
        ),
        "reschedule" => matches!(state, Draft | AwaitingApproval | Scheduled),
        "reauthenticated" => state == NeedsAuthentication,
        "resolve_user_action" => state == NeedsUserAction,
        "resolve_outcome" => state == OutcomeUnknown,
        "retry" => state == Failed,
        "cancel" => !matches!(state, Published | Cancelled | Publishing | OutcomeUnknown),
        "begin_preparing" => state == Scheduled,
        "advance" => state == Preparing,
        "resume" => state == RetryScheduled,
        "handoff_ready" => state == Preparing,
        "published" => state == Publishing,
        "retry_scheduled" => active,
        "authentication_required" | "user_action_required" => {
            active || matches!(state, RetryScheduled | Scheduled)
        }
        "schedule_expired" => matches!(state, Scheduled | RetryScheduled),
        "outcome_unknown" => state == Publishing,
        "failed" => active || state == RetryScheduled,
        other => panic!("no expectation for {other}"),
    }
}

#[test]
fn transition_table_matches_specification() {
    for state in PublicationState::ALL {
        let base = in_state(state);
        for event in sample_events(base.content().variant_id) {
            let name = event.name();
            let mut p = base.clone();
            let result = p.apply(event, t0());
            assert_eq!(
                result.is_ok(),
                expected_allowed(state, name),
                "{name} from {state}: {result:?}"
            );
            if result.is_err() {
                assert_eq!(p, base, "a rejected {name} from {state} must not modify it");
            }
        }
    }
}

#[test]
fn terminal_states_accept_nothing() {
    for state in [PublicationState::Published, PublicationState::Cancelled] {
        assert!(state.is_terminal());
        let base = in_state(state);
        for event in sample_events(base.content().variant_id) {
            assert!(base.clone().apply(event, t0()).is_err());
        }
    }
}

#[test]
fn happy_path_reaches_published() {
    use PublicationEvent as E;
    let mut p = draft();
    p.apply(E::SubmitForReview, t0()).unwrap();
    p.apply(approve_event(&p), t0()).unwrap();
    assert_eq!(p.state(), PublicationState::Scheduled);
    assert!(p.has_valid_approval());
    assert_eq!(p.due_at(), Some(schedule().at));
    p.apply(E::BeginPreparing, t0()).unwrap();
    for stage in [
        ActiveStage::Uploading,
        ActiveStage::Processing,
        ActiveStage::Publishing,
    ] {
        p.apply(E::Advance { to: stage }, t0()).unwrap();
        assert_eq!(p.state(), stage.state());
    }
    p.apply(
        E::Published {
            url: Some("https://example.com/p/1".into()),
            remote_id: Some("r1".into()),
        },
        t0(),
    )
    .unwrap();
    assert_eq!(p.state(), PublicationState::Published);
    assert_eq!(p.published().unwrap().remote_id.as_deref(), Some("r1"));
    assert_eq!(p.version(), 7);
}

#[test]
fn stages_cannot_be_skipped() {
    let mut p = in_state(PublicationState::Preparing);
    let err = p
        .apply(
            PublicationEvent::Advance {
                to: ActiveStage::Publishing,
            },
            t0(),
        )
        .unwrap_err();
    assert!(matches!(err, TransitionError::NotAllowed { .. }));
}

#[test]
fn editing_creates_a_new_revision_and_drops_approval() {
    let mut p = draft();
    p.apply(PublicationEvent::SubmitForReview, t0()).unwrap();
    p.apply(approve_event(&p), t0()).unwrap();
    let original = p.revision().clone();

    let edited = RevisionContent {
        caption: "Better caption".into(),
        ..p.content().clone()
    };
    let transition = p
        .apply(PublicationEvent::Revise { content: edited }, t0())
        .unwrap();

    assert!(transition.new_revision);
    assert!(transition.approval_dropped);
    assert_eq!(p.state(), PublicationState::AwaitingApproval);
    assert_eq!(p.revision().number, 2);
    assert!(p.approval().is_none());
    assert!(!p.has_valid_approval());
    // The old revision is untouched.
    assert_eq!(original.number, 1);
    assert_eq!(original.content.caption, "First caption");
}

#[test]
fn approval_of_an_old_revision_is_refused() {
    let mut p = draft();
    p.apply(
        PublicationEvent::Revise {
            content: RevisionContent {
                caption: "v2".into(),
                ..p.content().clone()
            },
        },
        t0(),
    )
    .unwrap();
    p.apply(PublicationEvent::SubmitForReview, t0()).unwrap();
    let err = p
        .apply(
            PublicationEvent::Approve {
                revision: 1,
                current_variant_revision: 1,
                by: "owner".into(),
            },
            t0(),
        )
        .unwrap_err();
    assert_eq!(
        err,
        TransitionError::StaleRevision {
            requested: 1,
            current: 2
        }
    );
}

type Edit = Box<dyn Fn(&RevisionContent) -> RevisionContent>;

#[test]
fn every_kind_of_edit_drops_approval() {
    let edits: Vec<Edit> = vec![
        Box::new(|c| RevisionContent {
            variant_id: VariantId::new(),
            ..c.clone()
        }),
        Box::new(|c| RevisionContent {
            variant_revision: c.variant_revision + 1,
            ..c.clone()
        }),
        Box::new(|c| RevisionContent {
            caption: format!("{} more", c.caption),
            ..c.clone()
        }),
        Box::new(|c| RevisionContent {
            hashtags: vec!["other".into()],
            ..c.clone()
        }),
        Box::new(|c| RevisionContent {
            destination: Destination {
                account_id: AccountId::new(),
                ..c.destination
            },
            ..c.clone()
        }),
    ];
    for edit in edits {
        let mut p = in_state(PublicationState::Scheduled);
        let content = edit(p.content());
        p.apply(PublicationEvent::Revise { content }, t0()).unwrap();
        assert_eq!(p.state(), PublicationState::AwaitingApproval);
        assert!(p.approval().is_none());
    }
}

#[test]
fn unchanged_content_keeps_approval() {
    let mut p = in_state(PublicationState::Scheduled);
    let same = RevisionContent {
        caption: format!("  {}  ", p.content().caption),
        hashtags: vec!["#shorts".into()],
        ..p.content().clone()
    };
    let err = p
        .apply(PublicationEvent::Revise { content: same }, t0())
        .unwrap_err();
    assert_eq!(err, TransitionError::Unchanged);
    assert!(p.has_valid_approval());
}

#[test]
fn approving_stale_media_is_refused() {
    let mut p = draft();
    p.apply(PublicationEvent::SubmitForReview, t0()).unwrap();
    let err = p
        .apply(
            PublicationEvent::Approve {
                revision: 1,
                current_variant_revision: 2,
                by: "owner".into(),
            },
            t0(),
        )
        .unwrap_err();
    assert_eq!(
        err,
        TransitionError::StaleMedia {
            reviewed: 1,
            current: 2
        }
    );
}

#[test]
fn approval_requires_a_schedule() {
    let mut p = Publication::draft(AssetId::new(), content(VariantId::new(), "c"), None, t0());
    p.apply(PublicationEvent::SubmitForReview, t0()).unwrap();
    assert_eq!(
        p.apply(approve_event(&p), t0()).unwrap_err(),
        TransitionError::MissingSchedule
    );
}

#[test]
fn destination_platform_cannot_change() {
    let mut p = draft();
    let content = RevisionContent {
        destination: Destination {
            account_id: AccountId::new(),
            platform: Platform::Linkedin,
        },
        ..p.content().clone()
    };
    assert_eq!(
        p.apply(PublicationEvent::Revise { content }, t0())
            .unwrap_err(),
        TransitionError::PlatformMismatch {
            expected: Platform::YoutubeShorts,
            found: Platform::Linkedin
        }
    );
}

#[test]
fn rescheduling_keeps_approval() {
    let mut p = in_state(PublicationState::Scheduled);
    let later = Schedule::new(t0() + Duration::days(1), TimeZoneName::utc());
    p.apply(PublicationEvent::Reschedule { schedule: later }, t0())
        .unwrap();
    assert!(p.has_valid_approval());
    assert_eq!(p.due_at(), Some(later.at));
}

#[test]
fn expired_schedule_asks_the_user_and_resumes_on_reschedule() {
    let mut p = in_state(PublicationState::Scheduled);
    p.apply(PublicationEvent::ScheduleExpired, t0()).unwrap();
    assert_eq!(p.state(), PublicationState::NeedsUserAction);
    assert_eq!(
        p.user_action(),
        Some(&UserAction::ScheduleExpired {
            scheduled_for: schedule().at
        })
    );
    // "Resolve" alone is not enough: a new time is needed.
    assert!(
        p.clone()
            .apply(PublicationEvent::ResolveUserAction, t0())
            .is_err()
    );
    let now = Schedule::new(t0(), TimeZoneName::utc());
    p.apply(PublicationEvent::Reschedule { schedule: now }, t0())
        .unwrap();
    assert_eq!(p.state(), PublicationState::Scheduled);
    assert!(p.user_action().is_none());
}

#[test]
fn transient_failure_retries_from_the_checkpointed_stage() {
    let mut p = in_state(PublicationState::Uploading);
    let at = t0() + Duration::minutes(5);
    p.apply(
        PublicationEvent::RetryScheduled {
            at,
            reason: "connection reset".into(),
        },
        t0(),
    )
    .unwrap();
    assert_eq!(p.due_at(), Some(at));
    assert_eq!(p.last_error(), Some("connection reset"));
    p.apply(
        PublicationEvent::Resume {
            at: ActiveStage::Uploading,
        },
        at,
    )
    .unwrap();
    assert_eq!(p.state(), PublicationState::Uploading);
    assert_eq!(p.retry_at(), None);
}

#[test]
fn lost_publish_response_needs_resolution_not_retry() {
    let mut p = in_state(PublicationState::Publishing);
    p.apply(
        PublicationEvent::OutcomeUnknown {
            message: "response lost".into(),
        },
        t0(),
    )
    .unwrap();
    assert_eq!(p.state(), PublicationState::OutcomeUnknown);
    assert_eq!(p.due_at(), None, "never picked up again automatically");
    assert!(p.clone().apply(PublicationEvent::Retry, t0()).is_err());
    assert!(p.clone().apply(PublicationEvent::Cancel, t0()).is_err());

    let mut found = p.clone();
    found
        .apply(
            PublicationEvent::ResolveOutcome {
                resolution: OutcomeResolution::Published {
                    url: Some("https://example.com/p/2".into()),
                    remote_id: None,
                },
            },
            t0(),
        )
        .unwrap();
    assert_eq!(found.state(), PublicationState::Published);

    p.apply(
        PublicationEvent::ResolveOutcome {
            resolution: OutcomeResolution::NotPublished,
        },
        t0(),
    )
    .unwrap();
    assert_eq!(p.state(), PublicationState::RetryScheduled);
}

#[test]
fn outcome_unknown_only_follows_publishing() {
    for state in [PublicationState::Uploading, PublicationState::Processing] {
        let mut p = in_state(state);
        assert!(
            p.apply(
                PublicationEvent::OutcomeUnknown {
                    message: "?".into()
                },
                t0()
            )
            .is_err()
        );
    }
}

#[test]
fn manual_handoff_flow() {
    let mut p = in_state(PublicationState::Preparing);
    p.apply(
        PublicationEvent::HandoffReady {
            package_dir: "/exports/p1".into(),
        },
        t0(),
    )
    .unwrap();
    assert_eq!(p.state(), PublicationState::NeedsUserAction);
    assert!(
        p.clone()
            .apply(PublicationEvent::ResolveUserAction, t0())
            .is_err()
    );
    p.apply(
        PublicationEvent::ConfirmManualPost {
            url: Some("https://example.com/p/3".into()),
        },
        t0(),
    )
    .unwrap();
    assert_eq!(p.state(), PublicationState::Published);
    assert!(p.published().unwrap().manual);
}

#[test]
fn manual_post_confirmation_requires_a_handoff() {
    let mut p = in_state(PublicationState::NeedsUserAction);
    assert!(
        p.apply(PublicationEvent::ConfirmManualPost { url: None }, t0())
            .is_err()
    );
}

#[test]
fn authentication_pause_and_recovery() {
    let mut p = in_state(PublicationState::Uploading);
    p.apply(
        PublicationEvent::AuthenticationRequired {
            message: "token expired".into(),
        },
        t0(),
    )
    .unwrap();
    assert_eq!(p.state(), PublicationState::NeedsAuthentication);
    assert_eq!(p.due_at(), None);
    p.apply(PublicationEvent::Reauthenticated, t0()).unwrap();
    assert_eq!(p.state(), PublicationState::RetryScheduled);
    assert_eq!(p.due_at(), Some(t0()));
}

#[test]
fn failed_publications_can_be_retried_while_approved() {
    let mut p = in_state(PublicationState::Failed);
    p.apply(PublicationEvent::Retry, t0()).unwrap();
    assert_eq!(p.state(), PublicationState::RetryScheduled);

    let mut unapproved = in_state(PublicationState::Failed);
    unapproved
        .apply(PublicationEvent::RevokeApproval, t0())
        .unwrap();
    assert_eq!(unapproved.state(), PublicationState::AwaitingApproval);
}

#[test]
fn workers_cannot_start_unapproved_work() {
    let mut record = in_state(PublicationState::Scheduled).record().clone();
    record.approval.as_mut().unwrap().revision = 0;
    let mut p = Publication::from_record(record);
    assert_eq!(
        p.apply(PublicationEvent::BeginPreparing, t0()).unwrap_err(),
        TransitionError::NotApproved
    );
}

#[test]
fn hashtags_are_normalized() {
    let c = RevisionContent::new(
        VariantId::new(),
        1,
        "  hi  ",
        &["#Food".into(), "food".into(), " ".into(), "#recipes".into()],
        destination(),
    )
    .unwrap();
    assert_eq!(c.caption, "hi");
    assert_eq!(c.hashtags, vec!["Food".to_owned(), "recipes".to_owned()]);
    assert!(
        RevisionContent::new(
            VariantId::new(),
            1,
            "",
            &["two words".into()],
            destination()
        )
        .is_err()
    );
}

#[test]
fn transitions_record_from_and_to() {
    let mut p = draft();
    let t = p.apply(PublicationEvent::SubmitForReview, t0()).unwrap();
    assert_eq!(t.from, PublicationState::Draft);
    assert_eq!(t.to, PublicationState::AwaitingApproval);
    assert_eq!(t.event, "submit_for_review");
    assert_eq!(t.revision, 1);
    assert!(!t.new_revision);
}

#[test]
fn state_names_round_trip() {
    for state in PublicationState::ALL {
        assert_eq!(PublicationState::parse(state.as_str()), Some(state));
        let json = serde_json::to_string(&state).unwrap();
        assert_eq!(json, format!("\"{}\"", state.as_str()));
    }
}
