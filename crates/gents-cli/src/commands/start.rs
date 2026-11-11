use std::io::IsTerminal as _;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, ensure, Context, Result};
use clap::CommandFactory as _;
use gents::store::config_client::GraphqlEndpoint;
use gents::store::local::storage_backend::{IncompatibleStore, IncompatibleStoreKind};
use tokio::runtime::Handle;

use crate::cli::args::{ShellArgs, StoreKeyCustodyArg};
use crate::cli::Cli;
use crate::config_writes::ConfigAccess;
use crate::graphql_access::graphql_endpoint_available;
use crate::home_state::{read_finished_init_config, read_runtime_state, resolve_home_dir};
use crate::native_service::{
    NativeServiceConfig, NativeServiceManager, NativeServicePlatform, ServiceExit,
};
use crate::shared::StoredInitConfig;
use gents_protocol::node_readiness::{
    AgentReadinessState, AgentReadinessUnavailableReason, NodeReadinessSnapshot,
};

mod setup;
mod yolo;

pub(crate) use yolo::default_agent;
pub(crate) use yolo::node_readiness;

const ENDPOINT_WAIT: Duration = Duration::from_secs(60);
/// A runtime publishes the agents it can run a little after its GraphQL answers.
const PUBLISH_WAIT: Duration = Duration::from_secs(5);

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
        report_ignored_client_setting(&home);
        ensure!(
            !refuses_continue_with_a_prompt(&args),
            "{}",
            shell_cannot_continue_with_a_prompt()
        );
        let graphql = ensure_graphql(&home).await?;
        let agent = chosen_agent(&home, &config, &args).await?;
        wait_for_its_agent(&graphql, &config.node_did, agent.as_deref(), PUBLISH_WAIT).await;
        let shown =
            crate::commands::shell::open(&home, agent, args.prompt, args.continue_session, runtime)
                .await?;
        if let Some(hint) = continue_hint(shown.as_deref(), home_given.as_deref()) {
            eprintln!("{hint}");
        }
        return Ok(());
    };
    let graphql = ensure_graphql(&home).await?;
    let agent = chosen_agent(&home, &config, &args).await?;
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

/// The agent a run talks to: a continued session's own agent, the yolo agent, or `--agent`,
/// else the node's default.
async fn chosen_agent(
    home: &Path,
    config: &crate::shared::StoredInitConfig,
    args: &StartArgs,
) -> Result<Option<String>> {
    if args.continue_session.is_none() && !args.yolo {
        return Ok(args.agent.clone());
    }
    let graphql = wait_for_graphql(home).await?;
    if let Some(session) = &args.continue_session {
        return Ok(Some(
            session_agent(&graphql, &config.node_did, session).await?,
        ));
    }
    Ok(Some(yolo::ensure_agent(home, config, &graphql).await?))
}

/// `gents config client set` once chose between the built-in shell and Codex in this file.
/// Codex is gone, so a file left behind is named and otherwise ignored.
fn report_ignored_client_setting(home: &Path) {
    let path = home.join(gents::store::local::home::CLIENT_CONFIG_FILE_NAME);
    if path.exists() {
        eprintln!("{}", ignored_client_setting_notice(&path));
    }
}

fn ignored_client_setting_notice(path: &Path) -> String {
    format!(
        "Ignoring {}: `gents` now opens only the built-in shell. Delete the file to stop this notice.",
        crate::shell_word(path)
    )
}

/// The shell keeps a session's history in Gents, so the session its center pane showed at quit
/// can be reopened with `--continue`.
fn continue_hint(session: Option<&str>, home_flag: Option<&Path>) -> Option<String> {
    let session = shlex::try_quote(session?).ok()?;
    let mut command = format!("gents --continue {session}");
    if let Some(home) = home_flag {
        command.push_str(&format!(" --home {}", crate::shell_word(home)));
    }
    Some(format!("Pick this up again with: {command}"))
}

/// The shell opens a continued session in its first tab and sends a prompt from a new one, so
/// the two together would split the turn from its session.
fn refuses_continue_with_a_prompt(args: &StartArgs) -> bool {
    args.continue_session.is_some() && args.prompt.is_some()
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
    let graphql = ensure_graphql(&home).await?;
    wait_for_its_agent(&graphql, &config.node_did, None, PUBLISH_WAIT).await;
    let shown = crate::commands::shell::open(&home, None, None, None, runtime).await?;
    if let Some(hint) = continue_hint(shown.as_deref(), None) {
        eprintln!("{hint}");
    }
    Ok(())
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

/// Starts this home's runtime when its GraphQL does not answer.
async fn ensure_graphql(home: &Path) -> Result<GraphqlEndpoint> {
    if let Some(graphql) = answering_graphql(home).await? {
        return Ok(graphql);
    }
    start_service(home).await?;
    or_store_refusal(home, wait_for_graphql(home).await).await
}

/// A service that refuses to open its home's store says why only in its own log and exits
/// with the refusal's status. A wait that ran out on such a home reports that refusal, typed,
/// so this process prints it and exits as the service did.
async fn or_store_refusal<T>(home: &Path, waited: Result<T>) -> Result<T> {
    match waited {
        Ok(value) => Ok(value),
        Err(error) => Err(match service_store_refusal(home).await {
            Some(store) => anyhow::Error::new(store),
            None => error,
        }),
    }
}

async fn service_store_refusal(home: &Path) -> Option<IncompatibleStore> {
    let service_home = home.to_path_buf();
    let exit = tokio::task::spawn_blocking(move || -> Result<Option<ServiceExit>> {
        let executable = std::env::current_exe().context("resolving the gents executable")?;
        NativeServiceManager::new(NativeServiceConfig::new(service_home, executable)?)?.last_exit()
    })
    .await
    .ok()?
    .ok()??;
    store_refusal(home, &exit)
}

fn store_refusal(home: &Path, exit: &ServiceExit) -> Option<IncompatibleStore> {
    let kind = exit.refused_store()?;
    let data_path = match kind {
        IncompatibleStoreKind::InsecureKey => home.join("keys"),
        _ => gents::store::local::home::default_data_dir(home),
    };
    Some(IncompatibleStore { kind, data_path })
}

/// A runtime records where its GraphQL answers once it serves, and nothing tells another
/// process when, so this bounded wait reads that record again.
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

/// Only the NodeReadiness document a runtime publishes tells another process which agents it
/// can run, and it lands a little after GraphQL answers, so this bounded wait reads it again.
/// The shell opens either way: with no agent that can run it opens on Setup, which connects a
/// model, and it reports a read that fails.
async fn wait_for_its_agent(
    graphql: &GraphqlEndpoint,
    node_did: &str,
    agent: Option<&str>,
    wait: Duration,
) {
    let access = ConfigAccess::Graphql(graphql.clone());
    let deadline = Instant::now() + wait;
    loop {
        let Ok(snapshot) = yolo::node_readiness(&access, node_did).await else {
            return;
        };
        if can_run(snapshot.as_ref(), agent) || Instant::now() >= deadline {
            return;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

/// Whether the agent a tab opens on is ready: the one `--agent` names, else the default.
fn can_run(snapshot: Option<&NodeReadinessSnapshot>, agent: Option<&str>) -> bool {
    snapshot.is_some_and(|snapshot| {
        let agent = agent.unwrap_or(&snapshot.default_agent_did);
        snapshot
            .agents
            .iter()
            .any(|entry| entry.state == AgentReadinessState::Ready && entry.agent_did == agent)
    })
}

/// The reasons an agent cannot run that connecting a model clears, which the shell's Setup
/// and Accounts menu do.
pub(crate) fn wants_a_model(reason: AgentReadinessUnavailableReason) -> bool {
    use AgentReadinessUnavailableReason as Reason;
    matches!(
        reason,
        Reason::BackendNotConfigured
            | Reason::BackendDisabled
            | Reason::BackendTemporarilyUnavailable
            | Reason::CredentialsRequired
            | Reason::SubscriptionNotEntitled
            | Reason::InferenceProfileInvalid
    )
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
    use gents_protocol::node_readiness::{AgentReadinessEntry, NodeReadinessProcessState};

    fn readiness(default: &str, ready: &[&str]) -> NodeReadinessSnapshot {
        NodeReadinessSnapshot {
            format_version: 1,
            process_state: NodeReadinessProcessState::Ready,
            active_generation: 1,
            router_generation: 1,
            default_agent_did: default.to_string(),
            agents: ready
                .iter()
                .map(|agent| AgentReadinessEntry {
                    agent_did: agent.to_string(),
                    state: AgentReadinessState::Ready,
                    reason: None,
                })
                .collect(),
        }
    }

    #[test]
    fn a_service_that_exited_refusing_the_store_is_reported_as_that_refusal() {
        let home = Path::new("/srv/home");
        let exit = |code| ServiceExit {
            reason: format!("exited with code {code}"),
            restarts: 0,
            clean: false,
            code: Some(code),
        };

        let older = store_refusal(
            home,
            &exit(crate::native_service::INCOMPATIBLE_STORE_EXIT_CODE),
        )
        .expect("an older home");
        assert_eq!(older.kind, IncompatibleStoreKind::UnknownLineage);
        assert_eq!(
            older.data_path,
            gents::store::local::home::default_data_dir(home)
        );
        let error = anyhow::Error::new(older.clone());
        assert_eq!(
            gents::store::local::storage_backend::incompatible_store(error.as_ref(), Path::new("")),
            Some(older)
        );

        let key = store_refusal(home, &exit(crate::native_service::INSECURE_KEY_EXIT_CODE))
            .expect("an unsafe key");
        assert_eq!(key.kind, IncompatibleStoreKind::InsecureKey);
        assert_eq!(key.data_path, home.join("keys"));

        assert_eq!(store_refusal(home, &exit(1)), None);
    }

    #[test]
    fn printed_commands_quote_a_home_with_spaces() {
        let home = Path::new("/srv/my home");
        for (message, command) in [
            (
                no_service_manager_message(home),
                "gents runtime server --home '/srv/my home'",
            ),
            (
                ignored_client_setting_notice(&home.join("client.json")),
                "'/srv/my home/client.json'",
            ),
        ] {
            assert!(message.contains(command), "{message}");
        }
    }

    #[test]
    fn the_wait_ends_once_the_agent_a_tab_opens_on_is_ready() {
        for agent in [None, Some("a")] {
            assert!(!can_run(None, agent));
            assert!(!can_run(Some(&readiness("a", &[])), agent));
            assert!(!can_run(Some(&readiness("a", &["other"])), agent));
            assert!(can_run(Some(&readiness("a", &["a"])), agent));
        }
        assert!(can_run(Some(&readiness("a", &["other"])), Some("other")));
    }

    #[test]
    fn the_reasons_a_model_clears_are_the_inference_ones() {
        use AgentReadinessUnavailableReason as Reason;
        let wanted: Vec<_> = Reason::ALL
            .into_iter()
            .filter(|r| wants_a_model(*r))
            .collect();
        assert_eq!(
            wanted,
            [
                Reason::BackendNotConfigured,
                Reason::BackendDisabled,
                Reason::BackendTemporarilyUnavailable,
                Reason::CredentialsRequired,
                Reason::SubscriptionNotEntitled,
                Reason::InferenceProfileInvalid,
            ]
        );
    }

    #[test]
    fn a_left_client_setting_is_named_and_says_how_to_stop_the_notice() {
        let notice = ignored_client_setting_notice(Path::new("/srv/my home/client.json"));
        assert!(notice.contains("'/srv/my home/client.json'"), "{notice}");
        assert!(notice.contains("built-in shell"), "{notice}");
        assert!(notice.contains("Delete the file"), "{notice}");
    }

    #[test]
    fn no_service_manager_says_run_gents_server_then_gents() {
        assert_eq!(
            no_service_manager_message(Path::new("/srv/home")),
            "run `gents runtime server --home /srv/home` in another terminal, then `gents`"
        );
    }

    #[test]
    fn the_continue_hint_names_the_session_the_shell_showed() {
        assert_eq!(
            continue_hint(Some("s1"), None).as_deref(),
            Some("Pick this up again with: gents --continue s1")
        );
        assert_eq!(
            continue_hint(Some("s1"), Some(Path::new("/srv/my home"))).as_deref(),
            Some("Pick this up again with: gents --continue s1 --home '/srv/my home'")
        );
        assert_eq!(continue_hint(None, None), None);
    }

    #[test]
    fn continue_with_a_prompt_is_refused() {
        let args = |continuing: bool, prompt: bool| StartArgs {
            home: None,
            store_key_custody: None,
            agent: None,
            print: None,
            prompt: prompt.then(|| "hello".to_string()),
            continue_session: continuing.then(|| "s1".to_string()),
            yolo: false,
        };
        assert!(refuses_continue_with_a_prompt(&args(true, true)));
        assert!(!refuses_continue_with_a_prompt(&args(true, false)));
        assert!(!refuses_continue_with_a_prompt(&args(false, true)));
        let message = shell_cannot_continue_with_a_prompt();
        assert!(
            message.contains("gents --continue SESSION_ID -p MESSAGE"),
            "{message}"
        );
    }
}
