//! Tauri commands. Each one forwards to `shorts_desktop::App`.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::Serialize;
use shorts_desktop::views::{
    AccountView, AssetDetail, AssetSummary, MonitorView, PublicationView, ReviewView, TagCount,
    VariantView,
};
use shorts_desktop::{
    AppError, AssetUpdate, EditPublication, ImportOutcome, LibraryQuery, NewPublication,
    Preferences, PreferencesUpdate, ScheduleInput,
};
use shorts_domain::{
    AccountId, AssetId, JobId, OutcomeResolution, Platform, PublicationId, RenderRecipe, VariantId,
};
use tauri::State;

use crate::{MediaStatus, Shell};

type Result<T> = std::result::Result<T, AppError>;

#[tauri::command]
pub async fn list_assets(
    shell: State<'_, Shell>,
    query: LibraryQuery,
) -> Result<Vec<AssetSummary>> {
    shell.app.list_assets(&query).await
}

#[tauri::command]
pub async fn asset_detail(shell: State<'_, Shell>, id: AssetId) -> Result<AssetDetail> {
    shell.app.asset_detail(id).await
}

#[tauri::command]
pub async fn update_asset(
    shell: State<'_, Shell>,
    id: AssetId,
    update: AssetUpdate,
) -> Result<AssetSummary> {
    shell.app.update_asset(id, update).await
}

#[tauri::command]
pub async fn tag_counts(shell: State<'_, Shell>) -> Result<Vec<TagCount>> {
    shell.app.tag_counts().await
}

#[tauri::command]
pub async fn thumbnail(shell: State<'_, Shell>, id: AssetId) -> Result<Option<String>> {
    Ok(shell
        .app
        .thumbnail(id)
        .await?
        .map(|p| p.to_string_lossy().into_owned()))
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ImportResult {
    path: String,
    outcome: &'static str,
    asset_id: Option<String>,
    message: Option<String>,
}

/// Imports dropped or picked files one by one; one failure doesn't stop the rest.
#[tauri::command]
pub async fn import_files(
    shell: State<'_, Shell>,
    paths: Vec<String>,
    into: Option<AssetId>,
) -> Result<Vec<ImportResult>> {
    let mut results = Vec::with_capacity(paths.len());
    for path in paths {
        let result = match shell.app.import_file(Path::new(&path), into).await {
            Ok(ImportOutcome::Imported { asset_id }) => ImportResult {
                path,
                outcome: "imported",
                asset_id: Some(asset_id.to_string()),
                message: None,
            },
            Ok(ImportOutcome::Duplicate { asset_id }) => ImportResult {
                path,
                outcome: "duplicate",
                asset_id: Some(asset_id.to_string()),
                message: None,
            },
            Err(e) => ImportResult {
                path,
                outcome: "failed",
                asset_id: None,
                message: Some(e.to_string()),
            },
        };
        results.push(result);
    }
    Ok(results)
}

#[tauri::command]
pub async fn save_link(
    shell: State<'_, Shell>,
    url: String,
    title: Option<String>,
) -> Result<String> {
    Ok(shell
        .app
        .save_link(&url, title.as_deref())
        .await?
        .to_string())
}

#[tauri::command]
pub async fn create_variant(
    shell: State<'_, Shell>,
    asset_id: AssetId,
    name: String,
    recipe: RenderRecipe,
) -> Result<VariantView> {
    shell.app.create_variant(asset_id, &name, recipe).await
}

#[tauri::command]
pub async fn update_variant(
    shell: State<'_, Shell>,
    id: VariantId,
    name: String,
    recipe: RenderRecipe,
) -> Result<VariantView> {
    shell.app.update_variant(id, &name, recipe).await
}

#[tauri::command]
pub async fn render_variant(shell: State<'_, Shell>, id: VariantId) -> Result<()> {
    shell.app.render_variant(id).await
}

#[tauri::command]
pub async fn list_accounts(shell: State<'_, Shell>) -> Result<Vec<AccountView>> {
    shell.app.list_accounts().await
}

#[tauri::command]
pub async fn add_account(
    shell: State<'_, Shell>,
    platform: Platform,
    display_name: String,
) -> Result<AccountView> {
    shell.app.add_manual_account(platform, &display_name).await
}

#[tauri::command]
pub async fn rename_account(
    shell: State<'_, Shell>,
    id: AccountId,
    display_name: String,
) -> Result<AccountView> {
    shell.app.rename_account(id, &display_name).await
}

#[tauri::command]
pub async fn create_publication(
    shell: State<'_, Shell>,
    publication: NewPublication,
) -> Result<PublicationView> {
    shell.app.create_publication(publication).await
}

#[tauri::command]
pub async fn edit_publication(
    shell: State<'_, Shell>,
    id: PublicationId,
    edit: EditPublication,
) -> Result<PublicationView> {
    shell.app.edit_publication(id, edit).await
}

#[tauri::command]
pub async fn submit_for_review(
    shell: State<'_, Shell>,
    id: PublicationId,
) -> Result<PublicationView> {
    shell.app.submit_for_review(id).await
}

#[tauri::command]
pub async fn review(shell: State<'_, Shell>, id: PublicationId) -> Result<ReviewView> {
    shell.app.review(id).await
}

#[tauri::command]
pub async fn approve(
    shell: State<'_, Shell>,
    id: PublicationId,
    revision: u32,
) -> Result<PublicationView> {
    shell.app.approve(id, revision).await
}

#[tauri::command]
pub async fn revoke_approval(
    shell: State<'_, Shell>,
    id: PublicationId,
) -> Result<PublicationView> {
    shell.app.revoke_approval(id).await
}

#[tauri::command]
pub async fn schedule(
    shell: State<'_, Shell>,
    id: PublicationId,
    input: ScheduleInput,
) -> Result<PublicationView> {
    shell.app.schedule(id, input).await
}

#[tauri::command]
pub async fn cancel_publication(
    shell: State<'_, Shell>,
    id: PublicationId,
) -> Result<PublicationView> {
    shell.app.cancel_publication(id).await
}

#[tauri::command]
pub async fn retry_publication(
    shell: State<'_, Shell>,
    id: PublicationId,
) -> Result<PublicationView> {
    shell.app.retry_publication(id).await
}

#[tauri::command]
pub async fn resolve_user_action(
    shell: State<'_, Shell>,
    id: PublicationId,
) -> Result<PublicationView> {
    shell.app.resolve_user_action(id).await
}

#[tauri::command]
pub async fn confirm_posted(
    shell: State<'_, Shell>,
    id: PublicationId,
    url: Option<String>,
) -> Result<PublicationView> {
    shell.app.confirm_posted(id, url).await
}

#[tauri::command]
pub async fn resolve_outcome(
    shell: State<'_, Shell>,
    id: PublicationId,
    resolution: OutcomeResolution,
) -> Result<PublicationView> {
    shell.app.resolve_outcome(id, resolution).await
}

#[tauri::command]
pub async fn export_package(shell: State<'_, Shell>, id: PublicationId) -> Result<()> {
    shell.app.export_package(id).await
}

#[tauri::command]
pub async fn next_slot(
    shell: State<'_, Shell>,
    account_id: AccountId,
) -> Result<Option<DateTime<Utc>>> {
    shell.app.next_slot(account_id).await
}

#[tauri::command]
pub async fn monitor(shell: State<'_, Shell>) -> Result<MonitorView> {
    shell.app.monitor().await
}

#[tauri::command]
pub async fn retry_job(shell: State<'_, Shell>, id: JobId) -> Result<()> {
    shell.app.retry_job(id).await
}

#[tauri::command]
pub async fn cancel_job(shell: State<'_, Shell>, id: JobId) -> Result<()> {
    shell.app.cancel_job(id).await
}

#[tauri::command]
pub async fn preferences(shell: State<'_, Shell>) -> Result<Preferences> {
    shell.app.preferences().await
}

#[tauri::command]
pub async fn update_preferences(
    app: tauri::AppHandle,
    shell: State<'_, Shell>,
    update: PreferencesUpdate,
) -> Result<Preferences> {
    let restart_watch = update.watch_folder.is_some();
    let prefs = shell.app.update_preferences(update).await?;
    if restart_watch {
        shell.restart_watch(&app).await;
    }
    Ok(prefs)
}

#[tauri::command]
pub fn media_status(shell: State<'_, Shell>) -> MediaStatus {
    shell.media.clone()
}

/// Opens an export package folder in the system file manager. Only folders under the export
/// root (or the app's data folder) are accepted.
#[tauri::command]
pub async fn open_folder(shell: State<'_, Shell>, path: String) -> Result<()> {
    let path = PathBuf::from(path);
    let prefs = shell.app.preferences().await?;
    let mut roots = vec![shell.app.paths().data_dir.clone()];
    if let Some(dir) = prefs.export_dir {
        roots.push(PathBuf::from(dir));
    }
    let canonical = path
        .canonicalize()
        .map_err(|_| AppError::NotFound(path.display().to_string()))?;
    let allowed = roots
        .iter()
        .filter_map(|r| r.canonicalize().ok())
        .any(|root| canonical.starts_with(root));
    if !allowed || !canonical.is_dir() {
        return Err(AppError::Invalid(
            "only export package folders can be opened".into(),
        ));
    }
    let program = if cfg!(windows) {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    };
    std::process::Command::new(program)
        .arg(display_path(&canonical))
        .spawn()?;
    Ok(())
}

/// `canonicalize` returns `\\?\C:\...` on Windows, which Explorer doesn't accept.
fn display_path(path: &Path) -> PathBuf {
    let s = path.to_string_lossy();
    match s.strip_prefix(r"\\?\") {
        Some(rest) if !rest.starts_with("UNC\\") => PathBuf::from(rest),
        _ => path.to_owned(),
    }
}
