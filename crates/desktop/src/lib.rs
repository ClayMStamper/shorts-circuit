//! Desktop application services behind the Tauri shell.
//!
//! [`App`] ties the crates together: it imports files into the library, curates assets, edits
//! variants, prepares publications for review, schedules them, and registers the job handlers
//! that render variants and take approved publications through the publish pipeline (manual
//! handoff for now). Everything the UI shows comes back as the serializable views in [`views`].
//!
//! Nothing here holds or exposes credentials; views never include credential references.

mod accounts;
mod app;
mod error;
mod events;
mod library;
pub mod media;
mod monitor;
mod paths;
mod pipeline;
mod preferences;
mod publications;
mod variants;
pub mod views;
pub mod watch;

pub use app::App;
pub use error::{AppError, AppResult};
pub use events::AppEvent;
pub use library::{AssetUpdate, ImportOutcome, LibraryQuery, PermissionInput, VIDEO_EXTENSIONS};
pub use media::{FfmpegBackend, MediaBackend, RenderOutcome};
pub use paths::AppPaths;
pub use preferences::{Preferences, PreferencesUpdate};
pub use publications::{EditPublication, NewPublication, ScheduleInput};
