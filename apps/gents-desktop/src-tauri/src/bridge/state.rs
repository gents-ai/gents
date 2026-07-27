use std::sync::Arc;
use std::sync::Mutex;

use gents_desktop_core::client::ClientCore;
use tauri::async_runtime::{spawn, JoinHandle};
use tauri::{AppHandle, Emitter, State};

use gents_desktop_bridge::types::ClientUpdateEvent;

#[derive(Default)]
pub(crate) struct DesktopAppState {
    pub bridge: Mutex<DesktopBridge>,
}

#[derive(Default)]
pub(crate) struct DesktopBridge {
    pub core: Option<Arc<ClientCore>>,
    pub updates_task: Option<JoinHandle<()>>,
}

pub(crate) fn spawn_client_update_task(app: AppHandle, core: Arc<ClientCore>) -> JoinHandle<()> {
    spawn(async move {
        let mut store_updates = core.store_updates();
        let mut health_updates = core.p2p_health_updates();

        loop {
            tokio::select! {
                changed = store_updates.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    let _ = app.emit("desktop://client-updated", ClientUpdateEvent { reason: "store" });
                }
                changed = health_updates.changed() => {
                    if changed.is_err() {
                        break;
                    }
                    let _ = app.emit("desktop://client-updated", ClientUpdateEvent { reason: "health" });
                }
            }
        }
    })
}

pub(crate) fn current_core(state: &State<'_, DesktopAppState>) -> Option<Arc<ClientCore>> {
    state
        .bridge
        .lock()
        .expect("desktop bridge lock poisoned")
        .core
        .clone()
}
