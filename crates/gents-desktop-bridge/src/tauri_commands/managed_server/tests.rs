use super::*;
use super::{
    authority::*, launch::*, pairing::*, port::*, preference::*, readiness::*, reset::*,
    start_wait::*, status::*,
};
use crate::local_pairing::{
    await_managed_pairing_approval, enrollment_request_is_not_visible_yet, resolve_managed_request,
    select_managed_request, PairingApproval,
};
use crate::local_pairing::{
    observe_managed_runtime_schema, retry_managed_pairing, ManagedPairingTarget, PairingFailure,
};
use crate::state::ManagedServerState as ManagedServerRuntimeState;
use crate::types::{ManagedServerState, ManagedServerToolCeiling};
use gents_desktop_core::client::{ClientCore, EnrollmentRequestResult};
use gents_desktop_core::home_reset::*;
use gents_desktop_core::preference::{ReviewedHome, MANAGED_SERVER_CONFIG};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

mod authority;
mod crash_loop;
mod managed_schema;
mod orchestration;
mod pairing;
mod readiness;
mod reset;
mod reset_store;
mod status;

fn not_the_user_home() -> &'static Path {
    Path::new("/nonexistent-user-home")
}

fn write(path: &Path, contents: &str) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, contents).unwrap();
}

fn lineage_refusal(data_path: PathBuf) -> gents::storage_backend::IncompatibleStore {
    gents::storage_backend::IncompatibleStore {
        kind: gents::storage_backend::IncompatibleStoreKind::UnknownLineage,
        data_path,
    }
}

fn dirty_home(temp: &Path) -> (PathBuf, gents_desktop_core::client::DesktopPaths) {
    let home = temp.join(".gents");
    write(&home.join("data/MANIFEST"), "REGOMAN old lineage");
    write(
        &home.join("init.json"),
        r#"{"node_did":"did:key:old","node_name":"local"}"#,
    );
    write(&home.join("keys/local.key"), "identity");
    write(&home.join("runtime.json"), "{}");
    write(&home.join("p2p-secret-key"), "p2p");
    write(
        &home.join("grok-port-home/init.json"),
        r#"{"node_did":"did:key:other"}"#,
    );
    write(&home.join("grok-port-home/data/MANIFEST"), "REGOMAN other");
    let desktop = gents_desktop_core::client::DesktopPaths::from_root(temp.join("desktop"));
    write(
        &desktop.node_data_dir().join("MANIFEST"),
        "REGOMAN old client",
    );
    write(desktop.peer_directory_path(), "{}");
    write(
        desktop.principal_metadata_path(),
        r#"{"did":"did:key:z6MkClient","public_key_bytes":[1,2,3]}"#,
    );
    write(desktop.identity_key_path(), "principal");
    write(desktop.iroh_secret_key_path(), "iroh");
    write(&desktop.root().join(MANAGED_SERVER_CONFIG), "{}");
    write(&desktop.root().join("runtime/gents"), "packaged runtime");
    (std::fs::canonicalize(&home).unwrap(), desktop)
}

fn ready_status(node_did: &str) -> ManagedServerStatus {
    ManagedServerStatus {
        state: ManagedServerState::External,
        auto_start: false,
        node_name: Some("local".to_string()),
        node_did: Some(node_did.to_string()),
        graphql: Some("http://127.0.0.1:9191/graphql".to_string()),
        effective_tool_ceiling: Some(ManagedServerToolCeiling::MetaOnly),
        effective_tool_root: None,
        suggested_tool_root: None,
        pairing_ready: false,
        approval_required: false,
        runtime_booting: false,
        error: None,
        error_code: None,
    }
}

fn service_exit(restarts: u64) -> gents_server::native_service::ServiceExit {
    gents_server::native_service::ServiceExit {
        reason: "exited with code 78".to_string(),
        restarts,
        clean: false,
        code: Some(78),
    }
}

fn orchestration_state() -> (tempfile::TempDir, DesktopAppState) {
    use crate::config::{
        AppMeta, BootstrapPolicy, BridgeConfig, HomePolicy, ManagedServerPolicy, NodeHomePolicy,
    };
    use crate::snapshot::projection::SnapshotGrants;
    use crate::state::resolve_policy;

    let temp = tempfile::tempdir().expect("temporary bridge roots");
    let policy = resolve_policy(
        &BridgeConfig {
            home: HomePolicy::FixedRoot(temp.path().join("desktop")),
            bootstrap: BootstrapPolicy::LocalRuntimeAllowed {
                node_home: NodeHomePolicy::Fixed(temp.path().join("agent")),
            },
            app_meta: AppMeta {
                app_name: "managed-start-test".to_string(),
                app_version: env!("CARGO_PKG_VERSION").to_string(),
            },
            snapshot_grants: SnapshotGrants::core_only(),
            managed_server: ManagedServerPolicy::Allowed,
            store_key_custody: gents::store_key::StoreKeyCustodyChoice::File,
            client_host: false,
        },
        None,
    )
    .expect("fixed bridge policy");
    (temp, DesktopAppState::new(policy))
}

#[derive(Default)]
struct FakeLaunch {
    approval: std::sync::Mutex<std::collections::VecDeque<Result<bool, BridgeError>>>,
    approval_blocks: bool,
    approval_gated: bool,
    approval_release: tokio::sync::Notify,
    starts: std::sync::Mutex<std::collections::VecDeque<Result<Launched, BridgeError>>>,
    readiness: std::sync::Mutex<std::collections::VecDeque<Readiness>>,
    start_calls: std::sync::atomic::AtomicUsize,
    approval_waits: std::sync::atomic::AtomicUsize,
    waiting: tokio::sync::Notify,
    still_booting: bool,
}

impl ManagedLaunch for FakeLaunch {
    async fn requires_approval(&self) -> Result<bool, BridgeError> {
        self.approval
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Ok(false))
    }

    async fn await_approval(&self) -> anyhow::Result<()> {
        self.approval_waits
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.waiting.notify_one();
        if self.approval_blocks {
            std::future::pending::<()>().await;
        }
        if self.approval_gated {
            self.approval_release.notified().await;
        }
        Ok(())
    }

    async fn start(&self) -> Result<Launched, BridgeError> {
        self.start_calls
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.starts
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Ok(Launched::Started))
    }

    async fn await_ready(&self) -> anyhow::Result<Readiness> {
        let next = self.readiness.lock().unwrap().pop_front();
        match next {
            Some(readiness) => Ok(readiness),
            None if self.still_booting => Err(RuntimeStillBooting {
                waited: Duration::from_secs(300),
            }
            .into()),
            None => {
                self.waiting.notify_one();
                std::future::pending().await
            }
        }
    }
}

impl FakeLaunch {
    fn starts(&self) -> usize {
        self.start_calls.load(std::sync::atomic::Ordering::SeqCst)
    }
}
