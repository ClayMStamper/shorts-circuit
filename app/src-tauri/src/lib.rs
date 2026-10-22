//! Tauri 2 shell. It owns the window, wires the Rust core together, starts the durable worker
//! and the watch folder, and forwards core events to the webview. All work happens in
//! `shorts-desktop`; commands here only translate arguments and results. No credentials or
//! tokens ever cross into the webview.

mod cloud;
mod commands;
mod keyring_store;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use shorts_adapters::AdapterRegistry;
use shorts_desktop::cloud::CloudSync;
use shorts_desktop::instagram::{InstagramConnector, InstagramSetup};
use shorts_desktop::linkedin::{LinkedInConnector, LinkedInSetup};
use shorts_desktop::snapchat::{SnapSetup, SnapchatConnector};
use shorts_desktop::tiktok::{TikTokIntake, TikTokSetup};
use shorts_desktop::watch::{WatchConfig, WatchFolder, WatchKind};
use shorts_desktop::youtube::{YouTubeConnector, YouTubeSetup};
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
    /// Optional TikTok new-post detection; unavailable unless `SHORTS_BROKER_URL` is set.
    pub tiktok: Arc<TikTokIntake>,
    /// YouTube Shorts publishing; channels are connected in Settings.
    pub youtube: Arc<YouTubeConnector>,
    /// Instagram Reels publishing; accounts are connected through their Facebook Page with a
    /// pasted token.
    pub instagram: Arc<InstagramConnector>,
    /// LinkedIn Company Page posts; pages are connected in Settings with a pasted token.
    pub linkedin: Arc<LinkedInConnector>,
    /// Snapchat Spotlight posts; profiles are connected in Settings with the owner's Snap OAuth
    /// app (loopback sign-in or a pasted code).
    pub snapchat: Arc<SnapchatConnector>,
    /// Optional cloud runner; off until its address and token are saved in Settings.
    pub cloud: Arc<CloudSync>,
    pub(crate) watch: Mutex<Option<WatchFolder>>,
    /// The downloads folder, watched only for originals of pending TikTok posts.
    pub(crate) downloads_watch: Mutex<Option<WatchFolder>>,
    shutdown: CancellationToken,
}

impl Shell {
    /// (Re)starts the watch folder from the saved preference.
    pub async fn restart_watch(&self, handle: &tauri::AppHandle) {
        let (folder, downloads) = match self.app.preferences().await {
            Ok(prefs) => (prefs.watch_folder, prefs.downloads_folder),
            Err(e) => {
                warn!(error = %e, "could not read preferences");
                return;
            }
        };
        {
            let mut slot = self.downloads_watch.lock().expect("downloads watch lock");
            *slot = None;
            // The same folder as the watch folder needs no second watch.
            if let Some(dir) = downloads.filter(|d| Some(d) != folder.as_ref()) {
                match WatchFolder::start(
                    self.app.clone(),
                    PathBuf::from(&dir),
                    WatchConfig {
                        kind: WatchKind::Downloads,
                        ..WatchConfig::default()
                    },
                ) {
                    Ok(watch) => {
                        info!(folder = %dir, "watching downloads for TikTok originals");
                        *slot = Some(watch);
                    }
                    Err(e) => warn!(folder = %dir, error = %e, "cannot watch the downloads folder"),
                }
            }
        }
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
        // Connectors register their adapters below; accounts without one use export packages.
        AdapterRegistry::new(),
        Arc::new(SystemClock),
    )
    .await?;
    if let Ok(tz) = iana_time_zone::get_timezone() {
        app.ensure_timezone(&tz).await?;
    }
    let youtube = YouTubeConnector::new(
        app.clone(),
        YouTubeSetup::from_env(),
        Arc::new(keyring_store::KeyringCredentials),
    );
    let instagram = InstagramConnector::new(
        app.clone(),
        InstagramSetup::from_env(),
        Arc::new(keyring_store::KeyringCredentials),
    );
    let linkedin = LinkedInConnector::new(
        app.clone(),
        LinkedInSetup::from_env(),
        Arc::new(keyring_store::KeyringCredentials),
    );
    let snapchat = SnapchatConnector::new(
        app.clone(),
        SnapSetup::from_env(),
        Arc::new(keyring_store::KeyringCredentials),
    );
    let shutdown = CancellationToken::new();
    app.start_worker(WorkerConfig::default(), shutdown.clone())
        .await?;
    let tiktok = TikTokIntake::new(
        app.clone(),
        Some(TikTokSetup::from_env()),
        Arc::new(keyring_store::KeyringCredentials),
    );
    tauri::async_runtime::spawn(tiktok.clone().run(shutdown.clone()));
    tauri::async_runtime::spawn(instagram.clone().run(shutdown.clone()));
    let cloud = CloudSync::new(app.clone(), Arc::new(keyring_store::KeyringCredentials));
    tauri::async_runtime::spawn(cloud.clone().run(shutdown.clone()));
    Ok(Shell {
        app,
        media,
        tiktok,
        youtube,
        instagram,
        linkedin,
        snapchat,
        cloud,
        watch: Mutex::new(None),
        downloads_watch: Mutex::new(None),
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
            commands::list_collections,
            commands::create_collection,
            commands::rename_collection,
            commands::delete_collection,
            commands::add_to_collection,
            commands::remove_from_collection,
            commands::create_rule,
            commands::update_rule,
            commands::delete_rule,
            commands::set_unattended,
            commands::rule_activity,
            commands::check_watch_folder,
            commands::get_original,
            commands::cancel_get_original,
            commands::send_to_workflows,
            commands::attach_original,
            commands::default_downloads_folder,
            commands::list_workflows,
            commands::workflow_detail,
            commands::create_workflow,
            commands::update_workflow,
            commands::delete_workflow,
            commands::activity,
            commands::publish_now,
            commands::republish,
            commands::check_for_new_content,
            commands::tiktok_app_info,
            commands::tiktok_set_app,
            commands::tiktok_clear_app,
            commands::tiktok_status,
            commands::tiktok_set_enabled,
            commands::tiktok_connect,
            commands::tiktok_finish_connect,
            commands::tiktok_disconnect,
            commands::tiktok_check_now,
            commands::youtube_status,
            commands::youtube_set_client,
            commands::youtube_clear_client,
            commands::youtube_connect,
            commands::youtube_connect_progress,
            commands::youtube_cancel_connect,
            commands::youtube_disconnect,
            commands::youtube_set_public_uploads,
            commands::instagram_status,
            commands::instagram_find_pages,
            commands::instagram_connect,
            commands::instagram_disconnect,
            commands::linkedin_status,
            commands::linkedin_find_pages,
            commands::linkedin_connect,
            commands::linkedin_disconnect,
            commands::snapchat_status,
            commands::snapchat_set_client,
            commands::snapchat_clear_client,
            commands::snapchat_connect,
            commands::snapchat_connect_progress,
            commands::snapchat_cancel_connect,
            commands::snapchat_finish_with_code,
            commands::snapchat_check_connection,
            commands::snapchat_disconnect,
            commands::open_link,
            commands::match_suggestions,
            commands::confirm_match,
            commands::dismiss_match,
            commands::monitor,
            commands::retry_job,
            commands::cancel_job,
            commands::preferences,
            commands::update_preferences,
            commands::media_status,
            commands::open_folder,
            cloud::cloud_status,
            cloud::cloud_configure,
            cloud::cloud_set_enabled,
            cloud::cloud_forget,
            cloud::cloud_sync_now,
            cloud::send_to_cloud,
            cloud::take_back_from_cloud,
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
