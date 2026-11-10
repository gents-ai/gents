use std::io::IsTerminal as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, ensure, Context, Result};
use clap::CommandFactory as _;
use gents::store::config_client::GraphqlEndpoint;
use tokio::runtime::Handle;

use crate::cli::args::{CodexArgs, ShellArgs, StoreKeyCustodyArg};
use crate::cli::Cli;
use crate::client_settings::{ClientSettings, InterfaceClient};
use crate::commands::codex::{
    endpoint_answers, ensure_an_agent_can_run, ensure_endpoint_is_this_homes, open_codex,
    wait_for_agent_to_publish, PUBLISH_WAIT,
};
use crate::graphql_access::graphql_endpoint_available;
use crate::home_state::{read_finished_init_config, read_runtime_state, resolve_home_dir};
use crate::native_service::{NativeServiceConfig, NativeServiceManager, NativeServicePlatform};
use crate::shared::StoredInitConfig;

mod setup;
mod yolo;

const ENDPOINT_WAIT: Duration = Duration::from_secs(60);
/// A runtime publishes an agent it did not have at start once it reconciles the new
/// documents, and nothing tells another process when that has happened.
const YOLO_PUBLISH_WAIT: Duration = Duration::from_secs(30);

enum InterfaceEndpoint {
    /// Waits for the same endpoint as Codex: the shell reads this home's runtime there.
    Shell {
        remote: String,
    },
    Codex {
        remote: String,
    },
}

impl InterfaceEndpoint {
    fn for_client(client: InterfaceClient) -> Self {
        match client {
            InterfaceClient::Shell => Self::Shell {
                remote: crate::DEFAULT_CODEX_REMOTE.to_string(),
            },
            InterfaceClient::Codex => Self::Codex {
                remote: crate::DEFAULT_CODEX_REMOTE.to_string(),
            },
        }
    }

    async fn answers(&self) -> bool {
        match self {
            Self::Shell { remote } | Self::Codex { remote } => endpoint_answers(remote).await,
        }
    }

    /// An endpoint answering proves nothing about whose it is: Codex's port is fixed
    /// per machine. This home's runtime has to say the endpoint is its own, and one with
    /// its Codex endpoint off cannot say, so it is refused the same way.
    async fn ensure_is_this_homes(&self, expected_did: &str) -> Result<()> {
        match self {
            Self::Shell { remote } | Self::Codex { remote } => {
                ensure_endpoint_is_this_homes(remote, expected_did).await
            }
        }
    }

    /// Where this home's runtime says who it is and which agents can run.
    fn remote(&self) -> &str {
        match self {
            Self::Shell { remote } | Self::Codex { remote } => remote,
        }
    }

    fn describe(&self) -> String {
        match self {
            Self::Shell { remote } => format!("runtime endpoint {remote}"),
            Self::Codex { remote } => format!("Codex endpoint {remote}"),
        }
    }

    async fn open(
        self,
        home: &Path,
        agent: Option<String>,
        prompt: Option<String>,
        resume: Option<String>,
        runtime: Handle,
    ) -> Result<()> {
        match self {
            Self::Shell { remote } => {
                crate::commands::shell::open(home, &remote, agent, prompt, resume, runtime).await
            }
            Self::Codex { remote } => {
                open_codex(CodexArgs {
                    remote,
                    remote_auth_token_env: None,
                    agent,
                    no_alt_screen: false,
                    prompt,
                    resume,
                })
                .await
            }
        }
    }
}

pub(crate) struct StartArgs {
    pub(crate) home: Option<PathBuf>,
    pub(crate) store_key_custody: Option<StoreKeyCustodyArg>,
    pub(crate) agent: Option<String>,
    pub(crate) print: Option<String>,
    pub(crate) prompt: Option<String>,
    pub(crate) continue_session: Option<String>,
    pub(crate) yolo: bool,
}

/// Dispatched before telemetry, so nothing writes over the interface.
pub(crate) async fn run(mut args: StartArgs, runtime: Handle) -> Result<()> {
    // Setup prompts go to stderr, so a redirected stderr would hide them.
    let terminal = std::io::stdin().is_terminal()
        && std::io::stdout().is_terminal()
        && std::io::stderr().is_terminal();
    if !terminal && args.print.is_none() {
        eprint!("{}", Cli::command().render_help());
        std::process::exit(2);
    }
    let home_given = args.home.take().map(crate::absolutize).transpose()?;
    let (home, config) = ready_home(home_given.clone(), args.store_key_custody, terminal).await?;
    let Some(message) = args.print.take() else {
        let client = ClientSettings::read(&home)?.client;
        ensure!(
            !refuses_continue_with_a_prompt(client, &args),
            "{}",
            shell_cannot_continue_with_a_prompt()
        );
        let endpoint = InterfaceEndpoint::for_client(client);
        ensure_runtime(&home, &config.node_did, &endpoint).await?;
        let (agent, made) = chosen_agent(&home, &config, &args).await?;
        if let (true, Some(agent)) = (made, agent.as_deref()) {
            wait_for_agent_to_publish(crate::DEFAULT_CODEX_REMOTE, agent, YOLO_PUBLISH_WAIT).await;
        }
        ensure_an_agent_can_run(endpoint.remote(), &home, agent.is_some(), PUBLISH_WAIT).await?;
        let opened_at = chrono::Utc::now();
        let opened = endpoint
            .open(
                &home,
                agent.clone(),
                args.prompt,
                args.continue_session,
                runtime,
            )
            .await;
        print_continue_hint(
            &home,
            &config.node_did,
            agent.as_deref(),
            opened_at,
            home_given.as_deref(),
        )
        .await;
        return opened;
    };
    let graphql = ensure_graphql(&home).await?;
    let (agent, _) = chosen_agent(&home, &config, &args).await?;
    crate::commands::chat::print_reply(
        &home,
        &graphql,
        &config.node_did,
        agent.as_deref(),
        args.continue_session.as_deref(),
        &message,
    )
    .await
}

/// The agent a run talks to, and whether this run made it: a continued session's own agent,
/// the yolo agent, or `--agent`, else the node's default.
async fn chosen_agent(
    home: &Path,
    config: &crate::shared::StoredInitConfig,
    args: &StartArgs,
) -> Result<(Option<String>, bool)> {
    if args.continue_session.is_none() && !args.yolo {
        return Ok((args.agent.clone(), false));
    }
    let graphql = wait_for_graphql(home).await?;
    if let Some(session) = &args.continue_session {
        let agent = session_agent(&graphql, &config.node_did, session).await?;
        return Ok((Some(agent), false));
    }
    let (agent, made) = yolo::ensure_agent(home, config, &graphql).await?;
    Ok((Some(agent), made))
}

/// Interfaces keep a session's history in Gents, so the newest session the window's opening
/// agent took a turn in can be reopened with `--continue`. A shell tab on another agent is not
/// counted. Best effort: a failed read prints nothing.
async fn print_continue_hint(
    home: &Path,
    node_did: &str,
    agent: Option<&str>,
    opened_at: chrono::DateTime<chrono::Utc>,
    home_flag: Option<&Path>,
) {
    let Ok(Some(graphql)) = answering_graphql(home).await else {
        return;
    };
    let access = crate::config_writes::ConfigAccess::Graphql(graphql);
    let agent = match agent {
        Some(agent) => agent.to_owned(),
        None => match yolo::default_agent(&access, node_did).await {
            Ok(agent) => agent,
            Err(_) => return,
        },
    };
    let latest = crate::graphql_rows(
        &access,
        "AgentRequest",
        &format!(
            r#"{{ AgentRequest(filter: {{node_did: {{_eq: "{}"}}, agent_did: {{_eq: "{}"}}}}, order: {{created_at: DESC}}, limit: 1) {{ session_id created_at }} }}"#,
            gents::store::graphql::escape_graphql_string(node_did),
            gents::store::graphql::escape_graphql_string(&agent)
        ),
    )
    .await;
    let Ok(rows) = latest else {
        return;
    };
    if let Some(hint) = continue_hint(rows.first(), opened_at, home_flag) {
        eprintln!("{hint}");
    }
}

fn continue_hint(
    latest: Option<&serde_json::Value>,
    opened_at: chrono::DateTime<chrono::Utc>,
    home_flag: Option<&Path>,
) -> Option<String> {
    let latest = latest?;
    let created = chrono::DateTime::parse_from_rfc3339(latest["created_at"].as_str()?).ok()?;
    if created < opened_at {
        return None;
    }
    let session = shlex::try_quote(latest["session_id"].as_str()?).ok()?;
    let mut command = format!("gents --continue {session}");
    if let Some(home) = home_flag {
        command.push_str(&format!(" --home {}", crate::shell_word(home)));
    }
    Some(format!("Pick this up again with: {command}"))
}

/// The shell opens a continued session in its first tab and sends a prompt from a new one, so
/// the two together would split the turn from its session.
fn refuses_continue_with_a_prompt(client: InterfaceClient, args: &StartArgs) -> bool {
    client == InterfaceClient::Shell && args.continue_session.is_some() && args.prompt.is_some()
}

fn shell_cannot_continue_with_a_prompt() -> &'static str {
    "The shell opens the session for you to type in. Leave PROMPT out, or send it with `gents --continue SESSION_ID -p MESSAGE`."
}

async fn session_agent(graphql: &GraphqlEndpoint, node_did: &str, session: &str) -> Result<String> {
    let owner = gents::ConfigAccess::Graphql(graphql.clone())
        .transact_readonly("cli.start.session_agent", |txn| {
            Box::pin(async move {
                gents::store::config_client::agent_session::load_agent_session_owner_in_txn(
                    txn, node_did, session,
                )
                .await
            })
        })
        .await?
        .with_context(|| {
            format!("no session {session} on this home; `gents sessions list` shows them")
        })?;
    let agent = owner.session.agent_did;
    ensure!(
        !agent.trim().is_empty(),
        "session {session} records no agent to continue it with"
    );
    Ok(agent)
}

/// Dispatched before telemetry, so nothing writes over the shell.
pub(crate) async fn shell(args: ShellArgs, runtime: Handle) -> Result<()> {
    let (home, config) = ready_home(args.home, args.store_key_custody, true).await?;
    let overridden = args.codex_remote.is_some();
    let endpoint = InterfaceEndpoint::Shell {
        remote: args
            .codex_remote
            .unwrap_or_else(|| crate::DEFAULT_CODEX_REMOTE.to_string()),
    };
    if overridden {
        ensure_override_answers(&endpoint, &home).await?;
        // The general refusal says to stop whatever holds the endpoint, which on a mistyped
        // override can be another home's runtime.
        if endpoint
            .ensure_is_this_homes(&config.node_did)
            .await
            .is_err()
        {
            bail!(
                "{} answers, but it is not this home's runtime; point --codex-remote at the --codex-shim-port this home's `gents runtime server` uses",
                endpoint.remote()
            );
        }
    } else {
        ensure_runtime(&home, &config.node_did, &endpoint).await?;
    }
    ensure_an_agent_can_run(endpoint.remote(), &home, false, PUBLISH_WAIT).await?;
    endpoint.open(&home, None, None, None, runtime).await
}

/// Setup asks its questions on the terminal, so a home that needs it and has none is refused.
async fn ready_home(
    home: Option<PathBuf>,
    custody: Option<StoreKeyCustodyArg>,
    terminal: bool,
) -> Result<(PathBuf, StoredInitConfig)> {
    let home_given = home.map(crate::absolutize).transpose()?;
    let mut home = crate::absolutize(resolve_home_dir(home_given.as_deref()))?;
    let config = match read_finished_init_config(&home)? {
        Some(config) => config,
        None => {
            ensure!(
                terminal,
                "{} is not set up yet; run `gents` in a terminal to set it up, then run this again",
                crate::shell_word(&home)
            );
            home = setup::setup(home_given, custody).await?;
            read_finished_init_config(&home)?.context("setup left this home uninitialized")?
        }
    };
    Ok((home, config))
}

/// `-p` needs only GraphQL, so a runtime with its interface endpoints off still answers it.
async fn ensure_graphql(home: &Path) -> Result<GraphqlEndpoint> {
    if let Some(graphql) = answering_graphql(home).await? {
        return Ok(graphql);
    }
    start_service(home).await?;
    wait_for_graphql(home).await
}

/// A runtime records where its GraphQL answers a little after its interface endpoints open,
/// and nothing tells another process when, so this bounded wait reads that record again.
async fn wait_for_graphql(home: &Path) -> Result<GraphqlEndpoint> {
    if let Some(graphql) = answering_graphql(home).await? {
        return Ok(graphql);
    }
    eprintln!(
        "Waiting for this home's runtime to answer (up to {} s).",
        ENDPOINT_WAIT.as_secs()
    );
    let deadline = Instant::now() + ENDPOINT_WAIT;
    loop {
        if let Some(graphql) = answering_graphql(home).await? {
            return Ok(graphql);
        }
        if Instant::now() >= deadline {
            bail!(
                "the runtime did not answer within {} s; it was left running, check `gents runtime service status --home {}`",
                ENDPOINT_WAIT.as_secs(),
                crate::shell_word(home)
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn answering_graphql(home: &Path) -> Result<Option<GraphqlEndpoint>> {
    match read_runtime_state(home)? {
        Some(state) if graphql_endpoint_available(&state.graphql).await => Ok(Some(
            crate::resolve_graphql_endpoint(Some(&state.graphql), Some(home))?,
        )),
        _ => Ok(None),
    }
}

/// An overridden endpoint is one a person started by hand, so nothing is started here.
/// `ensure_runtime` installs a service definition under the one per-user label
/// (`native_service::SERVICE_LABEL`) for the first home that asks, and the service manager
/// then refuses every other home.
async fn ensure_override_answers(endpoint: &InterfaceEndpoint, home: &Path) -> Result<()> {
    let remote = endpoint.remote();
    let port = url::Url::parse(remote)
        .ok()
        .and_then(|url| url.port())
        .with_context(|| format!("{remote} names no port"))?;
    ensure!(
        endpoint.answers().await,
        "nothing answers at {remote}; start this home's runtime there first with `gents runtime server --home {home} --http-port <free port> --codex-shim-port {port}`, then run `gents shell` again",
        home = crate::shell_word(home)
    );
    Ok(())
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
            return ensure_up_runtime_opens(home, expected_did, endpoint, ENDPOINT_WAIT).await;
        }
    }
    start_service(home).await?;
    wait_for_endpoint(home, endpoint, ENDPOINT_WAIT).await?;
    endpoint.ensure_is_this_homes(expected_did).await
}

/// Starts this home's native service, installing it first if it is not installed.
async fn start_service(home: &Path) -> Result<()> {
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
    .map_err(|error| anyhow!("{error:#}\n{hint}"))
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
            "this home's runtime is up, but its {} has not opened. Check `gents status --home {home}`: a missing agent keeps it closed, and a restart will not help that. Otherwise restart it with `gents runtime service restart --home {home}`, or restart whatever is running it, then run `gents` again.",
            endpoint.describe(),
            home = crate::shell_word(home)
        );
    }
    endpoint.ensure_is_this_homes(expected_did).await
}

/// launchd and systemd report supervision, not readiness, and no cross-process notification of
/// readiness exists, so the endpoint answering is the only observation. A bounded, single-task
/// async retry blocks no runtime thread, the same shape as `service_update/execute.rs:330`.
async fn wait_for_endpoint(
    home: &Path,
    endpoint: &InterfaceEndpoint,
    timeout: Duration,
) -> Result<()> {
    eprintln!(
        "Waiting for this home's runtime to open its {} (up to {} s).",
        endpoint.describe(),
        timeout.as_secs()
    );
    let deadline = Instant::now() + timeout;
    while !endpoint.answers().await {
        if Instant::now() >= deadline {
            bail!(
                "the runtime did not open its {} within {} s; it was left running, check `gents runtime service status --home {}`",
                endpoint.describe(),
                timeout.as_secs_f32(),
                crate::shell_word(home)
            );
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Ok(())
}

fn no_service_manager_message(home: &Path) -> String {
    format!(
        "run `gents runtime server --home {}` in another terminal, then `gents`",
        crate::shell_word(home)
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::codex::tests::{identity, runtime_answering, runtime_answering_on};
    use crate::commands::eval::testing::test_runtime_handle;

    #[tokio::test]
    async fn an_override_that_does_not_answer_names_gents_server_and_starts_nothing() {
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = closed.local_addr().unwrap().port();
        drop(closed);
        let endpoint = InterfaceEndpoint::Shell {
            remote: format!("ws://127.0.0.1:{port}/"),
        };

        let error = ensure_override_answers(&endpoint, Path::new("/srv/home"))
            .await
            .expect_err("nothing answers on a closed port");

        let error = error.to_string();
        assert!(
            error.contains(&format!(
                "`gents runtime server --home /srv/home --http-port <free port> --codex-shim-port {port}`"
            )),
            "{error}"
        );
        assert!(!error.contains("service"), "{error}");
    }

    /// A home `init` finished, whose recorded runtime answers GraphQL. Were `shell` to reach
    /// `ensure_runtime`, it would wait on that runtime instead of installing or starting the
    /// machine's one service job, which a test build shares with the user's daily runtime.
    async fn initialized_home(node_did: &str) -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        let path = home.path().to_string_lossy().into_owned();
        crate::home_state::write_init_config(
            home.path(),
            &StoredInitConfig {
                home: path.clone(),
                node_name: "fixture".into(),
                node_did: node_did.into(),
                key_path: None,
                identity_backend: None,
                keychain_label: None,
                secure_enclave_label: None,
                tool_package: None,
                tool_ceiling: crate::cli::args::ToolCeilingArg::MetaOnly,
                tool_root: None,
                store_encryption: None,
            },
        )
        .unwrap();
        let graphql = runtime_answering(vec![serde_json::json!({})]).await;
        crate::home_state::write_runtime_state(
            home.path(),
            &crate::shared::StoredRuntimeState {
                home: path,
                graphql: format!("http://{graphql}/api/v0/graphql"),
                node_name: "fixture".into(),
                node_did: node_did.into(),
                default_agent_did: "a".into(),
                p2p_transport: "none".into(),
                p2p_peer_id: None,
                p2p_listen_addresses: Vec::new(),
                p2p_admission: None,
            },
        )
        .unwrap();
        home
    }

    fn shell_args(home: &Path, remote: &str) -> ShellArgs {
        ShellArgs {
            home: Some(home.to_path_buf()),
            store_key_custody: None,
            codex_remote: Some(remote.to_string()),
        }
    }

    #[tokio::test]
    async fn the_shell_with_a_silent_override_never_reaches_ensure_runtime() {
        let home = initialized_home("mine").await;
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let remote = format!("ws://{}/", closed.local_addr().unwrap());
        drop(closed);

        let error = shell(shell_args(home.path(), &remote), test_runtime_handle())
            .await
            .expect_err("nothing answers on the override");

        let error = format!("{error:#}");
        assert!(
            error.starts_with(&format!("nothing answers at {remote}; ")),
            "{error}"
        );
    }

    #[tokio::test]
    async fn an_override_held_by_another_runtime_names_this_homes_port_and_stops_nothing() {
        let home = initialized_home("mine").await;
        let other = runtime_answering(vec![runtime_holding("other")]).await;
        let remote = format!("ws://{other}/");

        let error = shell(shell_args(home.path(), &remote), test_runtime_handle())
            .await
            .expect_err("another home's runtime answers");

        assert_eq!(
            format!("{error:#}"),
            format!("{remote} answers, but it is not this home's runtime; point --codex-remote at the --codex-shim-port this home's `gents runtime server` uses")
        );
    }

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
                .contains("gents runtime service status --home /srv/home"),
            "{error:#}"
        );
    }

    /// An endpoint on a free port whose `/runtime` starts answering as `node_did` after a moment.
    async fn endpoint_answering_late(node_did: &str) -> InterfaceEndpoint {
        let free = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let authority = free.local_addr().unwrap();
        drop(free);
        let body = runtime_holding(node_did);
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

        let error = error.to_string();
        assert!(error.contains("could not confirm"), "{error}");
        assert!(error.contains("gents --home /srv/home"), "{error}");
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

        let error = error.to_string();
        assert!(!error.contains(" off"), "{error}");
        let status = error.find("gents status --home /srv/home").expect(&error);
        let restart = error
            .find("gents runtime service restart --home /srv/home")
            .expect(&error);
        assert!(status < restart, "{error}");
    }

    fn runtime_holding(node_did: &str) -> serde_json::Value {
        let mut identity = identity(Some(&["a"]));
        identity.node_did = node_did.to_string();
        serde_json::to_value(identity).unwrap()
    }

    #[tokio::test]
    async fn printed_commands_quote_a_home_with_spaces() {
        let home = Path::new("/srv/my home");
        let closed = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = closed.local_addr().unwrap().port();
        drop(closed);
        let codex = InterfaceEndpoint::Codex {
            remote: format!("ws://127.0.0.1:{port}/"),
        };

        for (message, command) in [
            (
                no_service_manager_message(home),
                "gents runtime server --home '/srv/my home'",
            ),
            (
                wait_for_endpoint(home, &codex, Duration::ZERO)
                    .await
                    .unwrap_err()
                    .to_string(),
                "gents runtime service status --home '/srv/my home'",
            ),
            (
                ensure_up_runtime_opens(home, "mine", &codex, Duration::ZERO)
                    .await
                    .unwrap_err()
                    .to_string(),
                "gents runtime service restart --home '/srv/my home'",
            ),
        ] {
            assert!(message.contains(command), "{message}");
        }
    }

    #[test]
    fn the_default_interface_is_the_built_in_shell_on_the_runtime_endpoint() {
        let endpoint = InterfaceEndpoint::for_client(InterfaceClient::default());
        let InterfaceEndpoint::Shell { remote } = &endpoint else {
            panic!("expected the shell");
        };
        assert_eq!(remote, crate::DEFAULT_CODEX_REMOTE);
        assert!(endpoint.describe().contains(crate::DEFAULT_CODEX_REMOTE));
    }

    #[test]
    fn no_service_manager_says_run_gents_server_then_gents() {
        assert_eq!(
            no_service_manager_message(Path::new("/srv/home")),
            "run `gents runtime server --home /srv/home` in another terminal, then `gents`"
        );
    }

    #[test]
    fn the_continue_hint_names_the_last_session_since_the_window_opened() {
        let opened = chrono::DateTime::parse_from_rfc3339("2026-11-05T10:00:00+00:00")
            .unwrap()
            .with_timezone(&chrono::Utc);
        let row = |at: &str| serde_json::json!({"session_id": "s1", "created_at": at});
        let after = row("2026-11-05T10:00:05.5+00:00");
        assert_eq!(
            continue_hint(Some(&after), opened, None).as_deref(),
            Some("Pick this up again with: gents --continue s1")
        );
        assert_eq!(
            continue_hint(Some(&after), opened, Some(Path::new("/srv/my home"))).as_deref(),
            Some("Pick this up again with: gents --continue s1 --home '/srv/my home'")
        );
        let before = row("2026-11-05T09:59:59+00:00");
        assert_eq!(continue_hint(Some(&before), opened, None), None);
        assert_eq!(continue_hint(None, opened, None), None);
    }

    #[test]
    fn only_the_shell_refuses_continue_with_a_prompt() {
        let args = |continuing: bool, prompt: bool| StartArgs {
            home: None,
            store_key_custody: None,
            agent: None,
            print: None,
            prompt: prompt.then(|| "hello".to_string()),
            continue_session: continuing.then(|| "s1".to_string()),
            yolo: false,
        };
        let shell = InterfaceClient::Shell;
        assert!(refuses_continue_with_a_prompt(shell, &args(true, true)));
        assert!(!refuses_continue_with_a_prompt(shell, &args(true, false)));
        assert!(!refuses_continue_with_a_prompt(shell, &args(false, true)));
        let codex = InterfaceClient::Codex;
        assert!(!refuses_continue_with_a_prompt(codex, &args(true, true)));
        let message = shell_cannot_continue_with_a_prompt();
        assert!(
            message.contains("gents --continue SESSION_ID -p MESSAGE"),
            "{message}"
        );
    }
}
