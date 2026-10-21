//! Watch folder: imports videos that appear in a folder (e.g. one the phone or editing tool
//! syncs to) once they have finished being written. File events are backed by a periodic
//! rescan, since synced folders don't always report changes, and the last check is recorded so
//! the UI can show "last checked … · Check now".

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, UNIX_EPOCH};

use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, warn};

use crate::app::App;
use crate::error::{AppError, AppResult};
use crate::events::AppEvent;
use crate::library::is_video_file;

const SEEN_KEY: &str = "watch_seen";

#[derive(Debug, Clone, Copy)]
pub struct WatchConfig {
    /// A file must keep the same size for this long before it is imported.
    pub settle: Duration,
    pub poll: Duration,
    /// How often the whole folder is listed again.
    pub rescan: Duration,
}

impl Default for WatchConfig {
    fn default() -> Self {
        Self {
            settle: Duration::from_secs(3),
            poll: Duration::from_millis(500),
            rescan: Duration::from_secs(10 * 60),
        }
    }
}

/// Files already handled, so restarts don't re-hash the whole folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
struct Seen {
    size: u64,
    modified: u64,
}

/// A running watch. Dropping it stops watching.
pub struct WatchFolder {
    dir: PathBuf,
    scan: mpsc::UnboundedSender<oneshot::Sender<()>>,
    _watcher: RecommendedWatcher,
    task: tokio::task::JoinHandle<()>,
}

impl Drop for WatchFolder {
    fn drop(&mut self) {
        self.task.abort();
    }
}

impl WatchFolder {
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Lists the folder now. The request is sent immediately; the returned future (which
    /// doesn't borrow the watch) resolves once new files are queued for import.
    pub fn check_now(&self) -> impl std::future::Future<Output = AppResult<()>> + Send + 'static {
        let (done, wait) = oneshot::channel();
        let sent = self.scan.send(done).is_ok();
        async move {
            let stopped = || AppError::Invalid("the watch folder stopped".into());
            if !sent {
                return Err(stopped());
            }
            wait.await.map_err(|_| stopped())
        }
    }

    /// Starts watching `dir` (not recursively). Existing files are considered too.
    pub fn start(app: Arc<App>, dir: PathBuf, config: WatchConfig) -> AppResult<Self> {
        if !dir.is_dir() {
            return Err(AppError::Invalid(format!(
                "{} is not a folder",
                dir.display()
            )));
        }
        let (tx, rx) = mpsc::unbounded_channel::<PathBuf>();
        let notify_tx = tx.clone();
        let mut watcher =
            notify::recommended_watcher(move |res: notify::Result<notify::Event>| match res {
                Ok(event) => {
                    if matches!(
                        event.kind,
                        EventKind::Create(_) | EventKind::Modify(_) | EventKind::Any
                    ) {
                        for path in event.paths {
                            let _ = notify_tx.send(path);
                        }
                    }
                }
                Err(e) => warn!(error = %e, "watch folder error"),
            })
            .map_err(|e| AppError::Invalid(format!("cannot watch {}: {e}", dir.display())))?;
        watcher
            .watch(&dir, RecursiveMode::NonRecursive)
            .map_err(|e| AppError::Invalid(format!("cannot watch {}: {e}", dir.display())))?;

        drop(tx);
        let (scan, scans) = mpsc::unbounded_channel();
        // The rescan timer fires immediately, so existing files are considered too.
        let task = tokio::spawn(settle_loop(app, dir.clone(), rx, scans, config));
        Ok(Self {
            dir,
            scan,
            _watcher: watcher,
            task,
        })
    }
}

fn candidate(path: &Path) -> bool {
    let hidden = path
        .file_name()
        .and_then(|n| n.to_str())
        .is_none_or(|n| n.starts_with('.') || n.starts_with('~'));
    !hidden && is_video_file(path)
}

fn fingerprint(path: &Path) -> Option<Seen> {
    let meta = std::fs::metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    let modified = meta
        .modified()
        .ok()
        .and_then(|m| m.duration_since(UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_secs());
    Some(Seen {
        size: meta.len(),
        modified,
    })
}

struct Pending {
    size: u64,
    since: Instant,
}

fn list(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .map(|entries| entries.flatten().map(|e| e.path()).collect())
        .unwrap_or_default()
}

fn consider(path: PathBuf, seen: &BTreeMap<String, Seen>, pending: &mut HashMap<PathBuf, Pending>) {
    if !candidate(&path) {
        return;
    }
    let Some(fp) = fingerprint(&path) else { return };
    if seen.get(&path.to_string_lossy().into_owned()) == Some(&fp) {
        return;
    }
    let entry = pending.entry(path).or_insert(Pending {
        size: fp.size,
        since: Instant::now(),
    });
    if entry.size != fp.size {
        entry.size = fp.size;
        entry.since = Instant::now();
    }
}

async fn settle_loop(
    app: Arc<App>,
    dir: PathBuf,
    mut rx: mpsc::UnboundedReceiver<PathBuf>,
    mut scans: mpsc::UnboundedReceiver<oneshot::Sender<()>>,
    config: WatchConfig,
) {
    let mut seen: BTreeMap<String, Seen> = app
        .store
        .settings()
        .get(SEEN_KEY)
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    let mut pending: HashMap<PathBuf, Pending> = HashMap::new();
    let mut ticker = tokio::time::interval(config.poll);
    let mut rescan = tokio::time::interval(config.rescan);
    loop {
        tokio::select! {
            path = rx.recv() => {
                let Some(path) = path else { return };
                consider(path, &seen, &mut pending);
            }
            _ = rescan.tick() => {
                for path in list(&dir) {
                    consider(path, &seen, &mut pending);
                }
                app.record_watch_check().await;
                // Items that waited for their TikTok caption may be due now.
                app.apply_deferred_rules().await;
            }
            done = scans.recv() => {
                let Some(done) = done else { continue };
                for path in list(&dir) {
                    consider(path, &seen, &mut pending);
                }
                app.record_watch_check().await;
                app.apply_deferred_rules().await;
                let _ = done.send(());
            }
            _ = ticker.tick() => {
                let mut ready = Vec::new();
                pending.retain(|path, p| match fingerprint(path) {
                    None => false,
                    Some(fp) if fp.size != p.size => {
                        p.size = fp.size;
                        p.since = Instant::now();
                        true
                    }
                    Some(_) if p.size > 0 && p.since.elapsed() >= config.settle => {
                        ready.push(path.clone());
                        false
                    }
                    Some(_) => true,
                });
                for path in ready {
                    import(&app, &path, &mut seen).await;
                }
            }
        }
    }
}

async fn import(app: &App, path: &Path, seen: &mut BTreeMap<String, Seen>) {
    debug!(path = %path.display(), "watch folder import");
    let imported = match app.import_file(path, None).await {
        Ok(outcome) => {
            if let crate::ImportOutcome::Duplicate { asset_id } = outcome {
                app.emit(AppEvent::Imported {
                    asset_id: asset_id.to_string(),
                    duplicate: true,
                });
            }
            Some(outcome.asset_id())
        }
        Err(e) => {
            warn!(path = %path.display(), error = %e, "watch folder import failed");
            app.emit(AppEvent::ImportFailed {
                path: path.to_string_lossy().into_owned(),
                message: e.to_string(),
            });
            // Transient problems (locked file, I/O) are retried on the next change or start.
            if !matches!(e, AppError::Invalid(_)) {
                return;
            }
            None
        }
    };
    if let Some(fp) = fingerprint(path) {
        seen.insert(path.to_string_lossy().into_owned(), fp);
        if let Err(e) = app.store.settings().set(SEEN_KEY, &*seen).await {
            warn!(error = %e, "could not record watch folder state");
        }
    }
    // Routing comes after the file is recorded as handled; it is idempotent, so a crash in
    // between at worst routes the same asset again on the next start.
    if let Some(asset_id) = imported {
        app.route_watched(asset_id).await;
        app.suggest_matches_for(asset_id).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn partial_and_hidden_files_are_ignored() {
        assert!(candidate(Path::new("/w/clip.mp4")));
        assert!(!candidate(Path::new("/w/.clip.mp4")));
        assert!(!candidate(Path::new("/w/~clip.mp4")));
        assert!(!candidate(Path::new("/w/clip.mp4.crdownload")));
        assert!(!candidate(Path::new("/w/clip.mp4.partial")));
        assert!(!candidate(Path::new("/w/notes.txt")));
    }
}
