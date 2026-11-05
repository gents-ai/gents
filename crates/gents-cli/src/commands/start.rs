use std::io::IsTerminal as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, ensure, Context, Result};
use clap::CommandFactory as _;

use crate::cli::args::{CodexArgs, StoreKeyCustodyArg};
use crate::cli::Cli;
use crate::client_settings::{ensure_client_supported, ClientSettings, InterfaceClient};
use crate::commands::codex::{
    endpoint_answers, ensure_an_agent_can_run, ensure_endpoint_is_this_homes, interface_bin,
    launch_interface, open_codex, runtime_identity_at, PUBLISH_WAIT,
};
use crate::graphql_access::graphql_endpoint_available;
use crate::home_state::{read_init_config, read_runtime_state, resolve_home_dir};
use crate::native_service::{NativeServiceConfig, NativeServiceManager, NativeServicePlatform};

mod setup;

const GROK_BIN_ENV: &str = "GENTS_GROK_BIN";
const ENDPOINT_WAIT: Duration = Duration::from_secs(60);

enum InterfaceEndpoint {
    Codex {
        remote: String,
    },
    /// Grok's socket has no way to say whose it is, so `runtime` is this home's
    /// Codex endpoint, where its runtime identifies itself.
    Grok {
        socket: PathBuf,
        runtime: String,
    },
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
                runtime: crate::DEFAULT_CODEX_REMOTE.to_string(),
            },
            #[cfg(not(unix))]
            InterfaceClient::Grok => unreachable!("refused off Unix above"),
        })
    }

    async fn answers(&self) -> bool {
        match self {
            Self::Codex { remote } => endpoint_answers(remote).await,
            #[cfg(unix)]
            Self::Grok { socket, .. } => tokio::net::UnixStream::connect(socket).await.is_ok(),
            #[cfg(not(unix))]
            Self::Grok { .. } => false,
        }
    }

    /// An endpoint answering proves nothing about whose it is: Codex's port is fixed
    /// per machine, and a stock Grok leader or another home's runtime can hold the
    /// per-machine Grok socket. This home's runtime has to say the endpoint is its own,
    /// and one with its Codex endpoint off cannot say, so it is refused the same way.
    async fn ensure_is_this_homes(&self, home: &Path, expected_did: &str) -> Result<()> {
        match self {
            Self::Codex { remote } => ensure_endpoint_is_this_homes(remote, expected_did).await,
            Self::Grok { socket, runtime } => {
                let holds_socket = runtime_identity_at(runtime).await.is_some_and(|identity| {
                    identity.node_did == expected_did
                        && identity.grok_socket.as_deref() == Some(socket.as_path())
                });
                ensure!(
                    holds_socket,
                    "the Grok socket {socket} is not held by this Gents home's runtime, so Grok did not open. Stop whatever holds it (`lsof {socket}` names it), restart this home's runtime with `gents service restart --home {home}` or whatever is running it, with its Codex endpoint on, then run `gents` again.",
                    socket = socket.display(),
                    home = home.display()
                );
                Ok(())
            }
        }
    }

    fn describe(&self) -> String {
        match self {
            Self::Codex { remote } => format!("Codex endpoint {remote}"),
            Self::Grok { socket, .. } => format!("Grok socket {}", socket.display()),
        }
    }

    async fn open(self, agent: Option<String>) -> Result<()> {
        match self {
            Self::Codex { remote } => {
                open_codex(CodexArgs {
                    remote,
                    remote_auth_token_env: None,
                    agent,
                    no_alt_screen: false,
                    prompt: None,
                })
                .await
            }
            Self::Grok { socket, .. } => {
                launch_interface(
                    "Grok",
                    &interface_bin(GROK_BIN_ENV, "grok"),
                    &grok_command_args(&socket, agent.as_deref()),
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
    agent: Option<String>,
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
    ensure_an_agent_can_run(
        crate::DEFAULT_CODEX_REMOTE,
        &home,
        agent.is_some(),
        PUBLISH_WAIT,
    )
    .await?;
    endpoint.open(agent).await
}

async fn ensure_runtime(
    home: &Path,
    expected_did: &str,
    endpoint: &InterfaceEndpoint,
) -> Result<()> {
    if endpoint.answers().await {
        return endpoint.ensure_is_this_homes(home, expected_did).await;
    }
    if let Some(state) = read_runtime_state(home)? {
        if graphql_endpoint_available(&state.graphql).await {
            return ensure_up_runtime_opens(home, expected_did, endpoint, ENDPOINT_WAIT).await;
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
    wait_for_endpoint(home, endpoint, ENDPOINT_WAIT).await?;
    endpoint.ensure_is_this_homes(home, expected_did).await
}

/// A runtime answers GraphQL before it opens its interface endpoints, so one still starting
/// looks like one with them off until this wait ends. Saying so sooner sends a person to
/// restart, and a restart starts the same window over.
async fn ensure_up_runtime_opens(
    home: &Path,
    expected_did: &str,
    endpoint: &InterfaceEndpoint,
    timeout: Duration,
) -> Result<()> {
    if wait_for_endpoint(home, endpoint, timeout).await.is_err() {
        bail!(
            "this home's runtime is up with its {} off. Restart it with `gents service restart --home {}`, or restart whatever is running it, then run `gents` again.",
            endpoint.describe(),
            home.display()
        );
    }
    endpoint.ensure_is_this_homes(home, expected_did).await
}

/// launchd and systemd report supervision, not readiness, and no cross-process notification of
/// readiness exists, so the endpoint answering is the only observation. A bounded, single-task
/// async retry blocks no runtime thread, the same shape as `service_update/execute.rs:330`.
async fn wait_for_endpoint(
    home: &Path,
    endpoint: &InterfaceEndpoint,
    timeout: Duration,
) -> Result<()> {
    let deadline = Instant::now() + timeout;
    while !endpoint.answers().await {
        if Instant::now() >= deadline {
            bail!(
                "the runtime did not open its {} within {} s; it was left running, check `gents service status --home {}`",
                endpoint.describe(),
                timeout.as_secs_f32(),
                home.display()
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

fn grok_command_args(socket: &Path, agent: Option<&str>) -> Vec<String> {
    let mut args = vec![
        "--leader".to_string(),
        "--leader-socket".to_string(),
        socket.display().to_string(),
    ];
    if let Some(agent) = agent {
        args.push("--agent".to_string());
        args.push(agent.to_string());
    }
    args
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
    use crate::commands::codex::tests::{identity, runtime_answering, runtime_answering_on};

    #[tokio::test]
    async fn wait_for_endpoint_gives_up_naming_gents_service_status() {
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = closed.local_addr().unwrap().port();
        drop(closed);
        let endpoint = InterfaceEndpoint::Codex {
            remote: format!("ws://127.0.0.1:{port}/"),
        };

        let error = wait_for_endpoint(
            Path::new("/srv/home"),
            &endpoint,
            Duration::from_millis(200),
        )
        .await
        .expect_err("a closed port never answers");

        assert!(
            error
                .to_string()
                .contains("gents service status --home /srv/home"),
            "{error:#}"
        );
    }

    /// An endpoint on a free port whose `/runtime` starts answering as `node_did` after a moment.
    async fn endpoint_answering_late(node_did: &str) -> InterfaceEndpoint {
        let free = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let authority = free.local_addr().unwrap();
        drop(free);
        let body = runtime_holding(node_did, None);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(300)).await;
            let listener = tokio::net::TcpListener::bind(authority).await.unwrap();
            runtime_answering_on(listener, vec![body]);
        });
        InterfaceEndpoint::Codex {
            remote: format!("ws://{authority}/"),
        }
    }

    #[tokio::test]
    async fn a_runtime_still_starting_is_waited_for_then_identified() {
        let endpoint = endpoint_answering_late("mine").await;

        ensure_up_runtime_opens(
            Path::new("/srv/home"),
            "mine",
            &endpoint,
            Duration::from_secs(30),
        )
        .await
        .expect("the endpoint opens within the wait");
    }

    #[tokio::test]
    async fn an_endpoint_that_opens_late_still_has_to_be_this_homes() {
        let endpoint = endpoint_answering_late("other").await;

        let error = ensure_up_runtime_opens(
            Path::new("/srv/home"),
            "mine",
            &endpoint,
            Duration::from_secs(30),
        )
        .await
        .expect_err("another home's runtime answers");

        assert!(
            error
                .to_string()
                .contains("does not belong to this Gents home"),
            "{error:#}"
        );
    }

    #[tokio::test]
    async fn an_endpoint_that_stays_off_says_restart_after_the_wait() {
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = closed.local_addr().unwrap().port();
        drop(closed);
        let endpoint = InterfaceEndpoint::Codex {
            remote: format!("ws://127.0.0.1:{port}/"),
        };

        let error = ensure_up_runtime_opens(
            Path::new("/srv/home"),
            "mine",
            &endpoint,
            Duration::from_millis(200),
        )
        .await
        .expect_err("nothing ever answers");

        assert!(
            error
                .to_string()
                .contains("gents service restart --home /srv/home"),
            "{error:#}"
        );
    }

    fn grok_endpoint(runtime: &str) -> InterfaceEndpoint {
        InterfaceEndpoint::Grok {
            socket: PathBuf::from("/tmp/held.sock"),
            runtime: format!("ws://{runtime}/"),
        }
    }

    fn runtime_holding(node_did: &str, socket: Option<&str>) -> serde_json::Value {
        let mut identity = identity(Some(&["a"]));
        identity.node_did = node_did.to_string();
        identity.grok_socket = socket.map(PathBuf::from);
        serde_json::to_value(identity).unwrap()
    }

    #[tokio::test]
    async fn only_this_homes_runtime_holding_the_socket_opens_grok() {
        let runtime = runtime_answering(vec![runtime_holding("mine", Some("/tmp/held.sock"))]);

        grok_endpoint(&runtime.await)
            .ensure_is_this_homes(Path::new("/srv/home"), "mine")
            .await
            .expect("this home's runtime holds the socket");
    }

    #[tokio::test]
    async fn anything_else_on_the_grok_socket_gets_one_refusal() {
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let nothing_answers = closed.local_addr().unwrap().to_string();
        drop(closed);
        let mut refusals = Vec::new();
        for runtime in [
            nothing_answers,
            runtime_answering(vec![runtime_holding("other", Some("/tmp/held.sock"))]).await,
            runtime_answering(vec![runtime_holding("mine", None)]).await,
            runtime_answering(vec![runtime_holding("mine", Some("/tmp/another.sock"))]).await,
        ] {
            let refusal = grok_endpoint(&runtime)
                .ensure_is_this_homes(Path::new("/srv/home"), "mine")
                .await
                .expect_err(&runtime)
                .to_string();
            refusals.push(refusal);
        }

        assert!(refusals[0].contains("/tmp/held.sock"), "{}", refusals[0]);
        assert!(
            refusals[0].contains("gents service restart --home /srv/home"),
            "{}",
            refusals[0]
        );
        assert!(
            refusals.iter().all(|refusal| refusal == &refusals[0]),
            "{refusals:?}"
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
            grok_command_args(Path::new("/tmp/leader.sock"), None),
            ["--leader", "--leader-socket", "/tmp/leader.sock"]
        );
    }

    #[test]
    fn grok_args_carry_the_agent() {
        assert_eq!(
            grok_command_args(Path::new("/tmp/leader.sock"), Some("did:key:z:other")),
            [
                "--leader",
                "--leader-socket",
                "/tmp/leader.sock",
                "--agent",
                "did:key:z:other"
            ]
        );
    }
}
