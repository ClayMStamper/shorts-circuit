use serde::Serialize;

/// Notifications for the UI, forwarded by the Tauri shell as `app-event`.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum AppEvent {
    LibraryChanged,
    Imported {
        asset_id: String,
        duplicate: bool,
    },
    ImportFailed {
        path: String,
        message: String,
    },
    PublicationChanged {
        id: String,
    },
    JobsChanged,
    RenderProgress {
        variant_id: String,
        fraction: f64,
    },
    CollectionsChanged,
    /// Collection rules could not run for an asset (the import itself succeeded).
    RulesFailed {
        asset_id: String,
        message: String,
    },
    /// The TikTok connector's status changed.
    TikTokChanged,
    /// A YouTube channel was connected, disconnected or changed, or a sign-in progressed.
    YouTubeChanged,
    /// An Instagram account was connected, disconnected or renewed.
    InstagramChanged,
    /// A LinkedIn page was connected or disconnected.
    LinkedInChanged,
    /// A Snapchat profile was connected or disconnected, or a sign-in progressed.
    SnapchatChanged,
    /// Cloud mode was configured, or a sync with the cloud runner finished.
    CloudChanged,
    /// The watch folder was listed (on start, on its timer, or "Check now").
    WatchFolderChecked,
    /// A file might be the original of a pending TikTok post (`asset_id` is the file's asset
    /// when one import triggered it).
    MatchSuggested {
        asset_id: Option<String>,
    },
}
