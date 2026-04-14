use std::sync::{Arc, RwLock};
use std::time::Duration;

use defra_node::{EmbeddedNode, EventName};
use tokio::sync::watch;

use super::query::load_full_snapshot;
use super::store::{ClientStore, SharedClientStore};

const OBSERVER_DEBOUNCE: Duration = Duration::from_millis(150);

pub struct ObservedStore {
    snapshot: RwLock<SharedClientStore>,
    focused_request_id: RwLock<Option<String>>,
    version_tx: watch::Sender<u64>,
}

impl ObservedStore {
    pub fn new(initial_snapshot: ClientStore) -> (Arc<Self>, watch::Receiver<u64>) {
        let (version_tx, version_rx) = watch::channel(1_u64);
        let store = Arc::new(Self {
            snapshot: RwLock::new(Arc::new(initial_snapshot)),
            focused_request_id: RwLock::new(None),
            version_tx,
        });
        (store, version_rx)
    }

    pub fn snapshot(&self) -> SharedClientStore {
        self.snapshot
            .read()
            .expect("store snapshot lock poisoned")
            .clone()
    }

    pub fn subscribe(&self) -> watch::Receiver<u64> {
        self.version_tx.subscribe()
    }

    pub fn focused_request_id(&self) -> Option<String> {
        self.focused_request_id
            .read()
            .expect("focused request lock poisoned")
            .clone()
    }

    pub fn set_focused_request_id(&self, request_id: Option<String>) {
        *self
            .focused_request_id
            .write()
            .expect("focused request lock poisoned") = request_id;
    }

    pub fn replace_snapshot(&self, snapshot: ClientStore) -> u64 {
        *self.snapshot.write().expect("store snapshot lock poisoned") = Arc::new(snapshot);

        let next_version = self.version_tx.borrow().saturating_add(1);
        self.version_tx.send_replace(next_version);
        next_version
    }
}

pub struct ObserverHandle {
    stop_tx: watch::Sender<bool>,
    task: tokio::task::JoinHandle<()>,
}

impl ObserverHandle {
    pub async fn shutdown(self) {
        let _ = self.stop_tx.send(true);
        let _ = self.task.await;
    }
}

pub fn spawn_observer(
    node: Arc<EmbeddedNode>,
    store: Arc<ObservedStore>,
) -> ObserverHandle {
    let (stop_tx, mut stop_rx) = watch::channel(false);
    let task = tokio::spawn(async move {
        let mut subscription = node.subscribe(&[EventName::Update]);

        loop {
            let next_message = tokio::select! {
                changed = stop_rx.changed() => match changed {
                    Ok(()) if *stop_rx.borrow() => {
                        tracing::debug!("desktop observation requested shutdown");
                        break;
                    }
                    Ok(()) => continue,
                    Err(_) => break,
                },
                message = subscription.recv() => message,
            };

            let Some(_message) = next_message else {
                tracing::debug!("desktop observation subscription closed");
                break;
            };

            tokio::time::sleep(OBSERVER_DEBOUNCE).await;
            while subscription.try_recv().is_ok() {}

            let dropped = subscription.check_and_reset_dropped();
            if dropped > 0 {
                tracing::warn!(dropped, "desktop observation subscription dropped messages");
            }

            match load_full_snapshot(node.as_ref()).await {
                Ok(snapshot) => {
                    let version = store.replace_snapshot(snapshot);
                    tracing::trace!(version, "desktop observation snapshot refreshed");
                }
                Err(error) => {
                    tracing::error!(error = %error, "failed to refresh desktop observation snapshot");
                }
            }
        }
    });

    ObserverHandle { stop_tx, task }
}
