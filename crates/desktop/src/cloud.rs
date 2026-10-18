//! Optional cloud mode: hands approved publications to the owner's cloud runner, which posts
//! them while this computer is off, and mirrors what the runner reports.
//!
//! Off by default. Until a runner address and installation token are saved and cloud mode is
//! turned on, nothing here talks to a network and no publication changes owner. The address
//! is kept in settings; the token in the OS credential store (`cloud_runner_token`).
//!
//! Handing over moves ownership first (one SQLite transaction that also cancels this
//! computer's publish job), then delivers the account credential, the rendered video and the
//! revision. The sync loop re-delivers any cloud-owned publication the runner doesn't list,
//! mirrors the runner's reports, pulls credentials the runner rotated and pushes new ones after
//! a reconnect. Taking back asks the runner to release the publication first; only then does
//! this computer own it again.

use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use shorts_cloud::{
    AccountPut, ClientError, CloudClient, CloudCredential, CredentialSource, ErrorCode,
    HandoverPut, MediaRef, PublicationStatus, sha256_file,
};
use shorts_domain::{
    AccountId, ConnectedAccount, Execution, ExecutionOwner, HandoverId, OwnerReport, Platform,
    Publication, PublicationEvent, PublicationId, PublicationState, TransitionError,
};
use shorts_publish::PublishCheckpoint;
use shorts_store::{PublicationFilter, StoreError};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tracing::{info, warn};

use crate::app::App;
use crate::credentials::CredentialStore;
use crate::error::{AppError, AppResult};
use crate::events::AppEvent;
use crate::views::PublicationView;
use crate::{instagram, linkedin, snapchat, youtube};

const SETTINGS_KEY: &str = "cloud_runner";
const ACCOUNTS_KEY: &str = "cloud_accounts";
/// The installation token's entry in the OS credential store.
pub const TOKEN_KEY: &str = "cloud_runner_token";
const SYNC_EVERY: Duration = Duration::from_secs(30);

pub const NEXT_STEP: &str = "Set up a cloud runner (see docs/cloud-runner.md), create an \
    installation token on it with `shorts-runner token create`, and paste its address and the \
    token here.";

/// Saved in settings. No secrets.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CloudSettings {
    pub(crate) enabled: bool,
    pub(crate) url: Option<String>,
}

/// What this computer last sent the runner for an account.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct SentCredential {
    revision: u64,
    digest: String,
}

pub(crate) async fn settings(app: &App) -> AppResult<CloudSettings> {
    Ok(app
        .store
        .settings()
        .get(SETTINGS_KEY)
        .await?
        .unwrap_or_default())
}

/// Cloud mode as the Settings screen shows it.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CloudStatus {
    pub enabled: bool,
    pub url: Option<String>,
    pub token_saved: bool,
    /// Publications the cloud runner owns right now.
    pub held: usize,
    pub last_sync_at: Option<DateTime<Utc>>,
    pub last_error: Option<String>,
    pub next_step: Option<String>,
}

#[derive(Debug, Default)]
struct SyncState {
    last_sync_at: Option<DateTime<Utc>>,
    last_error: Option<String>,
}

fn credential_error(e: impl std::fmt::Display) -> AppError {
    AppError::Intake(format!("credential store: {e}"))
}

fn export(
    credentials: &dyn CredentialStore,
    account: &ConnectedAccount,
) -> Result<Option<CloudCredential>, String> {
    match account.platform {
        Platform::YoutubeShorts => youtube::cloud_credential(credentials, account.id),
        Platform::InstagramReels => instagram::cloud_credential(credentials, account.id),
        Platform::Linkedin => linkedin::cloud_credential(credentials, account.id),
        Platform::SnapchatSpotlight => snapchat::cloud_credential(credentials, account.id),
    }
}

fn import(
    credentials: &dyn CredentialStore,
    account: &ConnectedAccount,
    credential: &CloudCredential,
) -> Result<(), String> {
    if credential.platform() != account.platform {
        return Err("the cloud runner's credential is for another platform".into());
    }
    match account.platform {
        Platform::YoutubeShorts => {
            youtube::import_cloud_credential(credentials, account.id, credential)
        }
        Platform::InstagramReels => {
            instagram::import_cloud_credential(credentials, account.id, credential)
        }
        Platform::Linkedin => {
            linkedin::import_cloud_credential(credentials, account.id, credential)
        }
        Platform::SnapchatSpotlight => {
            snapchat::import_cloud_credential(credentials, account.id, credential)
        }
    }
}

/// The desktop side of cloud mode.
pub struct CloudSync {
    app: Arc<App>,
    credentials: Arc<dyn CredentialStore>,
    /// One sync, hand-over or take-back at a time, so a re-delivery never races a take-back.
    busy: tokio::sync::Mutex<()>,
    state: Mutex<SyncState>,
    wake: Notify,
}

impl CloudSync {
    pub fn new(app: Arc<App>, credentials: Arc<dyn CredentialStore>) -> Arc<Self> {
        Arc::new(Self {
            app,
            credentials,
            busy: tokio::sync::Mutex::new(()),
            state: Mutex::new(SyncState::default()),
            wake: Notify::new(),
        })
    }

    /// A client for the saved runner, if an address and token are saved.
    fn configured(&self, settings: &CloudSettings) -> AppResult<Option<CloudClient>> {
        let Some(url) = &settings.url else {
            return Ok(None);
        };
        let Some(token) = self.credentials.get(TOKEN_KEY).map_err(credential_error)? else {
            return Ok(None);
        };
        Ok(Some(CloudClient::new(url, &token)?))
    }

    /// The client when cloud mode is on.
    async fn client(&self) -> AppResult<Option<CloudClient>> {
        let settings = settings(&self.app).await?;
        if !settings.enabled {
            return Ok(None);
        }
        self.configured(&settings)
    }

    async fn held(&self) -> AppResult<Vec<Publication>> {
        Ok(self
            .app
            .store
            .publications()
            .list(&PublicationFilter {
                owner: Some(ExecutionOwner::Cloud),
                ..Default::default()
            })
            .await?)
    }

    pub async fn status(&self) -> AppResult<CloudStatus> {
        let settings = settings(&self.app).await?;
        let token_saved = self
            .credentials
            .get(TOKEN_KEY)
            .map_err(credential_error)?
            .is_some();
        let held = self.held().await?.len();
        let state = self.state.lock().expect("cloud state");
        Ok(CloudStatus {
            enabled: settings.enabled,
            url: settings.url.clone(),
            token_saved,
            held,
            last_sync_at: state.last_sync_at,
            last_error: state.last_error.clone(),
            next_step: (settings.url.is_none() || !token_saved).then(|| NEXT_STEP.to_owned()),
        })
    }

    /// Saves the runner address and token after checking them, and turns cloud mode on.
    pub async fn configure(&self, url: &str, token: &str) -> AppResult<CloudStatus> {
        let client = CloudClient::new(url, token)?;
        client.check().await?;
        let _busy = self.busy.lock().await;
        self.credentials
            .set(TOKEN_KEY, token.trim())
            .map_err(credential_error)?;
        let settings = CloudSettings {
            enabled: true,
            url: Some(client.base_url().to_string()),
        };
        self.app
            .store
            .settings()
            .set(SETTINGS_KEY, &settings)
            .await?;
        info!("cloud runner configured");
        drop(_busy);
        self.after_change();
        self.status().await
    }

    /// Turns cloud mode on or off. Turning it off is refused while the runner holds
    /// publications, which would otherwise be stranded there.
    pub async fn set_enabled(&self, enabled: bool) -> AppResult<CloudStatus> {
        let _busy = self.busy.lock().await;
        let mut settings = settings(&self.app).await?;
        if enabled && self.configured(&settings)?.is_none() {
            return Err(AppError::Invalid(NEXT_STEP.into()));
        }
        if !enabled {
            self.refuse_while_held().await?;
        }
        settings.enabled = enabled;
        self.app
            .store
            .settings()
            .set(SETTINGS_KEY, &settings)
            .await?;
        drop(_busy);
        self.after_change();
        self.status().await
    }

    /// Forgets the runner address and token (cloud mode off).
    pub async fn forget(&self) -> AppResult<CloudStatus> {
        let _busy = self.busy.lock().await;
        self.refuse_while_held().await?;
        self.credentials
            .delete(TOKEN_KEY)
            .map_err(credential_error)?;
        self.app.store.settings().remove(SETTINGS_KEY).await?;
        self.app.store.settings().remove(ACCOUNTS_KEY).await?;
        drop(_busy);
        self.after_change();
        self.status().await
    }

    async fn refuse_while_held(&self) -> AppResult<()> {
        let held = self.held().await?.len();
        if held > 0 {
            return Err(AppError::Invalid(format!(
                "the cloud runner still has {held} publication(s); take them back first"
            )));
        }
        Ok(())
    }

    fn after_change(&self) {
        self.app.emit(AppEvent::CloudChanged);
        self.app.emit(AppEvent::JobsChanged);
        self.wake.notify_one();
    }

    /// Hands an approved, scheduled publication to the cloud runner.
    pub async fn send(&self, id: PublicationId) -> AppResult<PublicationView> {
        let client = self
            .client()
            .await?
            .ok_or_else(|| AppError::Invalid("turn on cloud mode in Settings first".into()))?;
        let _busy = self.busy.lock().await;
        let p = self.app.store.publications().require(id).await?;
        let account = self
            .app
            .store
            .accounts()
            .require(p.content().destination.account_id)
            .await?;
        if !account.capabilities.can_publish_automatically() {
            return Err(AppError::Invalid(format!(
                "{} uses export packages, which the cloud runner can't post; connect it first",
                account.display_name
            )));
        }
        if export(self.credentials.as_ref(), &account)
            .map_err(credential_error)?
            .is_none()
        {
            return Err(AppError::Invalid(format!(
                "{} isn't signed in on this computer; connect it first",
                account.display_name
            )));
        }
        self.rendered(&p).await?;
        let p = match self
            .app
            .store
            .hand_over_to_cloud(id, HandoverId::new(), self.app.now(), |job| {
                PublishCheckpoint::of(job).reached_provider()
            })
            .await
        {
            Ok(p) => p,
            Err(StoreError::Transition(t)) => return Err(t.into()),
            Err(e) => return Err(e.into()),
        };
        info!(publication = %id, "handed over to the cloud runner");
        self.app
            .emit(AppEvent::PublicationChanged { id: id.to_string() });
        self.app.emit(AppEvent::JobsChanged);
        if let Err(e) = self.deliver(&client, &p).await {
            warn!(publication = %id, error = %e, "could not deliver to the cloud runner yet");
            self.app
                .set_cloud_note(id, Some(format!("Not on the cloud runner yet: {e}")));
        }
        self.app.publication(id).await
    }

    /// Takes a publication back from the cloud runner. Refused while the runner is working on
    /// it.
    pub async fn take_back(&self, id: PublicationId) -> AppResult<PublicationView> {
        let _busy = self.busy.lock().await;
        let p = self.app.store.publications().require(id).await?;
        let Some(handover) = p.handover().filter(|_| p.owner() == ExecutionOwner::Cloud) else {
            return Err(AppError::Invalid(
                "this publication is already on this computer".into(),
            ));
        };
        let settings = settings(&self.app).await?;
        let client = self.configured(&settings)?.ok_or_else(|| {
            AppError::Invalid("the cloud runner's address and token are missing".into())
        })?;
        match client.release(id, handover).await {
            Ok(status) => self.returned(&p, Some(status.report)).await?,
            // The runner never got it, so nothing can be running there.
            Err(ClientError::Api(e)) if e.code == ErrorCode::NotFound => {
                self.returned(&p, None).await?;
            }
            Err(ClientError::Api(e)) if e.code == ErrorCode::InFlight => {
                return Err(AppError::Invalid(e.message));
            }
            Err(e) => return Err(e.into()),
        }
        info!(publication = %id, "taken back from the cloud runner");
        self.app.publication(id).await
    }

    /// Syncs now instead of waiting for the next round.
    pub async fn sync_now(&self) -> AppResult<CloudStatus> {
        self.sync_once().await?;
        self.status().await
    }

    /// Syncs every 30 seconds while cloud mode is on, until `shutdown`.
    pub async fn run(self: Arc<Self>, shutdown: CancellationToken) {
        loop {
            if let Err(e) = self.sync_once().await {
                warn!(error = %e, "cloud sync failed");
            }
            tokio::select! {
                () = tokio::time::sleep(SYNC_EVERY) => {}
                () = self.wake.notified() => {}
                () = shutdown.cancelled() => return,
            }
        }
    }

    /// One round: mirror reports, finish interrupted take-backs, re-deliver what the runner
    /// doesn't have, and exchange changed credentials. Does nothing while cloud mode is off.
    pub async fn sync_once(&self) -> AppResult<()> {
        let Some(client) = self.client().await? else {
            return Ok(());
        };
        let _busy = self.busy.lock().await;
        let result = self.sync_with(&client).await;
        let changed = {
            let mut state = self.state.lock().expect("cloud state");
            let error = result.as_ref().err().map(ToString::to_string);
            let changed = state.last_error != error || state.last_sync_at.is_none();
            state.last_sync_at = Some(self.app.now());
            state.last_error = error;
            changed
        };
        // Publication changes have their own events; this one is for the Settings status.
        if changed {
            self.app.emit(AppEvent::CloudChanged);
        }
        result
    }

    async fn sync_with(&self, client: &CloudClient) -> AppResult<()> {
        self.pull_credentials(client).await?;
        let statuses = client.publications().await?;
        let by_id: HashMap<PublicationId, &PublicationStatus> = statuses
            .iter()
            .map(|s| (s.report.publication_id, s))
            .collect();
        for p in self.held().await? {
            let status = by_id
                .get(&p.id())
                .filter(|s| Some(s.report.handover) == p.handover());
            let outcome = match status {
                Some(s) if s.report.owner == ExecutionOwner::Desktop => {
                    // Released on the runner, but this computer never heard the answer.
                    self.returned(&p, Some(s.report.clone())).await
                }
                Some(s) => self.mirror(&p, &s.report).await.map(|_| ()),
                None => self.deliver(client, &p).await,
            };
            match outcome {
                Ok(()) => self.app.set_cloud_note(p.id(), None),
                Err(e) => {
                    warn!(publication = %p.id(), error = %e, "cloud sync of a publication failed");
                    self.app
                        .set_cloud_note(p.id(), Some(format!("Not on the cloud runner yet: {e}")));
                }
            }
        }
        self.push_reconnected(client).await
    }

    /// The variant render the revision was approved with.
    async fn rendered(
        &self,
        p: &Publication,
    ) -> AppResult<(String, u64, shorts_domain::MediaProperties)> {
        let content = p.content();
        let variant = self
            .app
            .store
            .variants()
            .require(content.variant_id)
            .await?;
        if variant.revision() != content.variant_revision {
            return Err(AppError::Invalid(
                "the variant was edited after this revision was made; edit and re-approve".into(),
            ));
        }
        let render = variant
            .render()
            .filter(|r| Path::new(&r.path).is_file())
            .ok_or_else(|| {
                AppError::Invalid("render the variant before sending it to the cloud".into())
            })?;
        Ok((
            render.path.clone(),
            render.size_bytes,
            render.properties.clone(),
        ))
    }

    /// Sends the account, the video and the revision. Every step is idempotent.
    async fn deliver(&self, client: &CloudClient, p: &Publication) -> AppResult<()> {
        let handover = p
            .handover()
            .ok_or_else(|| AppError::Invalid("not handed over".into()))?;
        let account = self
            .app
            .store
            .accounts()
            .require(p.content().destination.account_id)
            .await?;
        self.push_account(client, &account, true).await?;
        let (path, _, properties) = self.rendered(p).await?;
        let path = Path::new(&path);
        let size_bytes = tokio::fs::metadata(path).await?.len();
        let sha256 = sha256_file(path).await?;
        client.upload_file(path, &sha256, &|_| {}).await?;
        let status = client
            .hand_over(&HandoverPut {
                handover,
                publication: p.record().clone(),
                media: MediaRef {
                    sha256,
                    size_bytes,
                    properties,
                },
            })
            .await?;
        info!(publication = %p.id(), "delivered to the cloud runner");
        self.mirror(p, &status.report).await?;
        Ok(())
    }

    /// Shows the runner's report on the local copy.
    async fn mirror(&self, p: &Publication, report: &OwnerReport) -> AppResult<Publication> {
        let report = report.clone();
        match self
            .app
            .apply_event(p.id(), move |_| PublicationEvent::Mirror {
                report: report.clone(),
            })
            .await
        {
            Ok(p) => Ok(p),
            Err(AppError::Transition(TransitionError::Unchanged)) => Ok(p.clone()),
            Err(e) => Err(e),
        }
    }

    /// The runner gave the publication back (with its last report, unless it never had it):
    /// mirror that, then own it here again and queue its job if it is due.
    async fn returned(&self, p: &Publication, report: Option<OwnerReport>) -> AppResult<()> {
        if let Some(report) = report {
            self.mirror(p, &report).await?;
        }
        self.app
            .transition(p.id(), |_| PublicationEvent::ReturnToDesktop {
                execution: Execution::IDLE,
            })
            .await?;
        self.app.set_cloud_note(p.id(), None);
        Ok(())
    }

    async fn sent(&self) -> AppResult<HashMap<AccountId, SentCredential>> {
        Ok(self
            .app
            .store
            .settings()
            .get(ACCOUNTS_KEY)
            .await?
            .unwrap_or_default())
    }

    async fn record_sent(&self, account: AccountId, sent: SentCredential) -> AppResult<()> {
        let mut all = self.sent().await?;
        all.insert(account, sent);
        self.app.store.settings().set(ACCOUNTS_KEY, &all).await?;
        Ok(())
    }

    /// Sends the account and its credential. With `always` false, only when the credential
    /// changed since it was last sent. Pulls the runner's copy first if it rotated it.
    async fn push_account(
        &self,
        client: &CloudClient,
        account: &ConnectedAccount,
        always: bool,
    ) -> AppResult<()> {
        for _ in 0..2 {
            let credential = export(self.credentials.as_ref(), account)
                .map_err(credential_error)?
                .ok_or_else(|| {
                    AppError::Invalid(format!(
                        "{} isn't signed in on this computer",
                        account.display_name
                    ))
                })?;
            let digest = credential.digest();
            let sent = self.sent().await?.remove(&account.id);
            if !always && sent.as_ref().is_some_and(|s| s.digest == digest) {
                return Ok(());
            }
            let put = AccountPut {
                account: account.clone(),
                credential,
                base_revision: sent.map(|s| s.revision),
            };
            match client.put_account(&put).await {
                Ok(status) => {
                    return self
                        .record_sent(
                            account.id,
                            SentCredential {
                                revision: status.credential_revision,
                                digest,
                            },
                        )
                        .await;
                }
                Err(ClientError::Api(e)) if e.code == ErrorCode::CredentialConflict => {
                    self.pull_credential(client, account).await?;
                }
                Err(e) => return Err(e.into()),
            }
        }
        Err(AppError::Invalid(
            "the cloud runner keeps changing this account's credential; try again".into(),
        ))
    }

    async fn pull_credential(
        &self,
        client: &CloudClient,
        account: &ConnectedAccount,
    ) -> AppResult<()> {
        let copy = client.credential(account.id).await?;
        import(self.credentials.as_ref(), account, &copy.credential).map_err(credential_error)?;
        info!(account = %account.id, revision = copy.revision, "took a credential the cloud runner refreshed");
        self.record_sent(
            account.id,
            SentCredential {
                revision: copy.revision,
                digest: copy.credential.digest(),
            },
        )
        .await
    }

    /// Credentials the runner refreshed (e.g. a rotated refresh token) replace this
    /// computer's, which may no longer work.
    async fn pull_credentials(&self, client: &CloudClient) -> AppResult<()> {
        let sent = self.sent().await?;
        for status in client.accounts().await? {
            let known = sent.get(&status.account_id).map_or(0, |s| s.revision);
            if status.updated_by != CredentialSource::Runner || status.credential_revision <= known
            {
                continue;
            }
            let Some(account) = self.app.store.accounts().get(status.account_id).await? else {
                continue;
            };
            if let Err(e) = self.pull_credential(client, &account).await {
                warn!(account = %account.id, error = %e, "could not take a refreshed credential");
            }
        }
        Ok(())
    }

    /// After a reconnect on this computer, the runner's publications waiting for it get the
    /// new credential.
    async fn push_reconnected(&self, client: &CloudClient) -> AppResult<()> {
        let mut seen = Vec::new();
        for p in self.held().await? {
            let account = p.content().destination.account_id;
            if p.state() != PublicationState::NeedsAuthentication || seen.contains(&account) {
                continue;
            }
            seen.push(account);
            let account = self.app.store.accounts().require(account).await?;
            if let Err(e) = self.push_account(client, &account, false).await {
                warn!(account = %account.id, error = %e, "could not send a new credential");
            }
        }
        Ok(())
    }
}
