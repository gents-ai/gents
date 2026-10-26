use std::net::{IpAddr, Ipv4Addr};
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use gents::config_client::GraphqlEndpoint;
use gents_protocol::serve_lifecycle::{
    control::{
        current_capabilities, RuntimeControlCommand, RuntimeControlDisposition,
        RuntimeControlReceipt, RuntimeOperation, RUNTIME_CONTROL_PATH, RUNTIME_CONTROL_VERSION,
    },
    preparation::{
        RuntimePreparationOutcome, RuntimePreparationRefusal, RuntimePreparationReport,
        RuntimePreparationResult, RUNTIME_PREPARATION_VERSION,
    },
    ServeLifecycle,
};
use tokio::io::AsyncWriteExt as _;

use crate::cli::{P2pTransportArg, ServeArgs};
use crate::home_state::{load_initialized_home_identity, read_init_config, read_runtime_state};
use crate::shared::{StoredInitConfig, StoredRuntimeState};

const STARTUP_WAIT: Duration = Duration::from_secs(60);
const CONTROL_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_CONTROL_RECEIPT: usize = 64 * 1024;

mod unlock;
pub use unlock::{prepare_runtime_after_unlock, unlock_runtime_keys};

#[cfg(test)]
mod tests;

#[derive(Debug)]
struct PreparationFailure {
    reason: RuntimePreparationRefusal,
    message: String,
}

impl std::fmt::Display for PreparationFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for PreparationFailure {}

fn refusal(reason: RuntimePreparationRefusal, message: impl Into<String>) -> anyhow::Error {
    anyhow::Error::new(PreparationFailure {
        reason,
        message: message.into(),
    })
}

fn refusal_reason(error: &anyhow::Error) -> RuntimePreparationRefusal {
    if error
        .downcast_ref::<gents::store_key::StoreKeyUnavailable>()
        .is_some()
        || error
            .downcast_ref::<gents::identity::IdentityKeyUnavailable>()
            .is_some()
    {
        RuntimePreparationRefusal::KeyAccess
    } else {
        error
            .downcast_ref::<PreparationFailure>()
            .map_or(RuntimePreparationRefusal::Failed, |failure| failure.reason)
    }
}

pub(crate) struct PreparedStartup {
    pub(crate) reservation: gents::home::RuntimeStartupReservation,
    pub(crate) outcome: RuntimePreparationOutcome,
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
pub(crate) async fn prepare(args: ServeArgs, runtime: tokio::runtime::Handle) -> Result<()> {
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

async fn prepare_inner(mut args: ServeArgs, runtime: tokio::runtime::Handle) -> Result<()> {
    let _keychain_prompt_guard =
        crate::process_resources::prevent_keychain_prompts(!args.keychain_startup_approval)?;
    crate::process_resources::prepare()?;
    anyhow::ensure!(
        args.data_dir.is_none(),
        "runtime preparation requires the home's default data directory"
    );
    let home = crate::home_state::resolve_home_dir(args.home.as_deref());
    let home = tokio::task::spawn_blocking(move || std::fs::canonicalize(home)).await??;
    let build = gents::runtime_service::executable_build_id().await?;
    let reservation =
        tokio::time::timeout(STARTUP_WAIT, gents::home::reserve_runtime_startup(&home))
            .await
            .map_err(|_| {
                refusal(
                    RuntimePreparationRefusal::StartupBusy,
                    "Another runtime preparation is still in progress. Retry after it finishes.",
                )
            })??;
    let (config, state) = tokio::task::spawn_blocking({
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
            if observation.build == build && !observation.retiring {
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
                let identity = tokio::task::spawn_blocking({
                    let home = home.clone();
                    let config = config.clone();
                    move || {
                        if let Some(encryption) = config.store_encryption.as_ref() {
                            encryption.load(
                                &gents::store_key::home_key_file(&home),
                                &gents::home::default_data_dir(&home),
                            )?;
                        }
                        load_initialized_home_identity(&home, &config)
                    }
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
                })?;
                let authorization =
                    GraphqlEndpoint::as_principal(endpoint.to_string(), identity.did());
                let retired = control(&client, &endpoint, &config.node_did, RuntimeOperation::Retire { expected_build: observation.build }, Some(&authorization)).await?
                    .ok_or_else(|| refusal(RuntimePreparationRefusal::Unavailable, "The runtime closed before retirement could be confirmed. Retry to inspect its state."))?;
                match retired.disposition {
                    RuntimeControlDisposition::Retiring | RuntimeControlDisposition::Stopping => {}
                    RuntimeControlDisposition::Busy => return Err(refusal(RuntimePreparationRefusal::Busy, "The runtime still owns active work. Let it finish, then retry the update.")),
                    RuntimeControlDisposition::Starting => return Err(refusal(RuntimePreparationRefusal::Starting, "The runtime is still starting. Retry when it is ready.")),
                    _ => return Err(refusal(RuntimePreparationRefusal::Unavailable, "The observed runtime changed before retirement. Retry to inspect its state.")),
                }
            }
            tokio::time::timeout(STARTUP_WAIT, gents::home::wait_for_store_release(&home, &gents::home::default_data_dir(&home)))
                .await.map_err(|_| refusal(RuntimePreparationRefusal::StoreBusy, "The retiring runtime has not released its store. Retry after shutdown finishes."))??;
            outcome = RuntimePreparationOutcome::Replaced;
        }
        recorded_endpoint = Some(endpoint);
    }
    tokio::task::spawn_blocking({
        let home = home.clone();
        move || gents::home::lock_home_store(&home).map(drop)
    })
    .await?
    .map_err(|error| {
        if error.downcast_ref::<gents::home::StoreLockHeld>().is_some() {
            refusal(
                RuntimePreparationRefusal::StoreBusy,
                "Another runtime still owns this store. Its retirement has not been confirmed.",
            )
        } else {
            error
        }
    })?;
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
    args.no_codex_shim = true;
    args.grok_shim = false;
    super::serve::serve_prepared(
        args,
        runtime,
        PreparedStartup {
            reservation,
            outcome,
        },
    )
    .await
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

async fn control(
    client: &reqwest::Client,
    endpoint: &reqwest::Url,
    node_did: &str,
    operation: RuntimeOperation,
    authorization: Option<&GraphqlEndpoint>,
) -> Result<Option<RuntimeControlReceipt>> {
    let command = RuntimeControlCommand {
        version: RUNTIME_CONTROL_VERSION,
        node_did: node_did.to_owned(),
        nonce: uuid::Uuid::new_v4().to_string(),
        issued_at: chrono::Utc::now().to_rfc3339(),
        operation,
    };
    let mut url = endpoint.clone();
    url.set_path(RUNTIME_CONTROL_PATH);
    let mut request = client.post(url).json(&command);
    if let Some(authorization) = authorization {
        request = authorization.authorize(request)?;
    }
    let mut response = match request.send().await {
        Ok(response) => response,
        Err(error)
            if error.is_connect() && matches!(command.operation, RuntimeOperation::Observe) =>
        {
            return Ok(None)
        }
        Err(error) => {
            return Err(refusal(
                RuntimePreparationRefusal::Unavailable,
                format!("Runtime control did not complete. Retry to inspect its state: {error}"),
            ))
        }
    };
    if matches!(
        response.status(),
        reqwest::StatusCode::NOT_FOUND | reqwest::StatusCode::NOT_IMPLEMENTED
    ) {
        return Err(refusal(RuntimePreparationRefusal::Unsupported, "The running runtime does not support authenticated updates. Stop it explicitly before preparing this package."));
    }
    if !response.status().is_success() {
        if matches!(
            response.status(),
            reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
        ) {
            return Err(refusal(RuntimePreparationRefusal::Credentials, "The runtime did not authorize its recorded operator. Restore operator access before retrying."));
        }
        return Err(refusal(
            RuntimePreparationRefusal::Unavailable,
            format!(
                "Runtime control is unavailable (HTTP {}). Retry after the runtime is ready.",
                response.status().as_u16()
            ),
        ));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        anyhow::ensure!(
            bytes.len() + chunk.len() <= MAX_CONTROL_RECEIPT,
            "runtime control receipt is too large"
        );
        bytes.extend_from_slice(&chunk);
    }
    let receipt = serde_json::from_slice(&bytes)?;
    gents::runtime_service::verify_control_receipt(&command, &receipt).map_err(|error| {
        refusal(
            RuntimePreparationRefusal::InvalidHome,
            format!("The live runtime could not authenticate this node: {error:#}"),
        )
    })?;
    Ok(Some(receipt))
}
