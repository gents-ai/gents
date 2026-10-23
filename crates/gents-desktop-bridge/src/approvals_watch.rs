use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use gents::plugin::approval;
use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;

pub(crate) const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);

pub(crate) struct PromptSubscriptions {
    windows: BTreeMap<String, BTreeSet<String>>,
    active: watch::Sender<bool>,
}

impl Default for PromptSubscriptions {
    fn default() -> Self {
        Self {
            windows: BTreeMap::new(),
            active: watch::channel(false).0,
        }
    }
}

impl PromptSubscriptions {
    pub(crate) fn changes(&self) -> watch::Receiver<bool> {
        self.active.subscribe()
    }

    pub(crate) fn contains_window(&self, window: &str) -> bool {
        self.windows.contains_key(window)
    }

    pub(crate) fn contains_token(&self, window: &str, token: &str) -> bool {
        self.windows
            .get(window)
            .is_some_and(|tokens| tokens.contains(token))
    }

    pub(crate) fn token_count(&self) -> usize {
        self.windows.values().map(BTreeSet::len).sum()
    }

    pub(crate) fn subscribe(&mut self, window: &str, token: String) {
        self.windows
            .entry(window.to_owned())
            .or_default()
            .insert(token);
        self.update();
    }

    pub(crate) fn unsubscribe(&mut self, window: &str, token: &str) {
        if let Some(tokens) = self.windows.get_mut(window) {
            tokens.remove(token);
            if tokens.is_empty() {
                self.windows.remove(window);
            }
        }
        self.update();
    }

    pub(crate) fn remove_window(&mut self, window: &str) {
        self.windows.remove(window);
        self.update();
    }

    fn update(&self) {
        let active = !self.windows.is_empty();
        self.active.send_if_modified(|current| {
            let changed = *current != active;
            *current = active;
            changed
        });
    }
}

#[derive(Default)]
struct PendingQueue {
    seen: BTreeSet<String>,
    expires_at: Option<Instant>,
}

impl PendingQueue {
    fn refresh(&mut self, home: &Path) -> Result<bool> {
        let requests = approval::pending_requests(home)?;
        let now_ms = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
        self.expires_at = requests
            .iter()
            .filter_map(|request| {
                let expires = u128::from(request.expires_at_ms);
                let remaining = expires.saturating_sub(now_ms).saturating_add(1);
                Instant::now().checked_add(Duration::from_millis(
                    remaining.min(u128::from(u64::MAX)) as u64,
                ))
            })
            .min();
        let current = requests.into_iter().map(|request| request.id).collect();
        let changed = current != self.seen;
        self.seen = current;
        Ok(changed)
    }
}

fn changes_queue(event: &Event, queue: &Path) -> bool {
    if event.need_rescan() {
        return true;
    }
    if matches!(event.kind, EventKind::Access(_)) {
        return false;
    }
    // Kqueue may report only one transient entry for a coalesced directory write.
    event.paths.iter().any(|path| {
        path == queue
            || Some(path.as_path()) == queue.parent()
            || (path.parent() == Some(queue)
                && path.file_name().is_some_and(|name| name != ".listening"))
    })
}

/// notify 8.2 purges device event history when an FSEvents stream stops;
/// macOS uses kqueue to keep approval watcher lifetimes independent.
/// Its nonrecursive directory watches can grow recursively after changes, so
/// watch roots must stay inside the approval tree to bound descriptor use.
/// Permission preferences use a separate watch restricted to a regular file.
struct QueueWatch {
    watcher: RecommendedWatcher,
    queues: [PathBuf; 2],
    preferences: PathBuf,
}

impl QueueWatch {
    fn start(home: &Path, changes: mpsc::Sender<()>) -> Result<Self> {
        approval::prepare_listener(home)?;
        let parent = home.canonicalize().context("resolving the approval home")?;
        let queues = approval::queue_dirs(&parent);
        let watched_queues = queues.clone();
        let preferences = approval::workspace::preferences_file(&parent);
        let watched_preferences = preferences.clone();
        let mut watcher = notify::recommended_watcher(move |event: notify::Result<Event>| {
            let changed = match event {
                Ok(event) => {
                    watched_queues
                        .iter()
                        .any(|queue| changes_queue(&event, queue))
                        || (!matches!(event.kind, EventKind::Access(_))
                            && event.paths.iter().any(|path| path == &watched_preferences))
                }
                Err(error) => {
                    tracing::warn!(%error, "plugin approval filesystem watch failed");
                    true
                }
            };
            if changed {
                let _ = changes.try_send(());
            }
        })?;
        // Keep watching the parent across queue removal or atomic replacement.
        watcher.watch(
            queues[0].parent().context("approval queue has no parent")?,
            RecursiveMode::NonRecursive,
        )?;
        for queue in &queues {
            watcher.watch(queue, RecursiveMode::Recursive)?;
        }
        let mut watch = Self {
            watcher,
            queues,
            preferences,
        };
        watch.watch_preferences()?;
        Ok(watch)
    }

    fn watch_preferences(&mut self) -> Result<()> {
        match std::fs::symlink_metadata(&self.preferences) {
            Ok(metadata) => {
                anyhow::ensure!(
                    metadata.file_type().is_file(),
                    "workspace permission preferences must be a regular file"
                );
                self.watcher
                    .watch(&self.preferences, RecursiveMode::NonRecursive)?;
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(error).context("inspecting workspace permission preferences"),
        }
        Ok(())
    }

    fn reattach(&mut self, home: &Path) -> Result<()> {
        approval::prepare_listener(home)?;
        let root = self.queues[0]
            .parent()
            .context("approval queue has no parent")?;
        let _ = self.watcher.unwatch(root);
        self.watcher.watch(root, RecursiveMode::NonRecursive)?;
        for queue in &self.queues {
            let _ = self.watcher.unwatch(queue);
            self.watcher.watch(queue, RecursiveMode::Recursive)?;
        }
        let _ = self.watcher.unwatch(&self.preferences);
        self.watch_preferences()?;
        Ok(())
    }
}

fn renew_listener(home: &Path, subscriptions: &watch::Receiver<bool>) -> Result<bool> {
    let active = subscriptions.borrow();
    if !*active {
        return Ok(false);
    }
    approval::mark_listening(home)?;
    Ok(true)
}

async fn until_expiry(expiry: Option<Instant>) {
    match expiry {
        Some(expiry) => tokio::time::sleep_until(expiry).await,
        None => std::future::pending().await,
    }
}

pub(crate) async fn watch_queue(
    home: PathBuf,
    heartbeat_interval: Duration,
    mut subscriptions: watch::Receiver<bool>,
    mut observed: impl FnMut(bool) + Send,
) {
    let (send, mut receive) = mpsc::channel(1);
    let mut heartbeat = tokio::time::interval(heartbeat_interval);
    heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut watcher = None;
    let mut pending = PendingQueue::default();
    loop {
        tokio::select! {
            changed = subscriptions.changed() => {
                if changed.is_err() {
                    return;
                }
                if !*subscriptions.borrow_and_update() {
                    watcher = None;
                    pending.expires_at = None;
                } else {
                    heartbeat.reset_immediately();
                }
            }
            _ = heartbeat.tick() => {
                if !*subscriptions.borrow() {
                    continue;
                }
                if watcher.is_none() {
                    match QueueWatch::start(&home, send.clone()) {
                        Ok(watch) => {
                            if let Err(error) = approval::pending_requests(&home) {
                                tracing::warn!(%error, "validating the plugin approval queue");
                                continue;
                            }
                            watcher = Some(watch);
                            let _ = send.try_send(());
                        }
                        Err(error) => {
                            tracing::warn!(%error, "starting plugin approval filesystem watch");
                            continue;
                        }
                    }
                }
                if let Err(error) = renew_listener(&home, &subscriptions) {
                    tracing::warn!(%error, "refreshing the plugin approval listener");
                    watcher = None;
                }
            }
            _ = receive.recv() => {
                if !*subscriptions.borrow() {
                    continue;
                }
                let Some(watch) = watcher.as_mut() else {
                    continue;
                };
                if let Err(error) = watch.reattach(&home) {
                    tracing::warn!(%error, "restoring plugin approval filesystem watch");
                    watcher = None;
                    continue;
                }
                match pending.refresh(&home) {
                    Ok(changed) => {
                        match renew_listener(&home, &subscriptions) {
                            Ok(true) => observed(changed),
                            Ok(false) => {}
                            Err(error) => {
                                tracing::warn!(%error, "refreshing the plugin approval listener");
                                watcher = None;
                            }
                        }
                    }
                    Err(error) => {
                        watcher = None;
                        tracing::warn!(%error, "reading changed plugin approvals");
                    }
                }
            }
            _ = until_expiry(pending.expires_at) => {
                if !*subscriptions.borrow() {
                    pending.expires_at = None;
                    continue;
                }
                match pending.refresh(&home) {
                    Ok(changed) => observed(changed),
                    Err(error) => {
                        pending.expires_at = None;
                        tracing::warn!(%error, "expiring plugin approval requests");
                    }
                }
            }
        }
    }
}

#[cfg(feature = "tauri-host")]
pub fn spawn_plugin_approvals_watch<R>(
    app: tauri::AppHandle<R>,
    home: PathBuf,
    subscriptions: watch::Receiver<bool>,
) -> tauri::async_runtime::JoinHandle<()>
where
    R: tauri::Runtime,
{
    use tauri::Emitter;
    tauri::async_runtime::spawn(watch_queue(
        home,
        HEARTBEAT_INTERVAL,
        subscriptions,
        move |changed| {
            if changed {
                let _ = app.emit(crate::contract::PLUGIN_APPROVALS_CHANGED_EVENT, ());
            }
        },
    ))
}

#[cfg(test)]
#[path = "approvals_watch_tests.rs"]
mod tests;
