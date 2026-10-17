//! Serializable views for the UI (camelCase JSON). They carry no credentials.

use chrono::{DateTime, Utc};
use serde::Serialize;
use shorts_domain::{
    ConnectedAccount, MediaProperties, NextAction, Permission, Platform, PublicationState,
    PublishAccess, RenderRecipe, ValidationIssue,
};

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetSummary {
    pub id: String,
    pub title: String,
    pub creator: Option<String>,
    pub source_url: Option<String>,
    pub notes: String,
    pub tags: Vec<String>,
    pub rating: Option<u8>,
    pub status: &'static str,
    pub video_permission: Permission,
    pub audio_permission: Permission,
    pub created_at: DateTime<Utc>,
    pub media: Option<MediaView>,
    pub publication_count: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaView {
    pub id: String,
    pub path: String,
    pub original_name: String,
    pub checksum: String,
    pub size_bytes: u64,
    pub properties: MediaProperties,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AssetDetail {
    pub asset: AssetSummary,
    pub variants: Vec<VariantView>,
    pub publications: Vec<PublicationView>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VariantView {
    pub id: String,
    pub asset_id: String,
    pub name: String,
    pub revision: u32,
    pub recipe: RenderRecipe,
    pub rendered: Option<RenderedView>,
    /// A render job is queued or running.
    pub rendering: bool,
    pub output_duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RenderedView {
    pub path: String,
    pub size_bytes: u64,
    pub properties: MediaProperties,
    pub rendered_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AccountView {
    pub id: String,
    pub platform: Platform,
    pub platform_name: &'static str,
    pub display_name: String,
    pub automatic: bool,
    /// Why automatic publishing is unavailable, and what would fix it.
    pub missing_reason: Option<String>,
    pub next_action: Option<NextAction>,
    pub manual_handoff: bool,
}

impl From<&ConnectedAccount> for AccountView {
    fn from(a: &ConnectedAccount) -> Self {
        let (automatic, missing_reason, next_action) = match &a.capabilities.publish_access {
            PublishAccess::Automatic => (true, None, None),
            PublishAccess::ManualOnly { missing } => (
                false,
                Some(missing.reason.clone()),
                Some(missing.next_action),
            ),
        };
        Self {
            id: a.id.to_string(),
            platform: a.platform,
            platform_name: a.platform.display_name(),
            display_name: a.display_name.clone(),
            automatic,
            missing_reason,
            next_action,
            manual_handoff: a.capabilities.manual_handoff,
        }
    }
}

/// Buttons the UI may offer for a publication in its current state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Action {
    Edit,
    SubmitForReview,
    Review,
    Approve,
    RevokeApproval,
    Reschedule,
    PublishNow,
    Cancel,
    Retry,
    ResolveUserAction,
    ConfirmPosted,
    ResolveOutcome,
    ExportPackage,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PublicationView {
    pub id: String,
    pub asset_id: String,
    pub asset_title: String,
    pub platform: Platform,
    pub platform_name: &'static str,
    pub account_id: String,
    pub account_name: String,
    pub state: PublicationState,
    pub revision: u32,
    pub variant_id: String,
    pub variant_name: String,
    pub caption: String,
    pub hashtags: Vec<String>,
    pub scheduled_at: Option<DateTime<Utc>>,
    pub timezone: Option<String>,
    /// The posting time in the user's timezone, e.g. `2026-11-01 09:00 EST`.
    pub scheduled_local: Option<String>,
    pub approved_revision: Option<u32>,
    pub approved: bool,
    pub retry_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub package_dir: Option<String>,
    pub published_url: Option<String>,
    /// What happens next or what the user needs to do, in plain words.
    pub status_text: String,
    pub actions: Vec<Action>,
    pub updated_at: DateTime<Utc>,
}

/// Everything needed to approve: video, destination, copy and time together.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ReviewView {
    pub publication: PublicationView,
    pub account: AccountView,
    pub variant: VariantView,
    /// The rendered file to preview; `None` until the variant is rendered.
    pub video_path: Option<String>,
    pub full_caption: String,
    pub issues: Vec<ValidationIssue>,
    pub issue_text: Vec<String>,
    pub warnings: Vec<String>,
    pub can_approve: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct JobView {
    pub id: String,
    pub kind: &'static str,
    pub subject: String,
    pub state: &'static str,
    pub run_at: DateTime<Utc>,
    pub claims: u32,
    pub failures: u32,
    pub last_error: Option<String>,
    pub pause_reason: Option<&'static str>,
    /// For finished export jobs: where the package was written.
    pub package_dir: Option<String>,
    pub updated_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MonitorView {
    pub publications: Vec<PublicationView>,
    pub jobs: Vec<JobView>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TagCount {
    pub tag: String,
    pub count: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_views_never_carry_credentials() {
        let mut account = ConnectedAccount::manual(Platform::Linkedin, "Page", Utc::now());
        account.credential_ref = Some("os-keyring:linkedin:example".into());
        let json = serde_json::to_string(&AccountView::from(&account)).unwrap();
        assert!(!json.contains("os-keyring"));
        assert!(!json.to_lowercase().contains("credential"));
        assert!(json.contains("\"nextAction\":\"export_package\""));
    }
}
