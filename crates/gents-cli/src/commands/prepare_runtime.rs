use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
#[cfg(test)]
use gents_protocol::serve_lifecycle::control::{
    RuntimeControlCommand, RuntimeControlReceipt, RUNTIME_CONTROL_PATH, RUNTIME_CONTROL_VERSION,
};
use gents_protocol::serve_lifecycle::{
    control::{current_capabilities, RuntimeControlDisposition, RuntimeOperation},
    preparation::{
        RuntimePreparationOutcome, RuntimePreparationRefusal, RuntimePreparationReport,
        RuntimePreparationResult, RUNTIME_PREPARATION_VERSION,
    },
    ServeLifecycle,
};
use gents_store::config_client::GraphqlEndpoint;
use tokio::io::AsyncWriteExt as _;

use crate::cli::{P2pTransportArg, PrepareRuntimeArgs};
use gents_service::home_state::load_initialized_home_identity;
use gents_service::home_state::read_init_config;
use gents_service::home_state::read_runtime_state;
use gents_service::runtime_control::{control, refusal, PreparationFailure};
use gents_service::state::StoredInitConfig;
use gents_service::state::StoredRuntimeState;

/// Preparation must eventually report a busy home if its startup reservation
/// or the prior runtime's store release does not arrive.
const STARTUP_WAIT: Duration = Duration::from_secs(60);
/// A recorded runtime endpoint may be stale or unresponsive during preparation.
const CONTROL_TIMEOUT: Duration = Duration::from_secs(5);

mod unlock;
pub use unlock::{prepare_runtime_after_unlock, unlock_runtime_keys};

#[cfg(test)]
mod tests;

fn refusal_reason(error: &anyhow::Error) -> RuntimePreparationRefusal {
    if gents_store::local::store_key::store_key_unavailable(error.as_ref()).is_some()
        || gents_store::identity::identity_key_unavailable(error.as_ref()).is_some()
    {
        RuntimePreparationRefusal::KeyAccess
    } else {
        error
            .downcast_ref::<PreparationFailure>()
            .map_or(RuntimePreparationRefusal::Failed, |failure| failure.reason)
    }
}

pub(crate) struct PreparedStartup {
    pub(crate) reservation: gents_store::local::home::RuntimeStartupReservation,
    pub(crate) outcome: RuntimePreparationOutcome,
    pub(crate) pending_config: Option<(StoredInitConfig, StoredInitConfig)>,
}

pub(crate) async fn emit(result: RuntimePreparationResult) -> Result<()> {
    let mut frame = serde_json::to_vec(&RuntimePreparationReport {
        version: RUNTIME_PREPARATION_VERSION,
        result,
    })?;
    frame.push(b'\n');
    let mut stdout = tokio::io::stdout();
    stdout.write_all(&frame).await?;
    stdout.flush().await?;
    Ok(())
}

/// Selecting this package explicitly selects its executable as the desired
/// build; digest inequality is not evidence that either executable is newer.
/// The preparation process becomes the server, retaining the startup lock
/// through native readiness instead of handing an unlocked home to a child.
pub(crate) async fn prepare(
    args: PrepareRuntimeArgs,
    runtime: tokio::runtime::Handle,
) -> Result<()> {
    let result = prepare_inner(args, runtime).await;
    if let Err(error) = &result {
        let reason = refusal_reason(error);
        if let Err(output_error) = emit(RuntimePreparationResult::Refused {
            reason,
            message: format!("{error:#}"),
        })
        .await
        {
            tracing::warn!(%output_error, "runtime preparation refusal channel closed");
        }
    }
    result
}

async fn prepare_inner(
    preparation: PrepareRuntimeArgs,
    runtime: tokio::runtime::Handle,
) -> Result<()> {
    let mut args = preparation.serve;
    let requested_root = preparation
        .change_tool_root
        .map(validate_requested_root)
        .transpose()?;
    let _keychain_prompt_guard = gents_service::process_resources::prevent_keychain_prompts(
        !args.keychain_startup_approval,
    )?;
    gents_service::process_resources::prepare()?;
    anyhow::ensure!(
        args.data_dir.is_none(),
        "runtime preparation requires the home's default data directory"
    );
    let home = gents_service::home_state::resolve_home_dir(args.home.as_deref());
    let home = tokio::task::spawn_blocking(move || std::fs::canonicalize(home)).await??;
    let build = gents_store::service::executable_build_id().await?;
    #[cfg(any(target_os = "macos", target_os = "linux"))]
    {
        let check_home = home.clone();
        let check_build = build.clone();
        tokio::task::spawn_blocking(move || -> Result<()> {
            let manager = gents_service::native_service::NativeServiceManager::new(gents_service::native_service::NativeServiceConfig::new(check_home.clone(), std::env::current_exe()?)?)?;
            if let Some(id) = manager.pending_update()? {
                return Err(refusal(RuntimePreparationRefusal::StartupBusy, format!("Managed update {id} is pending; inspect service update-status --home {} {id} before selecting another package.", check_home.display())));
            }
            if manager.definition_owned_by_home()? && manager.has_managed_executable()? {
                let selected = manager.installed_executable_path()?;
                if gents_service::service_update::executable_digest(&selected)? != check_build {
                    return Err(refusal(RuntimePreparationRefusal::Unsupported, "The node has an explicitly selected managed-update executable. Use that package or an explicit managed update; this package will not replace it automatically."));
                }
            }
            Ok(())
        }).await??;
    }
    let reservation = tokio::time::timeout(
        STARTUP_WAIT,
        gents_store::local::home::reserve_runtime_startup(&home),
    )
    .await
    .map_err(|_| {
        refusal(
            RuntimePreparationRefusal::StartupBusy,
            "Another runtime preparation is still in progress. Retry after it finishes.",
        )
    })??;
    let (mut config, state) = tokio::task::spawn_blocking({
        let home = home.clone();
        move || read_home(&home)
    })
    .await??;
    let client = tokio::task::spawn_blocking(|| {
        reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(CONTROL_TIMEOUT)
            .build()
    })
    .await??;
    let mut outcome = RuntimePreparationOutcome::Started;
    let mut recorded_endpoint = None;
    let mut credentials_checked = false;
    if let Some(state) = &state {
        args.p2p_transport = match state.p2p_transport.as_str() {
            "none" => P2pTransportArg::None,
            "iroh" => P2pTransportArg::Iroh,
            _ => {
                return Err(refusal(
                    RuntimePreparationRefusal::Unsupported,
                    "The recorded runtime uses an unsupported transport.",
                ))
            }
        };
        let endpoint = local_endpoint(&state.graphql)?;
        let observation = control(
            &client,
            &endpoint,
            &config.node_did,
            RuntimeOperation::Observe,
            None,
        )
        .await?;
        if let Some(observation) = observation {
            if observation.build == build && !observation.retiring && requested_root.is_none() {
                if observation.lifecycle != ServeLifecycle::Ready {
                    return Err(refusal(
                        RuntimePreparationRefusal::Starting,
                        "The matching runtime is still starting. Retry when it is ready.",
                    ));
                }
                if !current_capabilities()
                    .iter()
                    .all(|(name, version)| observation.capabilities.get(name) == Some(version))
                {
                    return Err(refusal(
                        RuntimePreparationRefusal::Unsupported,
                        "The running runtime does not expose this package's required capabilities.",
                    ));
                }
                return emit(RuntimePreparationResult::Ready {
                    outcome: RuntimePreparationOutcome::Reused,
                    home: home.to_string_lossy().into_owned(),
                    node_did: config.node_did,
                    graphql: state.graphql.clone(),
                    build,
                })
                .await;
            }
            if !observation.retiring {
                let identity = load_runtime_identity(&home, &config).await?;
                credentials_checked = true;
                let authorization =
                    GraphqlEndpoint::as_principal(endpoint.to_string(), identity.did())
                        .with_client_signing_capability(identity.client_signing_capability()?)?;
                if requested_root.is_some() {
                    let status = super::status::fetch_local_operator_status(&authorization, CONTROL_TIMEOUT)
                        .await
                        .map_err(|error| refusal(
                            RuntimePreparationRefusal::Unavailable,
                            format!("Could not confirm the running runtime's access settings: {error:#}"),
                        ))?;
                    validate_running_authority(&status, &config)?;
                }
                if let Some(root) = &requested_root {
                    ensure_requested_root_published(&authorization, &config, root).await?;
                }
                let retired = control(&client, &endpoint, &config.node_did, RuntimeOperation::Retire { expected_build: observation.build }, Some(&authorization)).await?
                    .ok_or_else(|| refusal(RuntimePreparationRefusal::Unavailable, "The runtime closed before retirement could be confirmed. Retry to inspect its state."))?;
                match retired.disposition {
                    RuntimeControlDisposition::Retiring | RuntimeControlDisposition::Stopping => {}
                    RuntimeControlDisposition::Busy => return Err(refusal(RuntimePreparationRefusal::Busy, "The runtime still owns active work. Let it finish, then retry the update.")),
                    RuntimeControlDisposition::Starting => return Err(refusal(RuntimePreparationRefusal::Starting, "The runtime is still starting. Retry when it is ready.")),
                    _ => return Err(refusal(RuntimePreparationRefusal::Unavailable, "The observed runtime changed before retirement. Retry to inspect its state.")),
                }
            }
            tokio::time::timeout(STARTUP_WAIT, gents_store::local::home::wait_for_store_release(&home, &gents_store::local::home::default_data_dir(&home)))
                .await.map_err(|_| refusal(RuntimePreparationRefusal::StoreBusy, "The retiring runtime has not released its store. Retry after shutdown finishes."))??;
            outcome = RuntimePreparationOutcome::Replaced;
        }
        recorded_endpoint = Some(endpoint);
    }
    tokio::task::spawn_blocking({
        let home = home.clone();
        move || gents_store::local::home::lock_home_store(&home).map(drop)
    })
    .await?
    .map_err(|error| {
        if error.lock_held().is_some() {
            refusal(
                RuntimePreparationRefusal::StoreBusy,
                "Another runtime still owns this store. Its retirement has not been confirmed.",
            )
        } else {
            error.into()
        }
    })?;
    let pending_config = if let Some(root) = requested_root {
        if !credentials_checked {
            drop(load_runtime_identity(&home, &config).await?);
        }
        let checked = validate_requested_root(root.clone())?;
        anyhow::ensure!(
            checked == root,
            "The requested tool root changed during runtime retirement"
        );
        let expected = config.clone();
        config.tool_root = Some(
            root.to_str()
                .context("The tool root is not valid UTF-8")?
                .to_owned(),
        );
        args.tool_root = Some(root);
        Some((expected, config))
    } else {
        None
    };
    let (address, port) = if let Some(endpoint) = recorded_endpoint {
        (
            endpoint
                .host_str()
                .context("recorded endpoint host")?
                .trim_matches(['[', ']'])
                .parse::<IpAddr>()?,
            endpoint
                .port_or_known_default()
                .context("recorded endpoint port")?,
        )
    } else {
        let address = args.http_addr.unwrap_or(Ipv4Addr::LOCALHOST.into());
        if !address.is_loopback() {
            return Err(refusal(
                RuntimePreparationRefusal::InvalidHome,
                "Managed runtime preparation requires a loopback HTTP listener.",
            ));
        }
        let port = match args.http_port {
            Some(port) if port != 0 => port,
            _ => {
                let listener = tokio::net::TcpListener::bind((address, 0)).await?;
                listener.local_addr()?.port()
            }
        };
        (address, port)
    };
    args.home = Some(home);
    args.http_addr = Some(address);
    args.http_port = Some(port);
    args.no_keychain_prompts = !args.keychain_startup_approval;
    super::serve::serve_prepared(
        args,
        runtime,
        PreparedStartup {
            reservation,
            outcome,
            pending_config,
        },
    )
    .await
}

async fn load_runtime_identity(
    home: &Path,
    config: &StoredInitConfig,
) -> Result<std::sync::Arc<dyn gents_store::identity::NodeIdentity>> {
    let home = home.to_path_buf();
    let config = config.clone();
    tokio::task::spawn_blocking(move || {
        if let Some(encryption) = config.store_encryption.as_ref() {
            encryption.load(
                &gents_store::local::store_key::home_key_file(&home),
                &gents_store::local::home::default_data_dir(&home),
            )?;
        }
        load_initialized_home_identity(&home, &config)
    })
    .await?
    .map_err(|error| {
        if refusal_reason(&error) == RuntimePreparationRefusal::KeyAccess {
            error.context("The recorded runtime keys need access")
        } else {
            refusal(
                RuntimePreparationRefusal::Credentials,
                format!("The runtime credentials are unavailable: {error:#}"),
            )
        }
    })
}

fn validate_requested_root(root: PathBuf) -> Result<PathBuf> {
    anyhow::ensure!(
        root.is_absolute(),
        "The tool root must be an absolute directory"
    );
    let root =
        std::fs::canonicalize(root).context("The tool root must be an existing directory")?;
    anyhow::ensure!(root.is_dir(), "The tool root must be an existing directory");
    anyhow::ensure!(root.to_str().is_some(), "The tool root is not valid UTF-8");
    Ok(root)
}

/// Serve admits a host tool root only when a WorkspaceRoot published for the
/// Node covers it. Asking the running runtime through the same owner refuses an
/// unpublished root before that runtime is retired; publishing stays with the
/// operator, since preparation must not widen the Node's authority.
async fn ensure_requested_root_published(
    authorization: &GraphqlEndpoint,
    config: &StoredInitConfig,
    root: &Path,
) -> Result<()> {
    if matches!(
        config.tool_ceiling,
        gents_service::args::ToolCeilingArg::MetaOnly
    ) {
        return Ok(());
    }
    let access = gents_store::config_client::ConfigAccess::Graphql(authorization.clone());
    gents_tools::surface::ensure_root_published_for_node(&access, &config.node_did, root)
        .await
        .map_err(|error| {
            refusal(
                RuntimePreparationRefusal::Failed,
                format!("The running runtime keeps its current workspace root: {error:#}"),
            )
        })
}

fn validate_running_authority(status: &serde_json::Value, config: &StoredInitConfig) -> Result<()> {
    let expected_ceiling = crate::format_tool_ceiling(config.tool_ceiling);
    let matches = status.get("node_did").and_then(serde_json::Value::as_str)
        == Some(config.node_did.as_str())
        && status
            .get("tool_ceiling")
            .and_then(serde_json::Value::as_str)
            == Some(expected_ceiling)
        && status
            .get("tool_root_error")
            .is_some_and(serde_json::Value::is_null)
        && status
            .get("cli_tools")
            .and_then(serde_json::Value::as_array)
            .is_some_and(Vec::is_empty);
    if !matches {
        return Err(refusal(
            RuntimePreparationRefusal::Unavailable,
            "The running runtime's access settings differ from its saved configuration. Stop it through its owner and reconcile access before changing the workspace root.",
        ));
    }
    Ok(())
}

pub(crate) fn commit_root_change(
    home: &Path,
    expected: &StoredInitConfig,
    updated: &StoredInitConfig,
) -> Result<()> {
    anyhow::ensure!(
        read_init_config(home)?.as_ref() == Some(expected),
        "Runtime configuration changed during startup; retry the workspace access change"
    );
    gents_service::home_state::write_init_config(home, updated)
}

fn read_home(home: &Path) -> Result<(StoredInitConfig, Option<StoredRuntimeState>)> {
    let config = read_init_config(home)?.ok_or_else(|| {
        refusal(
            RuntimePreparationRefusal::NotInitialized,
            "Initialize this runtime home before preparing it.",
        )
    })?;
    if std::fs::canonicalize(&config.home)? != home {
        return Err(refusal(
            RuntimePreparationRefusal::InvalidHome,
            "The initialized runtime belongs to a different home.",
        ));
    }
    let state = read_runtime_state(home)?;
    if let Some(state) = &state {
        if state.node_did != config.node_did || std::fs::canonicalize(&state.home)? != home {
            return Err(refusal(
                RuntimePreparationRefusal::InvalidHome,
                "The recorded runtime does not belong to this initialized home.",
            ));
        }
    }
    Ok((config, state))
}

fn local_endpoint(raw: &str) -> Result<reqwest::Url> {
    let endpoint = reqwest::Url::parse(raw)?;
    let loopback = endpoint
        .host_str()
        .and_then(|host| host.trim_matches(['[', ']']).parse::<IpAddr>().ok())
        .is_some_and(|ip| ip.is_loopback());
    if endpoint.scheme() != "http"
        || !loopback
        || !endpoint.username().is_empty()
        || endpoint.password().is_some()
        || endpoint.path() != "/api/v0/graphql"
        || endpoint.query().is_some()
        || endpoint.fragment().is_some()
    {
        return Err(refusal(
            RuntimePreparationRefusal::InvalidHome,
            "The recorded runtime must use an unambiguous local GraphQL endpoint.",
        ));
    }
    Ok(endpoint)
}
