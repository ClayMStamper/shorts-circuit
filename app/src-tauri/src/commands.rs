//! Tauri commands. Each one forwards to `shorts_desktop::App`.

use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde::Serialize;
use shorts_adapters::linkedin::Organization;
use shorts_desktop::instagram::{InstagramPages, InstagramStatus};
use shorts_desktop::linkedin::LinkedInStatus;
use shorts_desktop::snapchat::{SnapConnectProgress, SnapStatus};
use shorts_desktop::tiktok::{CheckReport, ConnectProgress, TikTokAppInfo, TikTokStatus};
use shorts_desktop::views::{
    AccountView, ActivityView, AssetDetail, AssetSummary, CollectionView, MatchSuggestion,
    MonitorView, PublicationView, ReviewView, RuleActivityView, TagCount, VariantView,
    WorkflowDetailView, WorkflowView, WorkflowsView,
};
use shorts_desktop::youtube::{YouTubeConnectProgress, YouTubeStatus};
use shorts_desktop::{
    AppError, AssetUpdate, EditPublication, ImportOutcome, LibraryQuery, NewPublication,
    NewWorkflow, Preferences, PreferencesUpdate, RuleInput, ScheduleInput, WorkflowUpdate,
};
use shorts_domain::instagram::InstagramLogin;
use shorts_domain::{
    AccountId, AssetId, CollectionId, JobId, OutcomeResolution, Platform, PublicationId,
    RenderRecipe, RuleId, VariantId, WorkflowId,
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
pub async fn list_collections(shell: State<'_, Shell>) -> Result<Vec<CollectionView>> {
    shell.app.list_collections().await
}

#[tauri::command]
pub async fn create_collection(shell: State<'_, Shell>, name: String) -> Result<CollectionView> {
    shell.app.create_collection(&name).await
}

#[tauri::command]
pub async fn rename_collection(
    shell: State<'_, Shell>,
    id: CollectionId,
    name: String,
) -> Result<CollectionView> {
    shell.app.rename_collection(id, &name).await
}

#[tauri::command]
pub async fn delete_collection(shell: State<'_, Shell>, id: CollectionId) -> Result<()> {
    shell.app.delete_collection(id).await
}

/// Returns the ids of the publications the collection's rules created.
#[tauri::command]
pub async fn add_to_collection(
    shell: State<'_, Shell>,
    collection_id: CollectionId,
    asset_id: AssetId,
) -> Result<Vec<String>> {
    Ok(shell
        .app
        .add_to_collection(collection_id, asset_id)
        .await?
        .iter()
        .map(ToString::to_string)
        .collect())
}

#[tauri::command]
pub async fn remove_from_collection(
    shell: State<'_, Shell>,
    collection_id: CollectionId,
    asset_id: AssetId,
) -> Result<()> {
    shell
        .app
        .remove_from_collection(collection_id, asset_id)
        .await
}

#[tauri::command]
pub async fn create_rule(
    shell: State<'_, Shell>,
    collection_id: CollectionId,
    rule: RuleInput,
) -> Result<CollectionView> {
    shell.app.create_rule(collection_id, rule).await
}

#[tauri::command]
pub async fn update_rule(
    shell: State<'_, Shell>,
    id: RuleId,
    rule: RuleInput,
) -> Result<CollectionView> {
    shell.app.update_rule(id, rule).await
}

#[tauri::command]
pub async fn delete_rule(shell: State<'_, Shell>, id: RuleId) -> Result<CollectionView> {
    shell.app.delete_rule(id).await
}

#[tauri::command]
pub async fn rule_activity(shell: State<'_, Shell>, id: RuleId) -> Result<RuleActivityView> {
    shell.app.rule_activity(id).await
}

/// Lists the watch folder now ("Check now"); returns the updated preferences.
#[tauri::command]
pub async fn check_watch_folder(shell: State<'_, Shell>) -> Result<Preferences> {
    let check = shell
        .watch
        .lock()
        .expect("watch lock")
        .as_ref()
        .map(|w| w.check_now())
        .ok_or_else(|| AppError::Invalid("no watch folder is set".into()))?;
    check.await?;
    shell.app.preferences().await
}

#[tauri::command]
pub async fn set_unattended(
    shell: State<'_, Shell>,
    collection_id: CollectionId,
    account_id: AccountId,
    enabled: bool,
) -> Result<CollectionView> {
    shell
        .app
        .set_unattended(collection_id, account_id, enabled)
        .await
}

#[tauri::command]
pub async fn list_workflows(shell: State<'_, Shell>) -> Result<WorkflowsView> {
    shell.app.list_workflows().await
}

#[tauri::command]
pub async fn workflow_detail(
    shell: State<'_, Shell>,
    id: WorkflowId,
) -> Result<WorkflowDetailView> {
    shell.app.workflow_detail(id).await
}

#[tauri::command]
pub async fn create_workflow(
    shell: State<'_, Shell>,
    workflow: NewWorkflow,
) -> Result<WorkflowView> {
    shell.app.create_workflow(workflow).await
}

#[tauri::command]
pub async fn update_workflow(
    shell: State<'_, Shell>,
    id: WorkflowId,
    update: WorkflowUpdate,
) -> Result<WorkflowView> {
    shell.app.update_workflow(id, update).await
}

#[tauri::command]
pub async fn delete_workflow(shell: State<'_, Shell>, id: WorkflowId) -> Result<()> {
    shell.app.delete_workflow(id).await
}

#[tauri::command]
pub async fn activity(shell: State<'_, Shell>, limit: Option<u32>) -> Result<ActivityView> {
    shell.app.activity(limit.unwrap_or(200)).await
}

#[tauri::command]
pub async fn publish_now(shell: State<'_, Shell>, id: PublicationId) -> Result<PublicationView> {
    shell.app.publish_now(id).await
}

#[tauri::command]
pub async fn republish(shell: State<'_, Shell>, id: PublicationId) -> Result<PublicationView> {
    shell.app.republish(id).await
}

/// "Check for new content now": lists the watch folder and asks TikTok for new posts (when
/// connected), then returns the Workflows page. A source that isn't set up is skipped.
#[tauri::command]
pub async fn check_for_new_content(shell: State<'_, Shell>) -> Result<WorkflowsView> {
    let watch = shell
        .watch
        .lock()
        .expect("watch lock")
        .as_ref()
        .map(|w| w.check_now());
    if let Some(check) = watch {
        check.await?;
    }
    if matches!(shell.tiktok.status().await?, TikTokStatus::Connected { .. }) {
        shell.tiktok.check_now().await?;
    }
    shell.app.apply_deferred_rules().await;
    shell.app.list_workflows().await
}

#[tauri::command]
pub async fn tiktok_app_info(shell: State<'_, Shell>) -> Result<TikTokAppInfo> {
    shell.tiktok.app_info()
}

/// Saves the owner's TikTok app in the OS credential store. The secret is never sent back.
#[tauri::command]
pub async fn tiktok_set_app(
    shell: State<'_, Shell>,
    client_key: String,
    client_secret: String,
) -> Result<TikTokStatus> {
    shell.tiktok.set_app(&client_key, &client_secret).await
}

#[tauri::command]
pub async fn tiktok_clear_app(shell: State<'_, Shell>) -> Result<TikTokStatus> {
    shell.tiktok.clear_app().await
}

#[tauri::command]
pub async fn tiktok_status(shell: State<'_, Shell>) -> Result<TikTokStatus> {
    shell.tiktok.status().await
}

#[tauri::command]
pub async fn tiktok_set_enabled(shell: State<'_, Shell>, enabled: bool) -> Result<TikTokStatus> {
    shell.tiktok.set_enabled(enabled).await
}

/// Opens TikTok's (or the broker's) sign-in page in the default browser. The UI then calls
/// `tiktok_finish_connect` until it stops reporting `pending`.
#[tauri::command]
pub async fn tiktok_connect(shell: State<'_, Shell>) -> Result<()> {
    let url = shell.tiktok.begin_connect().await?;
    if !matches!(url.scheme(), "https" | "http") {
        return Err(AppError::Invalid("the sign-in URL must be http(s)".into()));
    }
    open_external(url.as_str())?;
    Ok(())
}

#[tauri::command]
pub async fn tiktok_finish_connect(shell: State<'_, Shell>) -> Result<ConnectProgress> {
    shell.tiktok.finish_connect().await
}

#[tauri::command]
pub async fn tiktok_disconnect(shell: State<'_, Shell>) -> Result<TikTokStatus> {
    shell.tiktok.disconnect().await
}

#[tauri::command]
pub async fn tiktok_check_now(shell: State<'_, Shell>) -> Result<CheckReport> {
    shell.tiktok.check_now().await
}

#[tauri::command]
pub async fn youtube_status(shell: State<'_, Shell>) -> Result<YouTubeStatus> {
    shell.youtube.status().await
}

/// Saves the owner's Google OAuth client in the OS credential store. The secret is never sent
/// back to the webview.
#[tauri::command]
pub async fn youtube_set_client(
    shell: State<'_, Shell>,
    client_id: String,
    client_secret: String,
) -> Result<YouTubeStatus> {
    shell.youtube.set_client(&client_id, &client_secret).await
}

#[tauri::command]
pub async fn youtube_clear_client(shell: State<'_, Shell>) -> Result<YouTubeStatus> {
    shell.youtube.clear_client().await
}

/// Opens Google's consent page in the default browser. The UI then calls
/// `youtube_connect_progress` until it stops reporting `pending`.
#[tauri::command]
pub async fn youtube_connect(shell: State<'_, Shell>, account_id: Option<AccountId>) -> Result<()> {
    let url = shell.youtube.begin_connect(account_id).await?;
    if url.scheme() != "https" {
        return Err(AppError::Invalid(
            "the Google sign-in page must be https".into(),
        ));
    }
    open_external(url.as_str())?;
    Ok(())
}

#[tauri::command]
pub fn youtube_connect_progress(shell: State<'_, Shell>) -> YouTubeConnectProgress {
    shell.youtube.connect_progress()
}

#[tauri::command]
pub fn youtube_cancel_connect(shell: State<'_, Shell>) {
    shell.youtube.cancel_connect();
}

#[tauri::command]
pub async fn youtube_disconnect(
    shell: State<'_, Shell>,
    account_id: AccountId,
) -> Result<YouTubeStatus> {
    shell.youtube.disconnect(account_id).await
}

#[tauri::command]
pub async fn youtube_set_public_uploads(
    shell: State<'_, Shell>,
    account_id: AccountId,
    public: bool,
) -> Result<YouTubeStatus> {
    shell.youtube.set_public_uploads(account_id, public).await
}

#[tauri::command]
pub async fn instagram_status(shell: State<'_, Shell>) -> Result<InstagramStatus> {
    shell.instagram.status().await
}

/// The Facebook Pages a pasted user token manages, with their linked Instagram accounts. The
/// token is not stored by this call.
#[tauri::command]
pub async fn instagram_find_pages(
    shell: State<'_, Shell>,
    access_token: String,
) -> Result<InstagramPages> {
    shell.instagram.find_pages(&access_token).await
}

/// Connects an Instagram account: with Facebook login, the one linked to `page_id` (or the only
/// linked Page), keeping that Page's token; with Instagram login, the account the token
/// belongs to (export packages only). Tokens go to the OS credential store and are never sent
/// back to the webview.
#[tauri::command]
pub async fn instagram_connect(
    shell: State<'_, Shell>,
    access_token: String,
    login: InstagramLogin,
    page_id: Option<String>,
    account_id: Option<AccountId>,
) -> Result<InstagramStatus> {
    match login {
        InstagramLogin::FacebookLogin => {
            shell
                .instagram
                .connect(&access_token, page_id.as_deref(), account_id)
                .await?
        }
        InstagramLogin::InstagramLogin => {
            shell
                .instagram
                .connect_instagram_login(&access_token, account_id)
                .await?
        }
    };
    shell.instagram.status().await
}

#[tauri::command]
pub async fn instagram_disconnect(
    shell: State<'_, Shell>,
    account_id: AccountId,
) -> Result<InstagramStatus> {
    shell.instagram.disconnect(account_id).await
}

#[tauri::command]
pub async fn linkedin_status(shell: State<'_, Shell>) -> Result<LinkedInStatus> {
    shell.linkedin.status().await
}

/// The Company Pages a pasted token administers. The token is not stored by this call.
#[tauri::command]
pub async fn linkedin_find_pages(
    shell: State<'_, Shell>,
    access_token: String,
) -> Result<Vec<Organization>> {
    shell.linkedin.find_pages(&access_token).await
}

/// Connects a Company Page with a pasted token. The token goes to the OS credential store and
/// is never sent back to the webview.
#[tauri::command]
pub async fn linkedin_connect(
    shell: State<'_, Shell>,
    access_token: String,
    page: String,
    account_id: Option<AccountId>,
) -> Result<LinkedInStatus> {
    shell
        .linkedin
        .connect(&access_token, &page, account_id)
        .await?;
    shell.linkedin.status().await
}

#[tauri::command]
pub async fn linkedin_disconnect(
    shell: State<'_, Shell>,
    account_id: AccountId,
) -> Result<LinkedInStatus> {
    shell.linkedin.disconnect(account_id).await
}

#[tauri::command]
pub async fn snapchat_status(shell: State<'_, Shell>) -> Result<SnapStatus> {
    shell.snapchat.status().await
}

/// Saves the owner's Snap OAuth app (client id, secret and its registered redirect address) in
/// the OS credential store. The secret is never sent back to the webview.
#[tauri::command]
pub async fn snapchat_set_client(
    shell: State<'_, Shell>,
    client_id: String,
    client_secret: String,
    redirect_uri: String,
) -> Result<SnapStatus> {
    shell
        .snapchat
        .set_client(&client_id, &client_secret, &redirect_uri)
        .await
}

#[tauri::command]
pub async fn snapchat_clear_client(shell: State<'_, Shell>) -> Result<SnapStatus> {
    shell.snapchat.clear_client().await
}

/// Opens Snap's consent page in the default browser. With a loopback redirect the UI then
/// calls `snapchat_connect_progress` until it stops reporting `pending`; otherwise the owner
/// pastes the redirect page's text into `snapchat_finish_with_code`.
#[tauri::command]
pub async fn snapchat_connect(
    shell: State<'_, Shell>,
    account_id: Option<AccountId>,
) -> Result<()> {
    let url = shell.snapchat.begin_connect(account_id).await?;
    if url.scheme() != "https" {
        shell.snapchat.cancel_connect();
        return Err(AppError::Invalid(
            "the Snap sign-in page must be https".into(),
        ));
    }
    open_external(url.as_str())?;
    Ok(())
}

#[tauri::command]
pub fn snapchat_connect_progress(shell: State<'_, Shell>) -> SnapConnectProgress {
    shell.snapchat.connect_progress()
}

#[tauri::command]
pub fn snapchat_cancel_connect(shell: State<'_, Shell>) {
    shell.snapchat.cancel_connect();
}

/// Finishes a paste-mode sign-in with the text Snap's redirect page showed. The code is
/// exchanged here; no token comes back to the webview.
#[tauri::command]
pub async fn snapchat_finish_with_code(
    shell: State<'_, Shell>,
    pasted: String,
) -> Result<SnapStatus> {
    shell.snapchat.finish_with_code(&pasted).await?;
    shell.snapchat.status().await
}

#[tauri::command]
pub async fn snapchat_check_connection(
    shell: State<'_, Shell>,
    account_id: AccountId,
) -> Result<SnapStatus> {
    shell.snapchat.check_connection(account_id).await
}

#[tauri::command]
pub async fn snapchat_disconnect(
    shell: State<'_, Shell>,
    account_id: AccountId,
) -> Result<SnapStatus> {
    shell.snapchat.disconnect(account_id).await
}

/// Opens a published post (an https link the app recorded) in the default browser.
#[tauri::command]
pub fn open_link(url: String) -> Result<()> {
    let parsed = https_link(&url)?;
    open_external(&parsed)
}

#[tauri::command]
pub async fn match_suggestions(shell: State<'_, Shell>) -> Result<Vec<MatchSuggestion>> {
    shell.app.match_suggestions().await
}

#[tauri::command]
pub async fn confirm_match(
    shell: State<'_, Shell>,
    video_id: String,
    asset_id: AssetId,
) -> Result<AssetSummary> {
    shell.app.confirm_match(&video_id, asset_id).await
}

#[tauri::command]
pub async fn dismiss_match(
    shell: State<'_, Shell>,
    video_id: String,
    asset_id: AssetId,
) -> Result<()> {
    shell.app.dismiss_match(&video_id, asset_id).await
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

/// What "Get original" did, for the UI to say.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GetOriginal {
    /// The folder a download is picked up from (`None`: no downloads folder could be found).
    pub downloads_folder: Option<String>,
    /// Picking up downloads was off and has just been turned on.
    pub enabled_now: bool,
}

fn os_downloads_folder(app: &tauri::AppHandle) -> Option<String> {
    use tauri::Manager;
    app.path()
        .download_dir()
        .ok()
        .map(|p| p.to_string_lossy().into_owned())
}

/// "Get original": opens a waiting TikTok post in the browser and expects its video in the
/// downloads folder (TikTok's own "Download video"). Turns picking up downloads on (the OS
/// downloads folder) if it was off, since that's what the owner is asking for.
#[tauri::command]
pub async fn get_original(
    app: tauri::AppHandle,
    shell: State<'_, Shell>,
    asset_id: AssetId,
) -> Result<GetOriginal> {
    let url = shell.app.expect_original(asset_id).await?;
    let url = https_link(&url)?;
    let mut result = GetOriginal {
        downloads_folder: shell.app.preferences().await?.downloads_folder,
        enabled_now: false,
    };
    if result.downloads_folder.is_none()
        && let Some(folder) = os_downloads_folder(&app)
    {
        shell
            .app
            .update_preferences(PreferencesUpdate {
                downloads_folder: Some(Some(folder.clone())),
                ..Default::default()
            })
            .await?;
        shell.restart_watch(&app).await;
        result = GetOriginal {
            downloads_folder: Some(folder),
            enabled_now: true,
        };
    }
    open_external(&url)?;
    Ok(result)
}

/// "Send through workflows": runs the workflows for an item that never reached them. Returns how
/// many publications were created.
#[tauri::command]
pub async fn send_to_workflows(shell: State<'_, Shell>, asset_id: AssetId) -> Result<usize> {
    Ok(shell.app.send_to_workflows(asset_id).await?.len())
}

#[tauri::command]
pub async fn cancel_get_original(shell: State<'_, Shell>, asset_id: AssetId) -> Result<()> {
    shell.app.cancel_expect_original(asset_id).await
}

/// "Pick file…": attaches a chosen video to a waiting item.
#[tauri::command]
pub async fn attach_original(
    shell: State<'_, Shell>,
    asset_id: AssetId,
    path: String,
) -> Result<()> {
    shell
        .app
        .attach_original(asset_id, std::path::Path::new(&path))
        .await?;
    Ok(())
}

/// The OS downloads folder, to offer as the folder to watch for TikTok originals.
#[tauri::command]
pub fn default_downloads_folder(app: tauri::AppHandle) -> Option<String> {
    os_downloads_folder(&app)
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
    let restart_watch = update.watch_folder.is_some() || update.downloads_folder.is_some();
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
    std::process::Command::new(opener())
        .arg(display_path(&canonical))
        .spawn()?;
    Ok(())
}

fn opener() -> &'static str {
    if cfg!(windows) {
        "explorer"
    } else if cfg!(target_os = "macos") {
        "open"
    } else {
        "xdg-open"
    }
}

/// Accepts only `https://` links without spaces or quotes.
fn https_link(url: &str) -> Result<String> {
    let url = url.trim();
    let ok = url.starts_with("https://")
        && url.len() < 2048
        && !url
            .chars()
            .any(|c| c.is_whitespace() || c.is_control() || c == '"');
    if ok {
        Ok(url.to_owned())
    } else {
        Err(AppError::Invalid("only https links can be opened".into()))
    }
}

/// Opens a web page in the default browser. The URL is one argument, never shell-parsed.
///
/// On Windows this goes through the URL protocol handler rather than `explorer`, which
/// misreads long URLs with query strings (such as OAuth consent pages) and opens a folder.
fn open_external(url: &str) -> Result<()> {
    if cfg!(windows) {
        std::process::Command::new("rundll32.exe")
            .arg("url.dll,FileProtocolHandler")
            .arg(url)
            .spawn()?;
    } else {
        std::process::Command::new(opener()).arg(url).spawn()?;
    }
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
