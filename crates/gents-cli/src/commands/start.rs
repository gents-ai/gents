use std::io::IsTerminal as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context, Result};
use clap::CommandFactory as _;

use crate::cli::args::{CodexArgs, StoreKeyCustodyArg};
use crate::cli::Cli;
use crate::client_settings::{ensure_client_supported, ClientSettings, InterfaceClient};
use crate::commands::codex::{
    endpoint_answers, ensure_endpoint_is_this_homes, interface_bin, launch_interface, open_codex,
};
use crate::graphql_access::graphql_endpoint_available;
use crate::home_state::{read_init_config, read_runtime_state, resolve_home_dir};
use crate::native_service::{NativeServiceConfig, NativeServiceManager, NativeServicePlatform};

mod setup;

const GROK_BIN_ENV: &str = "GENTS_GROK_BIN";
const ENDPOINT_WAIT: Duration = Duration::from_secs(60);

enum InterfaceEndpoint {
    Codex { remote: String },
    Grok { socket: PathBuf },
}

impl InterfaceEndpoint {
    fn for_client(client: InterfaceClient) -> Result<Self> {
        ensure_client_supported(client, cfg!(unix))?;
        Ok(match client {
            InterfaceClient::Codex => Self::Codex {
                remote: crate::DEFAULT_CODEX_REMOTE.to_string(),
            },
            #[cfg(unix)]
            InterfaceClient::Grok => Self::Grok {
                socket: crate::commands::serve::resolve_grok_shim_socket_path(None),
            },
            #[cfg(not(unix))]
            InterfaceClient::Grok => unreachable!("refused off Unix above"),
        })
    }

    async fn answers(&self) -> bool {
        match self {
            Self::Codex { remote } => endpoint_answers(remote).await,
            #[cfg(unix)]
            Self::Grok { socket } => tokio::net::UnixStream::connect(socket).await.is_ok(),
            #[cfg(not(unix))]
            Self::Grok { .. } => false,
        }
    }

    /// Grok's socket is per machine and its limit is the shim's own lock, so only
    /// Codex's fixed port needs identifying.
    async fn ensure_is_this_homes(&self, expected_did: &str) -> Result<()> {
        match self {
            Self::Codex { remote } => ensure_endpoint_is_this_homes(remote, expected_did).await,
            Self::Grok { .. } => Ok(()),
        }
    }

    fn describe(&self) -> String {
        match self {
            Self::Codex { remote } => format!("Codex endpoint {remote}"),
            Self::Grok { socket } => format!("Grok socket {}", socket.display()),
        }
    }

    async fn open(self) -> Result<()> {
        match self {
            Self::Codex { remote } => {
                open_codex(CodexArgs {
                    remote,
                    remote_auth_token_env: None,
                    no_alt_screen: false,
                    prompt: None,
                })
                .await
            }
            Self::Grok { socket } => {
                launch_interface(
                    "Grok",
                    &interface_bin(GROK_BIN_ENV, "grok"),
                    &grok_command_args(&socket),
                    &interface_install_message(InterfaceClient::Grok),
                )
                .await
            }
        }
    }
}

/// Dispatched before telemetry, so nothing writes over the interface.
pub(crate) async fn run(
    home: Option<PathBuf>,
    store_key_custody: Option<StoreKeyCustodyArg>,
) -> Result<()> {
    // Setup prompts go to stderr, so a redirected stderr would hide them.
    if !std::io::stdin().is_terminal()
        || !std::io::stdout().is_terminal()
        || !std::io::stderr().is_terminal()
    {
        eprint!("{}", Cli::command().render_help());
        std::process::exit(2);
    }
    let home_given = home.map(crate::absolutize).transpose()?;
    let mut home = crate::absolutize(resolve_home_dir(home_given.as_deref()))?;
    let config = match read_init_config(&home)? {
        Some(config) => config,
        None => {
            home = setup::setup(home_given, store_key_custody).await?;
            read_init_config(&home)?.context("setup left this home uninitialized")?
        }
    };
    let endpoint = InterfaceEndpoint::for_client(ClientSettings::read(&home)?.client)?;
    ensure_runtime(&home, &config.node_did, &endpoint).await?;
    endpoint.open().await
}

async fn ensure_runtime(
    home: &Path,
    expected_did: &str,
    endpoint: &InterfaceEndpoint,
) -> Result<()> {
    if endpoint.answers().await {
        return endpoint.ensure_is_this_homes(expected_did).await;
    }
    if let Some(state) = read_runtime_state(home)? {
        if graphql_endpoint_available(&state.graphql).await {
            bail!(
                "this home's runtime is up with its {} off. Restart it with `gents service restart --home {}`, or restart whatever is running it, then run `gents` again.",
                endpoint.describe(),
                home.display()
            );
        }
    }
    let hint = no_service_manager_message(home);
    let platform = NativeServicePlatform::current().map_err(|_| anyhow!("{hint}"))?;
    let service_home = home.to_path_buf();
    tokio::task::spawn_blocking(move || -> Result<()> {
        let executable = std::env::current_exe().context("resolving the gents executable")?;
        let config = NativeServiceConfig::new(service_home, executable)?;
        let definition = config.definition_path(platform);
        let manager = NativeServiceManager::new(config)?;
        if !definition.is_file() {
            manager.install()?;
        }
        manager.start(false)
    })
    .await?
    .map_err(|error| anyhow!("{error:#}\n{hint}"))?;
    wait_for_endpoint(endpoint, ENDPOINT_WAIT).await?;
    endpoint.ensure_is_this_homes(expected_did).await
}

/// launchd and systemd report supervision, not readiness, and no cross-process notification of
/// readiness exists, so the endpoint answering is the only observation. A bounded, single-task
/// async retry blocks no runtime thread, the same shape as `service_update/execute.rs:330`.
async fn wait_for_endpoint(endpoint: &InterfaceEndpoint, timeout: Duration) -> Result<()> {
    let deadline = Instant::now() + timeout;
    while !endpoint.answers().await {
        if Instant::now() >= deadline {
            bail!(
                "the runtime did not open its {} within {} s; it was left running, check `gents service status`",
                endpoint.describe(),
                timeout.as_secs_f32()
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Ok(())
}

fn no_service_manager_message(home: &Path) -> String {
    format!(
        "run `gents server --home {}` in another terminal, then `gents`",
        home.display()
    )
}

fn grok_command_args(socket: &Path) -> Vec<String> {
    vec![
        "--leader".to_string(),
        "--leader-socket".to_string(),
        socket.display().to_string(),
    ]
}

pub(crate) fn interface_install_message(client: InterfaceClient) -> String {
    let (how, override_env) = match client {
        InterfaceClient::Codex => (
            "Codex was not found. Install it with `brew install codex` or `npm install -g @openai/codex`",
            crate::commands::codex::CODEX_BIN_ENV,
        ),
        InterfaceClient::Grok => (
            "Grok was not found. Install the Grok CLI from x.ai",
            GROK_BIN_ENV,
        ),
    };
    format!("{how}, or set {override_env} to its path. `gents chat` works without it.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn wait_for_endpoint_gives_up_naming_gents_service_status() {
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = closed.local_addr().unwrap().port();
        drop(closed);
        let endpoint = InterfaceEndpoint::Codex {
            remote: format!("ws://127.0.0.1:{port}/"),
        };

        let error = wait_for_endpoint(&endpoint, Duration::from_millis(200))
            .await
            .expect_err("a closed port never answers");

        assert!(
            error.to_string().contains("gents service status"),
            "{error:#}"
        );
    }

    #[test]
    fn no_service_manager_says_run_gents_server_then_gents() {
        assert_eq!(
            no_service_manager_message(Path::new("/srv/home")),
            "run `gents server --home /srv/home` in another terminal, then `gents`"
        );
    }

    #[test]
    fn grok_args_are_exact() {
        assert_eq!(
            grok_command_args(Path::new("/tmp/leader.sock")),
            ["--leader", "--leader-socket", "/tmp/leader.sock"]
        );
    }
}
