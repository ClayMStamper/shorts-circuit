//! Domain entities, publication state machine, revisions and approvals. No I/O.
//!
//! Everything here is plain data plus the rules that govern it. Persistence (`shorts-store`),
//! media processing (`shorts-media`) and job execution (`shorts-scheduler`) build on these types,
//! and every state change goes through the methods defined here rather than by writing fields.

pub mod account;
pub mod asset;
pub mod attempt;
pub mod collection;
pub mod ids;
pub mod job;
pub mod media;
pub mod platform;
pub mod publication;
pub mod recipe;
pub mod schedule;
pub mod source;
pub mod variant;
pub mod youtube;

pub use account::{Capabilities, ConnectedAccount, MissingCapability, NextAction, PublishAccess};
pub use asset::{Asset, AssetStatus, Permission, PermissionStatus, Rating, Tag};
pub use attempt::{AttemptOutcome, PublishAttempt};
pub use collection::{
    ApprovalMode, Collection, CollectionRule, RuleFiring, RuleOutput, RuleSpec, UnattendedGrant,
    approval_mode,
};
pub use ids::{
    AccountId, AssetId, AttemptId, CollectionId, JobId, MediaFileId, PublicationId, RuleId,
    VariantId,
};
pub use job::{Job, JobKind, JobRecord, JobState, Lease, OverduePolicy, PauseReason};
pub use media::{Checksum, MediaFile, MediaProperties};
pub use platform::{MediaRequirements, Platform, ValidationIssue};
pub use publication::{
    ActiveStage, Approval, Destination, OutcomeResolution, Publication, PublicationEvent,
    PublicationRecord, PublicationRevision, PublicationState, PublishedRef, RevisionContent,
    Transition, TransitionError, UserAction,
};
pub use recipe::{
    AudioChoice, CropRect, EncodingSettings, FitMode, RECIPE_SCHEMA_VERSION, RecipeIssue,
    RenderRecipe, SubtitleMode, SubtitleTrack, TrimRange,
};
pub use schedule::{Schedule, TimeZoneName};
pub use source::{MatchScore, TikTokPost, match_score, post_title};
pub use variant::{RenderedOutput, Variant};
