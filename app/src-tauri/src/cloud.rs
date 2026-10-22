//! Tauri commands for the optional cloud runner. Each one forwards to
//! `shorts_desktop::cloud::CloudSync`; the installation token goes straight to the OS
//! credential store and is never returned to the webview.

use shorts_desktop::AppError;
use shorts_desktop::cloud::CloudStatus;
use shorts_desktop::views::PublicationView;
use shorts_domain::PublicationId;
use tauri::State;

use crate::Shell;

type Result<T> = std::result::Result<T, AppError>;

#[tauri::command]
pub async fn cloud_status(shell: State<'_, Shell>) -> Result<CloudStatus> {
    shell.cloud.status().await
}

#[tauri::command]
pub async fn cloud_configure(
    shell: State<'_, Shell>,
    url: String,
    token: String,
) -> Result<CloudStatus> {
    shell.cloud.configure(&url, &token).await
}

#[tauri::command]
pub async fn cloud_set_enabled(shell: State<'_, Shell>, enabled: bool) -> Result<CloudStatus> {
    shell.cloud.set_enabled(enabled).await
}

#[tauri::command]
pub async fn cloud_forget(shell: State<'_, Shell>) -> Result<CloudStatus> {
    shell.cloud.forget().await
}

#[tauri::command]
pub async fn cloud_sync_now(shell: State<'_, Shell>) -> Result<CloudStatus> {
    shell.cloud.sync_now().await
}

#[tauri::command]
pub async fn send_to_cloud(shell: State<'_, Shell>, id: PublicationId) -> Result<PublicationView> {
    shell.cloud.send(id).await
}

#[tauri::command]
pub async fn take_back_from_cloud(
    shell: State<'_, Shell>,
    id: PublicationId,
) -> Result<PublicationView> {
    shell.cloud.take_back(id).await
}
