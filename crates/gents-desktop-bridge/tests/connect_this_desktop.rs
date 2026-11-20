use std::collections::BTreeSet;
use std::future::Future;
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use clap::Parser as _;
use gents_desktop_bridge::client_runtime::{ClientRuntime, ClientRuntimeOptions};
use gents_desktop_bridge::resolve_policy;
use gents_desktop_bridge::state::{current_core, DesktopAppState};
use gents_desktop_bridge::tauri_commands::{lifecycle, managed_server};
use gents_desktop_bridge::BootstrapPolicy;
use gents_desktop_bridge::BridgeConfig;
use gents_desktop_bridge::HomePolicy;
use gents_desktop_bridge::ManagedServerPolicy;
use gents_desktop_bridge::NodeHomePolicy;
use gents_desktop_bridge::SnapshotGrants;
use gents_desktop_bridge::StoreKeyCustodyChoice;
use gents_lean_vocab::families::connect_this_desktop_local_node as family;
use gents_store::client_error::BridgeError;
use gents_store::client_error::BridgeErrorCode;
use gents_store::client_journal::paths::DesktopPaths;
use gents_store::client_journal::principal_identity::PrincipalIdentity;
use tauri::test::MockRuntime;
use tauri::Manager as _;

const DEADLINE: Duration = Duration::from_secs(180);

#[expect(
    clippy::disallowed_methods,
    reason = "a synchronous test is its own entry point; the futures run on the desktop's installed runtime"
)]
fn run<F>(future: F) -> F::Output
where
    F: Future,
{
    tauri::async_runtime::block_on(future)
}

struct ReadyReport(tokio::sync::mpsc::UnboundedSender<serde_json::Value>);

#[async_trait::async_trait]
impl gents_host::serve::ServeClient for ReadyReport {
    fn report(&self, report: &serde_json::Value) -> anyhow::Result<()> {
        let _ = self.0.send(report.clone());
        Ok(())
    }

    fn start_failure_hint(&self, _home_dir: &Path) -> String {
        String::new()
    }

    fn version(&self) -> gents_host::http::version::VersionResponse {
        gents_host::http::version::version_response()
    }

    async fn bound_root_agents(
        &self,
        root: &Path,
        _home_dir: &Path,
        _access: &gents_store::config_client::ConfigAccess,
    ) -> anyhow::Result<gents_host::serve::BoundRootAgents> {
        anyhow::bail!("no desired-state root is bound: {}", root.display())
    }
}

fn free_port() -> u16 {
    std::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// A home made by `gents init` (through the provisioning that runs its
/// `init_home`), listening on its own loopback ports so it never answers for
/// another node on this machine. Its administrator key is `desktop`'s CLI
/// client, which the home records.
fn terminal_home(home: &Path, desktop: &Path) {
    run(gents_server::server_host::ensure_standard_home(
        tauri::async_runtime::handle().inner(),
        gents_server::server_host::ProvisionOptions {
            home: home.to_path_buf(),
            desktop_root: desktop.to_path_buf(),
            node_name: "Anvil".to_owned(),
            tool_ceiling: gents_service::args::ToolCeilingArg::Readonly,
            tool_root: None,
            store_key_custody: StoreKeyCustodyChoice::File,
            default_packs: false,
        },
    ))
    .unwrap();
    gents_service::network_settings::NetworkSettings {
        http_addr: Some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
        http_port: Some(free_port()),
        p2p_bind_addr: Some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
        p2p_port: Some(free_port()),
        p2p_relay_mode: Some(gents_service::args::P2pRelayModeArg::Disabled),
        p2p_discovery: Some(gents_service::args::P2pDiscoveryArg::Disabled),
    }
    .persist(home)
    .unwrap();
}

struct Served {
    node_did: String,
    graphql: String,
    node: std::thread::JoinHandle<anyhow::Result<()>>,
}

/// Stops the node the way its supervisor does, so it releases its home
/// before the home is removed: a node whose home disappears exits the process.
fn stop(served: Served) {
    let terminated = std::process::Command::new("kill")
        .args(["-TERM", &std::process::id().to_string()])
        .status()
        .unwrap();
    assert!(terminated.success());
    served.node.join().unwrap().unwrap();
}

/// Serves `home` the way `gents runtime server --home` does, on a dedicated
/// thread, and returns once it reports ready.
#[expect(
    clippy::disallowed_methods,
    reason = "the node is driven on a dedicated OS thread, off the runtime workers"
)]
fn serve(home: &Path) -> Served {
    let cli = gents_host::test_support::cli::Cli::try_parse_from([
        "gents",
        "server",
        "--home",
        home.to_str().unwrap(),
        "--no-keychain-prompts",
    ])
    .unwrap();
    let Some(gents_host::test_support::cli::Command::Server(args)) = cli.command else {
        panic!("`server` parses as the server command");
    };
    let (ready, mut reported) = tokio::sync::mpsc::unbounded_channel();
    let handle = tauri::async_runtime::handle().inner().clone();
    let node = std::thread::Builder::new()
        .name("terminal-node".into())
        .stack_size(32 * 1024 * 1024)
        .spawn(move || {
            handle.block_on(gents_host::serve::serve(
                args,
                handle.clone(),
                Arc::new(ReadyReport(ready)),
            ))
        })
        .unwrap();
    let Some(report) = run(async { tokio::time::timeout(DEADLINE, reported.recv()).await })
        .expect("the node reports ready within the deadline")
    else {
        panic!("the node stopped before it was ready: {:?}", node.join());
    };
    Served {
        node_did: report["node_did"].as_str().unwrap().to_owned(),
        graphql: report["graphql"].as_str().unwrap().to_owned(),
        node,
    }
}

fn desktop_app(desktop: &Path, node_home: &Path) -> tauri::App<MockRuntime> {
    let policy = resolve_policy(
        &BridgeConfig {
            home: HomePolicy::FixedRoot(desktop.to_path_buf()),
            bootstrap: BootstrapPolicy::LocalRuntimeAllowed {
                node_home: NodeHomePolicy::Fixed(node_home.to_path_buf()),
            },
            managed_server: ManagedServerPolicy::Allowed,
            snapshot_grants: SnapshotGrants::all(),
            client_host: false,
            store_key_custody: StoreKeyCustodyChoice::File,
            ..Default::default()
        },
        None,
    )
    .unwrap();
    tauri::test::mock_builder()
        .manage(DesktopAppState::new(policy))
        .build(tauri::test::mock_context(tauri::test::noop_assets()))
        .unwrap()
}

fn connect(app: &tauri::App<MockRuntime>) -> Result<(), BridgeError> {
    run(managed_server::desktop_managed_server_enroll(
        app.handle().clone(),
        app.state::<DesktopAppState>(),
    ))
}

fn shut_down(app: tauri::App<MockRuntime>) {
    run(lifecycle::desktop_client_shutdown(
        app.handle().clone(),
        app.state::<DesktopAppState>(),
    ))
    .unwrap();
}

fn desktop_did(desktop: &Path) -> Option<String> {
    PrincipalIdentity::load_existing(&DesktopPaths::from_root(desktop))
        .ok()
        .map(|identity| identity.did().to_owned())
}

fn route_saved(desktop: &Path, node_did: &str) -> bool {
    run(
        gents_store::client_journal::runtime_directory::load_runtime_connections(
            DesktopPaths::from_root(desktop).runtime_directory_path(),
        ),
    )
    .is_ok_and(|peers| peers.iter().any(|peer| peer.node_did == node_did))
}

/// What the node holds for `member`, read as the User `gents init` made its
/// owner: the enrollment requests it authored and its live grant receipts.
fn node_records(
    home: &Path,
    served: Option<&Served>,
    member: Option<&str>,
) -> (BTreeSet<String>, usize) {
    let (Some(served), Some(member)) = (served, member) else {
        return (BTreeSet::new(), 0);
    };
    run(async {
        let owner = gents_store::config_client::ConfigAccess::Graphql(
            gents_service::home_state::home_graphql_endpoint(home, served.graphql.clone())
                .await
                .unwrap(),
        );
        let requests =
            gents_store::identity::client_access::list_requests(&owner, &served.node_did)
                .await
                .unwrap()
                .into_iter()
                .filter(|request| request.client_did == member)
                .map(|request| request.request_id)
                .collect();
        let receipts = gents_store::identity::load_enrollment_grant_receipts(&owner, member)
            .await
            .unwrap()
            .into_iter()
            .filter(|receipt| !receipt.withdrawn)
            .count();
        (requests, receipts)
    })
}

fn node_agents(home: &Path, served: &Served) -> BTreeSet<String> {
    run(async {
        let owner = gents_store::config_client::ConfigAccess::Graphql(
            gents_service::home_state::home_graphql_endpoint(home, served.graphql.clone())
                .await
                .unwrap(),
        );
        owner.execute("{ Agent { _docID } }").await.unwrap()["data"]["Agent"]
            .as_array()
            .unwrap()
            .iter()
            .map(|agent| agent["_docID"].as_str().unwrap().to_owned())
            .collect()
    })
}

fn enrolled(app: &tauri::App<MockRuntime>, node_did: &str) -> bool {
    let Some(core) = current_core(&app.state::<DesktopAppState>()) else {
        return false;
    };
    run(async {
        let Ok(access) = core.operator_access(node_did) else {
            return false;
        };
        access
            .read_view(&gents_documents::views::config::NodeList::new(node_did))
            .await
            .is_ok_and(|rows| !rows.is_empty())
    })
}

/// The snapshot the startup screen waits on: this node's settings read as
/// the desktop's own client, authoritative for the node.
fn settings_as_the_desktop(
    app: &tauri::App<MockRuntime>,
    node_did: &str,
) -> gents_desktop_bridge::types::DesktopClientSnapshot {
    let core = current_core(&app.state::<DesktopAppState>()).expect("the desktop client runs");
    let mut updates = core.store_updates();
    let read = async {
        loop {
            let snapshot = lifecycle::desktop_client_snapshot(app.state::<DesktopAppState>())
                .await
                .unwrap();
            let client = snapshot
                .client
                .as_ref()
                .expect("the snapshot has the client");
            if client
                .operator_config_authoritative_node_dids
                .as_ref()
                .is_some_and(|dids| dids.iter().any(|did| did == node_did))
            {
                return snapshot;
            }
            updates.changed().await.unwrap();
        }
    };
    run(async { tokio::time::timeout(DEADLINE, read).await })
        .unwrap_or_else(|_| panic!("the desktop never read the settings of {node_did}"))
}

fn copy_cli_client(from: &Path, desktop: &Path) {
    let cli = desktop.join("cli");
    std::fs::create_dir_all(&cli).unwrap();
    for entry in std::fs::read_dir(from.join("cli")).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() {
            std::fs::copy(entry.path(), cli.join(entry.file_name())).unwrap();
        }
    }
}

fn refusal(error: &BridgeError) -> String {
    assert!(
        error.message.contains("Connect this desktop again"),
        "a refusal names the next call: {}",
        error.message
    );
    match error.code {
        BridgeErrorCode::EndpointUnreachable => "node_not_running".to_owned(),
        code => panic!("unexpected refusal {code:?}: {}", error.message),
    }
}

/// Connect this desktop, replayed against the bridge command on homes made by
/// `gents init`: one served from the terminal, one never started. A desktop
/// that connects then reads the node's settings as its own client.
#[test]
fn connect_this_desktop_joins_a_terminal_node_and_reads_its_settings() {
    let cases = family::cases::<String, String, String>();
    let root = tempfile::tempdir().unwrap();
    let path = |name: &str| -> PathBuf { root.path().join(name) };
    std::fs::create_dir_all(path("user/.local/share")).unwrap();
    std::fs::create_dir_all(path("user/.config")).unwrap();
    std::env::set_var("HOME", path("user"));
    std::env::set_var("XDG_DATA_HOME", path("user/.local/share"));
    std::env::set_var("XDG_CONFIG_HOME", path("user/.config"));
    gents_desktop_bridge::install_runtime();

    let served_home = path("anvil");
    let stopped_home = path("stopped");
    terminal_home(&served_home, &path("desktop"));
    terminal_home(&stopped_home, &path("desktop"));
    let served = serve(&served_home);
    let agents_before = node_agents(&served_home, &served);

    let mut connected = 0;
    for (index, case) in cases.into_iter().enumerate() {
        let desktop = path(&format!("case-{index}"));
        copy_cli_client(&path("desktop"), &desktop);
        let serving = case.input.node == "serving";
        let (home, live) = if serving {
            (&served_home, Some(&served))
        } else {
            (&stopped_home, None)
        };
        let node_did = served.node_did.clone();
        if case.input.identity && !case.input.enrolled {
            let core = run(ClientRuntime::start_with_paths_and_options(
                DesktopPaths::from_root(&desktop),
                ClientRuntimeOptions {
                    store_key_custody: StoreKeyCustodyChoice::File,
                    ..Default::default()
                },
            ))
            .unwrap();
            run(Arc::new(core).shutdown()).unwrap();
        }
        if case.input.enrolled {
            let earlier = desktop_app(&desktop, home);
            connect(&earlier).unwrap();
            shut_down(earlier);
        }
        assert_eq!(desktop_did(&desktop).is_some(), case.input.identity);
        assert_eq!(route_saved(&desktop, &node_did), case.input.route_saved);

        let identity_before = desktop_did(&desktop);
        let route_before = route_saved(&desktop, &node_did);
        let (requests_before, receipts_before) =
            node_records(home, live, identity_before.as_deref());

        let app = desktop_app(&desktop, home);
        let outcome = connect(&app);

        let identity_after = desktop_did(&desktop);
        if identity_before.is_some() {
            assert_eq!(identity_after, identity_before, "the desktop keeps its key");
        }
        let (requests_after, receipts_after) = node_records(home, live, identity_after.as_deref());
        let mut steps = Vec::new();
        if identity_before.is_none() && identity_after.is_some() {
            steps.push("create_identity".to_owned());
        }
        if !route_before && route_saved(&desktop, &node_did) {
            steps.push("save_route".to_owned());
        }
        if current_core(&app.state::<DesktopAppState>()).is_some() {
            steps.push("start_client".to_owned());
        }
        if !requests_after.is_subset(&requests_before) {
            steps.push("request_enrollment".to_owned());
        }
        if receipts_after > receipts_before {
            steps.push("approve".to_owned());
        }
        let now_enrolled = enrolled(&app, &node_did);
        case.check(family::ConnectOutput {
            steps,
            refusal: outcome.as_ref().err().map(refusal),
            enrolled: now_enrolled,
        });

        if outcome.is_ok() {
            connected += 1;
            let snapshot = settings_as_the_desktop(&app, &node_did);
            let client = snapshot.client.unwrap();
            let node = client
                .deployments
                .iter()
                .find(|deployment| deployment.node_did == node_did)
                .expect("the desktop lists the node it joined");
            assert_eq!(node.label, "Anvil");
            assert_eq!(
                snapshot.bootstrap.init_node_did.as_deref(),
                Some(node_did.as_str())
            );
            shut_down(app);
        }
    }
    assert!(connected > 0, "some case joins the served node");
    assert_eq!(
        node_agents(&served_home, &served),
        agents_before,
        "the node's agents are untouched"
    );
    let desktop = path("revoked-desktop");
    copy_cli_client(&path("desktop"), &desktop);
    let app = desktop_app(&desktop, &served_home);
    connect(&app).unwrap();
    let core = current_core(&app.state::<DesktopAppState>()).unwrap();
    let member = core.node_identity().did().to_owned();
    let (before, _) = node_records(&served_home, Some(&served), Some(&member));
    let old_request = before.iter().next().unwrap();
    run(async {
        gents_service::home_state::home_graphql_endpoint(&served_home, served.graphql.clone())
            .await
            .unwrap()
            .decide_runtime_access(&gents_protocol::client_access::DecideClientAccess {
                node_did: served.node_did.clone(),
                request_id: old_request.clone(),
                decision: gents_protocol::client_access::ClientAccessDecision::Revoke,
            })
            .await
            .unwrap();
    });
    let access = core.operator_access(&served.node_did).unwrap();
    let unavailable = run(
        gents_observe::client_projection::query::load_node_snapshot_on(&access, &served.node_did),
    )
    .unwrap_err();
    assert!(unavailable.refused());
    assert!(run(core.refresh_node(&served.node_did)).is_err());
    let blocked = run(lifecycle::desktop_client_snapshot(
        app.state::<DesktopAppState>(),
    ))
    .unwrap();
    assert!(!blocked
        .client
        .unwrap()
        .operator_config_authoritative_node_dids
        .unwrap_or_default()
        .contains(&served.node_did));
    let connection = core
        .runtime_connections()
        .into_iter()
        .find(|connection| connection.node_did == served.node_did)
        .unwrap();
    let pending = run(core.request_runtime_access(connection)).unwrap();
    assert!(!before.contains(&pending.request_id));
    let requests = run(async {
        let owner =
            gents_service::home_state::home_graphql_endpoint(&served_home, served.graphql.clone())
                .await
                .unwrap();
        gents_store::identity::client_access::list_requests(
            &gents_store::config_client::ConfigAccess::Graphql(owner),
            &served.node_did,
        )
        .await
        .unwrap()
    });
    assert!(requests
        .iter()
        .any(|request| request.request_id == pending.request_id && request.client_did == member));
    connect(&app).unwrap();
    let restored = settings_as_the_desktop(&app, &served.node_did);
    assert!(restored
        .client
        .unwrap()
        .operator_config_authoritative_node_dids
        .unwrap_or_default()
        .contains(&served.node_did));
    assert_eq!(core.node_identity().did(), member);
    shut_down(app);
    stop(served);
}
