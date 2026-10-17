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
    Imported { asset_id: String, duplicate: bool },
    ImportFailed { path: String, message: String },
    PublicationChanged { id: String },
    JobsChanged,
    RenderProgress { variant_id: String, fraction: f64 },
}
