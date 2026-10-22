use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock, RwLock};

use chrono::{DateTime, Utc};
use shorts_adapters::{AdapterRegistry, ErasedAdapter, ManualHandoff};
use shorts_domain::{JobKind, Platform, PublicationId};
use shorts_scheduler::{Clock, Worker, WorkerConfig};
use shorts_store::Store;
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::error::AppResult;
use crate::events::AppEvent;
use crate::media::MediaBackend;
use crate::paths::AppPaths;
use crate::pipeline::{DesktopHost, ExportHandler, RenderHandler};

/// The application core used by the desktop shell.
pub struct App {
    pub(crate) store: Store,
    pub(crate) media: Arc<dyn MediaBackend>,
    pub(crate) paths: AppPaths,
    adapters: RwLock<AdapterRegistry>,
    pub(crate) clock: Arc<dyn Clock>,
    events: broadcast::Sender<AppEvent>,
    worker: OnceLock<Arc<Worker>>,
    /// Problems reaching the cloud runner, per cloud-owned publication (set by the sync).
    cloud_notes: Mutex<HashMap<PublicationId, String>>,
    /// TikTok posts the owner asked to download ("Get original"), by video id.
    pub(crate) expected_originals: Mutex<HashMap<String, DateTime<Utc>>>,
    /// Why a download wasn't taken for a requested post, e.g. a different length, by video id.
    pub(crate) download_notes: Mutex<HashMap<String, String>>,
}

impl App {
    /// Opens (creating if needed) the library under `paths`.
    pub async fn open(
        paths: AppPaths,
        media: Arc<dyn MediaBackend>,
        adapters: AdapterRegistry,
        clock: Arc<dyn Clock>,
    ) -> AppResult<Arc<Self>> {
        paths.create_all()?;
        let store = Store::open(&paths.database).await?;
        let (events, _) = broadcast::channel(256);
        Ok(Arc::new(Self {
            store,
            media,
            paths,
            adapters: RwLock::new(adapters),
            clock,
            events,
            worker: OnceLock::new(),
            cloud_notes: Mutex::new(HashMap::new()),
            expected_originals: Mutex::new(HashMap::new()),
            download_notes: Mutex::new(HashMap::new()),
        }))
    }

    pub fn store(&self) -> &Store {
        &self.store
    }

    pub fn paths(&self) -> &AppPaths {
        &self.paths
    }

    /// Adds (or replaces) the adapter for its platform, e.g. once a connector is set up.
    pub fn register_adapter(&self, adapter: Arc<dyn ErasedAdapter>) {
        self.adapters
            .write()
            .expect("adapters lock")
            .register(adapter);
    }

    pub(crate) fn adapter(&self, platform: Platform) -> Option<Arc<dyn ErasedAdapter>> {
        self.adapters.read().expect("adapters lock").get(platform)
    }

    pub(crate) fn clock(&self) -> Arc<dyn Clock> {
        self.clock.clone()
    }

    pub fn subscribe(&self) -> broadcast::Receiver<AppEvent> {
        self.events.subscribe()
    }

    pub(crate) fn emit(&self, event: AppEvent) {
        // No subscribers is fine (e.g. in tests).
        let _ = self.events.send(event);
    }

    pub(crate) fn now(&self) -> DateTime<Utc> {
        self.clock.now()
    }

    pub(crate) async fn handoff(&self) -> AppResult<ManualHandoff> {
        let prefs = self.preferences().await?;
        let root = prefs
            .export_dir
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| self.paths.exports.clone());
        Ok(ManualHandoff::new(root))
    }

    /// Builds the durable worker with this app's job handlers. `config.overdue` is replaced by
    /// the saved preference.
    pub async fn worker(self: &Arc<Self>, mut config: WorkerConfig) -> AppResult<Arc<Worker>> {
        if let Some(worker) = self.worker.get() {
            return Ok(worker.clone());
        }
        config.overdue = self.preferences().await?.overdue_policy;
        let mut worker = Worker::new(self.store.clone(), config, self.clock.clone());
        let weak = Arc::downgrade(self);
        worker.register(
            JobKind::RenderVariant,
            Arc::new(RenderHandler::new(weak.clone())),
        );
        worker.register(
            JobKind::PublishPublication,
            Arc::new(shorts_publish::PublishHandler::new(Arc::new(
                DesktopHost::new(weak.clone()),
            ))),
        );
        worker.register(JobKind::ExportPackage, Arc::new(ExportHandler::new(weak)));
        let worker = Arc::new(worker);
        Ok(self.worker.get_or_init(|| worker).clone())
    }

    /// Starts the worker loop (with startup recovery) until `shutdown` fires.
    pub async fn start_worker(
        self: &Arc<Self>,
        config: WorkerConfig,
        shutdown: CancellationToken,
    ) -> AppResult<tokio::task::JoinHandle<()>> {
        let worker = self.worker(config).await?;
        Ok(tokio::spawn(worker.run(shutdown)))
    }

    /// Tells a running worker to look at the queue now.
    pub(crate) fn poke(&self) {
        if let Some(worker) = self.worker.get() {
            worker.waker().notify_one();
        }
        self.emit(AppEvent::JobsChanged);
    }

    pub(crate) fn running_worker(&self) -> Option<&Arc<Worker>> {
        self.worker.get()
    }

    pub(crate) fn cloud_note(&self, id: PublicationId) -> Option<String> {
        self.cloud_notes
            .lock()
            .expect("cloud notes")
            .get(&id)
            .cloned()
    }

    pub(crate) fn set_cloud_note(&self, id: PublicationId, note: Option<String>) {
        let mut notes = self.cloud_notes.lock().expect("cloud notes");
        let changed = match note {
            Some(n) => notes.insert(id, n.clone()).as_ref() != Some(&n),
            None => notes.remove(&id).is_some(),
        };
        drop(notes);
        if changed {
            self.emit(AppEvent::PublicationChanged { id: id.to_string() });
        }
    }
}
