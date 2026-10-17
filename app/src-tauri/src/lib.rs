//! Tauri 2 shell. It owns the window, wires the Rust core together, starts the durable worker
//! and the watch folder, and forwards core events to the webview. All work happens in
//! `shorts-desktop`; commands here only translate arguments and results. No credentials or
//! tokens ever cross into the webview.

mod commands;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use shorts_adapters::AdapterRegistry;
use shorts_desktop::watch::{WatchConfig, WatchFolder};
use shorts_desktop::{App, AppPaths, FfmpegBackend};
use shorts_media::{MediaTools, RenderCache, Renderer};
use shorts_scheduler::{SystemClock, WorkerConfig};
use tauri::{Emitter, Manager, RunEvent};
use tokio::sync::broadcast::error::RecvError;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

/// Whether FFmpeg/FFprobe were found at startup.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaStatus {
    pub available: bool,
    pub message: Option<String>,
}

/// State shared by all commands.
pub struct Shell {
    pub app: Arc<App>,
    pub media: MediaStatus,
    watch: Mutex<Option<WatchFolder>>,
    shutdown: CancellationToken,
}

impl Shell {
    /// (Re)starts the watch folder from the saved preference.
    pub async fn restart_watch(&self, handle: &tauri::AppHandle) {
        let folder = match self.app.preferences().await {
            Ok(prefs) => prefs.watch_folder,
            Err(e) => {
                warn!(error = %e, "could not read preferences");
                return;
            }
        };
        let mut slot = self.watch.lock().expect("watch lock");
        *slot = None;
        let Some(folder) = folder else { return };
        match WatchFolder::start(
            self.app.clone(),
            PathBuf::from(&folder),
            WatchConfig::default(),
        ) {
            Ok(watch) => {
                info!(%folder, "watching folder");
                *slot = Some(watch);
            }
            Err(e) => {
                warn!(%folder, error = %e, "cannot watch folder");
                let _ = handle.emit(
                    "app-event",
                    serde_json::json!({ "type": "watchFolderError", "message": e.to_string() }),
                );
            }
        }
    }
}

/// Media tools: bundled next to the executable (packaged app), `SHORTS_FFMPEG`/`SHORTS_FFPROBE`,
/// or PATH. Missing tools are reported to the UI rather than failing startup.
fn media_tools() -> (MediaTools, MediaStatus) {
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(PathBuf::from));
    match MediaTools::discover(exe_dir.as_deref()) {
        Ok(tools) => (
            tools,
            MediaStatus {
                available: true,
                message: None,
            },
        ),
        Err(e) => (
            MediaTools::new("ffmpeg", "ffprobe"),
            MediaStatus {
                available: false,
                message: Some(e.to_string()),
            },
        ),
    }
}

async fn start(data_dir: PathBuf) -> Result<Shell, Box<dyn std::error::Error>> {
    let paths = AppPaths::under(&data_dir);
    let (tools, media) = media_tools();
    let renderer = Renderer::new(tools, RenderCache::new(paths.cache.clone()), 1);
    let app = App::open(
        paths,
        Arc::new(FfmpegBackend::new(renderer)),
        // Increment 2 has no provider adapters: every account uses the export package.
        AdapterRegistry::new(),
        Arc::new(SystemClock),
    )
    .await?;
    if let Ok(tz) = iana_time_zone::get_timezone() {
        app.ensure_timezone(&tz).await?;
    }
    let shutdown = CancellationToken::new();
    app.start_worker(WorkerConfig::default(), shutdown.clone())
        .await?;
    Ok(Shell {
        app,
        media,
        watch: Mutex::new(None),
        shutdown,
    })
}

pub fn run() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,sqlx=warn".into()),
        )
        .init();

    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .setup(|app| {
            // Large originals and renders belong in the local (non-roaming) profile.
            let data_dir = app.path().app_local_data_dir()?;
            let shell = tauri::async_runtime::block_on(start(data_dir))?;
            let handle = app.handle().clone();

            let mut events = shell.app.subscribe();
            let forward = handle.clone();
            tauri::async_runtime::spawn(async move {
                loop {
                    match events.recv().await {
                        Ok(event) => {
                            let _ = forward.emit("app-event", &event);
                        }
                        Err(RecvError::Lagged(_)) => {
                            let _ = forward
                                .emit("app-event", serde_json::json!({ "type": "libraryChanged" }));
                        }
                        Err(RecvError::Closed) => break,
                    }
                }
            });

            app.manage(shell);
            let watch_handle = handle.clone();
            tauri::async_runtime::spawn(async move {
                let shell = watch_handle.state::<Shell>();
                shell.restart_watch(&watch_handle).await;
            });
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::list_assets,
            commands::asset_detail,
            commands::update_asset,
            commands::tag_counts,
            commands::thumbnail,
            commands::import_files,
            commands::save_link,
            commands::create_variant,
            commands::update_variant,
            commands::render_variant,
            commands::list_accounts,
            commands::add_account,
            commands::rename_account,
            commands::create_publication,
            commands::edit_publication,
            commands::submit_for_review,
            commands::review,
            commands::approve,
            commands::revoke_approval,
            commands::schedule,
            commands::cancel_publication,
            commands::retry_publication,
            commands::resolve_user_action,
            commands::confirm_posted,
            commands::resolve_outcome,
            commands::export_package,
            commands::next_slot,
            commands::monitor,
            commands::retry_job,
            commands::cancel_job,
            commands::preferences,
            commands::update_preferences,
            commands::media_status,
            commands::open_folder,
        ])
        .build(tauri::generate_context!())
        .expect("failed to build the Tauri application");

    app.run(|handle, event| {
        if let RunEvent::Exit = event {
            // Running jobs are released with their checkpoints and resume on next start.
            if let Some(shell) = handle.try_state::<Shell>() {
                shell.shutdown.cancel();
            }
        }
    });
}
