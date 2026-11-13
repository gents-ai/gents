use gents_protocol::operations::install_repair::RepairInput;
use gents_protocol::operations::install_repair::{
    Blocker, HomeChoice, HomeDecision, HomeState, RepairOperation, RepairOutcome, RepairReceipt,
    ServiceMove,
};
use gents_service::repair::ServiceProgram;
use std::future::Future;
use std::io::IsTerminal as _;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::{Duration, Instant};

use anyhow::{ensure, Context, Result};
use clap::CommandFactory as _;
use gents_host::args::StoreKeyCustodyArg;
use gents_store::config_client::GraphqlEndpoint;
use tokio::runtime::Handle;

use crate::cli::args::ShellArgs;
use crate::cli::Cli;
use crate::config_writes::ConfigAccess;
use gents_service::home_state::{
    default_home_ignoring_env, read_finished_init_config, resolve_home_dir,
};

use gents_protocol::node_readiness::{
    AgentReadinessState, AgentReadinessUnavailableReason, NodeReadinessSnapshot,
};
use gents_service::state::StoredInitConfig;
use progress::TerminalProgress;

mod progress;
mod setup;
mod yolo;

pub(crate) use yolo::default_agent;
pub(crate) use yolo::node_readiness;

/// A runtime publishes the agents it can run a little after its GraphQL answers.
const PUBLISH_WAIT: Duration = Duration::from_secs(5);

pub(crate) struct StartArgs {
    pub(crate) home: Option<PathBuf>,
    pub(crate) store_key_custody: Option<StoreKeyCustodyArg>,
    pub(crate) agent: Option<String>,
    pub(crate) print: Option<String>,
    pub(crate) steer: bool,
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
    ensure!(
        args.print.is_some() || !refuses_continue_with_a_prompt(&args),
        "{}",
        shell_cannot_continue_with_a_prompt()
    );
    let (home, config, graphql, home_flag) = ready_node(
        args.home.take(),
        args.store_key_custody,
        terminal,
        RepairOperation::Run,
    )
    .await?;
    let Some(message) = args.print.take() else {
        report_ignored_client_setting(&home);
        let agent = chosen_agent(&home, &config, &graphql, &args).await?;
        wait_for_its_agent(&graphql, &config.node_did, agent.as_deref(), PUBLISH_WAIT).await;
        let shown =
            crate::commands::shell::open(&home, agent, args.prompt, args.continue_session, runtime)
                .await?;
        if let Some(hint) = continue_hint(shown.as_deref(), home_flag.as_deref()) {
            eprintln!("{hint}");
        }
        return Ok(());
    };
    let agent = chosen_agent(&home, &config, &graphql, &args).await?;
    crate::commands::chat::print_reply(
        &home,
        &graphql,
        &config.node_did,
        agent.as_deref(),
        args.continue_session.as_deref(),
        &message,
        args.steer,
    )
    .await
}

/// The agent a run talks to: a continued session's own agent, the yolo agent, or `--agent`,
/// else the node's default.
async fn chosen_agent(
    home: &Path,
    config: &gents_service::state::StoredInitConfig,
    graphql: &GraphqlEndpoint,
    args: &StartArgs,
) -> Result<Option<String>> {
    if args.continue_session.is_none() && !args.yolo {
        return Ok(args.agent.clone());
    }
    if let Some(session) = &args.continue_session {
        return Ok(Some(
            session_agent(graphql, &config.node_did, session).await?,
        ));
    }
    Ok(Some(yolo::ensure_agent(home, config, graphql).await?))
}

/// `gents config client set` once chose between the built-in shell and Codex in this file.
/// Codex is gone, so a file left behind is named and otherwise ignored.
fn report_ignored_client_setting(home: &Path) {
    let path = home.join(gents_store::local::home::CLIENT_CONFIG_FILE_NAME);
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
    let owner = gents_store::config_client::ConfigAccess::Graphql(graphql.clone())
        .transact_readonly("cli.start.session_agent", |txn| {
            Box::pin(async move {
                gents_store::config_client::agent_session::load_agent_session_owner_in_txn(
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
    let (home, config, graphql, _) = ready_node(
        args.home,
        args.store_key_custody,
        true,
        RepairOperation::Run,
    )
    .await?;
    wait_for_its_agent(&graphql, &config.node_did, None, PUBLISH_WAIT).await;
    let shown = crate::commands::shell::open(&home, None, None, None, runtime).await?;
    if let Some(hint) = continue_hint(shown.as_deref(), None) {
        eprintln!("{hint}");
    }
    Ok(())
}

/// `gents runtime service repoint`: moves the per-user service to `home` and
/// starts its node there; a start that does not finish moves it back.
pub(crate) async fn repoint(home: Option<PathBuf>) -> Result<()> {
    let terminal = std::io::stdin().is_terminal() && std::io::stderr().is_terminal();
    let (home, _, graphql, _) = ready_node(home, None, terminal, RepairOperation::Repoint).await?;
    eprintln!(
        "The background service now serves {} at {}",
        crate::shell_word(&home),
        graphql.url()
    );
    Ok(())
}

/// Repairs what stands between this home and a running node, sets the home up
/// when it needs it, and starts its node; also returns the `--home` that
/// reopens that node. Setup asks its questions on the terminal, so a home that
/// needs it and has none is refused. Only `RepairOperation::Repoint` moves a
/// service that serves another home that still exists.
async fn ready_node(
    home: Option<PathBuf>,
    custody: Option<StoreKeyCustodyArg>,
    terminal: bool,
    operation: RepairOperation,
) -> Result<(PathBuf, StoredInitConfig, GraphqlEndpoint, Option<PathBuf>)> {
    let mut progress = TerminalProgress::new();
    let ready = ready_node_with(home, custody, terminal, operation, &mut progress).await;
    if ready.is_err() {
        progress.finish();
    }
    ready
}

async fn ready_node_with(
    home: Option<PathBuf>,
    custody: Option<StoreKeyCustodyArg>,
    terminal: bool,
    operation: RepairOperation,
    progress: &mut TerminalProgress,
) -> Result<(PathBuf, StoredInitConfig, GraphqlEndpoint, Option<PathBuf>)> {
    let mut home_flag = home.map(crate::absolutize).transpose()?;
    let flag = home_flag.clone();
    let mut choice = tokio::task::spawn_blocking(move || home_choice(flag.as_deref())).await??;
    let mut setup_home = home_flag.clone();
    loop {
        let mut set_up = false;
        let (moved_from, older_data) = loop {
            let report = gents_service::repair::execute(
                RepairInput::Prepare {
                    choice: choice.clone(),
                    service_move: ServiceMove::AfterSetup,
                    operation,
                },
                &ServiceProgram::new(gents_service::repair::current_program),
                progress,
                None,
            )
            .await?;
            match report.outcome {
                RepairOutcome::Ready | RepairOutcome::Answering(_) => {
                    break (report.serving_home, report.older_data)
                }
                RepairOutcome::NeedsSetup => {
                    ensure!(!set_up, "setup left this home uninitialized");
                    ensure!(terminal, "{}", not_set_up(&choice.home));
                    progress.finish();
                    choice = HomeChoice {
                        home: setup::setup(setup_home.take(), custody).await?,
                        explicit: false,
                    };
                    set_up = true;
                }
                RepairOutcome::Decide(decision) => {
                    (choice, home_flag) = ask(decision, terminal, progress).await?;
                    setup_home = Some(choice.home.clone());
                }
                RepairOutcome::Blocked(blocker) => blocked(progress, &blocker),
            }
        };
        let serving_home = moved_from.clone().filter(|_| choice.explicit);
        let mut set_up = false;
        loop {
            match gents_service::repair::execute(
                RepairInput::Start {
                    home: choice.home.clone(),
                    serving_home: serving_home.clone(),
                    older_data: older_data.clone(),
                },
                &ServiceProgram::new(gents_service::repair::current_program),
                progress,
                None,
            )
            .await?
            .outcome
            {
                RepairOutcome::Ready => {
                    anyhow::bail!("repair start did not return a runtime endpoint")
                }
                RepairOutcome::Answering(graphql) => {
                    progress.finish();
                    let config = read_finished_init_config(&choice.home)?
                        .context("setup left this home uninitialized")?;
                    let graphql = gents_service::home_state::resolve_graphql_endpoint(
                        Some(&graphql),
                        Some(&choice.home),
                    )?;
                    return Ok((choice.home, config, graphql, home_flag));
                }
                RepairOutcome::NeedsSetup => {
                    ensure!(!set_up, "setup left this home uninitialized");
                    ensure!(terminal, "{}", not_set_up(&choice.home));
                    progress.finish();
                    setup::setup(Some(choice.home.clone()), custody).await?;
                    set_up = true;
                }
                RepairOutcome::Decide(decision) => {
                    (choice, home_flag) =
                        restore_then_ask(decision, terminal, progress, move_service_back).await?;
                    setup_home = Some(choice.home.clone());
                    break;
                }
                RepairOutcome::Blocked(blocker) => {
                    give_back(moved_from.as_deref(), progress, move_service_back).await;
                    blocked(progress, &blocker)
                }
            }
        }
    }
}

/// Asks which folder to open; with no terminal to ask in, nothing is touched.
async fn ask(
    decision: HomeDecision,
    terminal: bool,
    progress: &mut TerminalProgress,
) -> Result<(HomeChoice, Option<PathBuf>)> {
    ensure!(
        terminal,
        "{}",
        cannot_load_without_terminal(&decision.home, decision.why)
    );
    progress.finish();
    eprintln!("{}", home_question(&decision));
    Ok(answered(
        decision,
        crate::interactive_backend::ask_choice(2).await?,
    ))
}

type MoveBack = for<'a> fn(
    HomeChoice,
    &'a mut TerminalProgress,
) -> Pin<Box<dyn Future<Output = Result<RepairReceipt>> + Send + 'a>>;

/// `prepare` already moved the node away from the folder it was serving when
/// `start` asks, so that folder gets its node back first: a question left
/// unanswered (Ctrl-C, no terminal) leaves it working.
async fn restore_then_ask(
    decision: HomeDecision,
    terminal: bool,
    progress: &mut TerminalProgress,
    move_back: MoveBack,
) -> Result<(HomeChoice, Option<PathBuf>)> {
    give_back(decision.serving_home.as_deref(), progress, move_back).await;
    ask(decision, terminal, progress).await
}

/// Moves the node back to `serving`, the folder it answered for before
/// `prepare` moved it, so a start that does not finish leaves that node
/// working; one line names it when it does not come back.
async fn give_back(serving: Option<&Path>, progress: &mut TerminalProgress, move_back: MoveBack) {
    let Some(serving) = serving else {
        return;
    };
    let choice = HomeChoice {
        home: serving.to_path_buf(),
        explicit: false,
    };
    let moved = move_back(choice, progress).await;
    if let Some(line) = not_restarted(serving, &moved) {
        progress.finish();
        eprintln!("{line}");
    }
}

/// Points the service back at `choice`'s folder and starts its node again,
/// since it was answering there; nothing else is started, the answer decides that.
fn move_service_back(
    choice: HomeChoice,
    progress: &mut TerminalProgress,
) -> Pin<Box<dyn Future<Output = Result<RepairReceipt>> + Send + '_>> {
    Box::pin(async move {
        gents_service::repair::execute(
            RepairInput::Restore { choice },
            &ServiceProgram::new(gents_service::repair::current_program),
            progress,
            None,
        )
        .await
        .map_err(anyhow::Error::from)
    })
}

fn not_restarted(serving: &Path, moved: &Result<RepairReceipt>) -> Option<String> {
    let reason = match moved {
        Ok(report) => match &report.outcome {
            RepairOutcome::Ready | RepairOutcome::Answering(_) => return None,
            RepairOutcome::Blocked(blocker) => blocker.to_string(),
            RepairOutcome::NeedsSetup | RepairOutcome::Decide(_) => "it needs setup".to_string(),
        },
        Err(error) => format!("{error:#}"),
    };
    Some(format!(
        "Your node for {} could not be restarted: {reason}",
        serving.display()
    ))
}

/// The folder an answer opens, and the `--home` that reopens it: none for the
/// default folder. The repaired folder is no longer asked about.
fn answered(decision: HomeDecision, picked: usize) -> (HomeChoice, Option<PathBuf>) {
    let (home, flag) = match picked {
        0 => (decision.default_home, None),
        _ => (decision.home.clone(), Some(decision.home)),
    };
    (
        HomeChoice {
            home,
            explicit: false,
        },
        flag,
    )
}

/// `--home` or `GENTS_HOME` naming the folder `gents` opens without them is
/// not a choice of another folder.
fn home_choice(home_given: Option<&Path>) -> Result<HomeChoice> {
    let home = crate::absolutize(resolve_home_dir(home_given))?;
    let given = home_given.is_some()
        || std::env::var_os(gents_store::local::home::GENTS_HOME_ENV)
            .is_some_and(|value| !value.is_empty());
    let canonical =
        |path: &Path| std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let explicit = given
        && !default_home_ignoring_env()
            .is_ok_and(|default| canonical(&default) == canonical(&home));
    Ok(HomeChoice { home, explicit })
}

fn not_set_up(home: &Path) -> String {
    format!(
        "{} is not set up yet; run `gents` in a terminal to set it up, then run this again",
        crate::shell_word(home)
    )
}

fn cannot_load_without_terminal(home: &Path, why: HomeState) -> String {
    format!(
        "{} can't be loaded: {why}. Run gents --home {} in a terminal to repair it.",
        home.display(),
        crate::shell_word(home)
    )
}

fn home_question(decision: &HomeDecision) -> String {
    format!(
        "{home} can't be loaded: {why}.\n  1) Use your default data folder ({default})\n  2) Repair {home} (what's there is set aside, then setup runs)",
        home = decision.home.display(),
        why = decision.why,
        default = decision.default_home.display()
    )
}

fn blocked(progress: &mut TerminalProgress, blocker: &Blocker) -> ! {
    progress.finish();
    eprintln!("{}", blocked_line(blocker));
    std::process::exit(exit_code(blocker));
}

/// A refused store exits with its kind's status, as `main` does.
fn exit_code(blocker: &Blocker) -> i32 {
    match blocker {
        Blocker::StoreRefused { kind, .. } => {
            gents_service::native_service::incompatible_store_exit_code(*kind)
        }
        _ => 1,
    }
}

/// A newer node is running, so it is named on its own.
fn blocked_line(blocker: &Blocker) -> String {
    match blocker {
        Blocker::NewerNode { .. } | Blocker::ServiceServesAnotherHome { .. } => blocker.to_string(),
        _ => format!(
            "Your local node couldn't start: {blocker}. Next: {}",
            crate::next_call::command(blocker.next_call(), None)
        ),
    }
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
                    effective_inference: None,
                })
                .collect(),
        }
    }

    #[test]
    fn printed_commands_quote_a_home_with_spaces() {
        let home = Path::new("/srv/my home");
        let message = ignored_client_setting_notice(&home.join("client.json"));
        assert!(message.contains("'/srv/my home/client.json'"), "{message}");
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
    fn no_service_manager_names_the_server_help() {
        let message = blocked_line(&Blocker::NoServiceManager {
            home: PathBuf::from("/srv/home"),
        });
        assert!(message.contains("gents runtime server --help"), "{message}");
    }

    #[test]
    fn a_node_that_does_not_come_back_is_named_in_one_line() {
        let serving = Path::new("/Users/me/other");
        let report = |outcome| {
            Ok(RepairReceipt {
                client: None,
                repaired: Vec::new(),
                outcome,
                serving_home: None,
                older_data: None,
            })
        };
        assert_eq!(not_restarted(serving, &report(RepairOutcome::Ready)), None);
        assert_eq!(
            not_restarted(serving, &report(RepairOutcome::NeedsSetup)).as_deref(),
            Some("Your node for /Users/me/other could not be restarted: it needs setup")
        );
        assert_eq!(
            not_restarted(
                serving,
                &report(RepairOutcome::Blocked(Blocker::OldNodeStillRunning { pid: 7 }))
            )
            .as_deref(),
            Some("Your node for /Users/me/other could not be restarted: the old background node (pid 7) did not stop. Quit it, then try again.")
        );
        assert_eq!(
            not_restarted(serving, &Err(anyhow::anyhow!("launchctl failed"))).as_deref(),
            Some("Your node for /Users/me/other could not be restarted: launchctl failed")
        );
    }

    #[test]
    fn a_refused_store_exits_with_its_kinds_status() {
        use gents_protocol::operations::install_repair::IncompatibleStoreKind::*;
        let refused = |kind| Blocker::StoreRefused {
            kind,
            message: String::new(),
        };
        assert_eq!(exit_code(&refused(UnknownLineage)), 65);
        assert_eq!(exit_code(&refused(InsecureKey)), 66);
        assert_eq!(exit_code(&refused(ForeignVersion)), 67);
        assert_eq!(
            exit_code(&Blocker::KeepsExiting {
                code: Some(65),
                last_error: None
            }),
            1
        );
    }

    #[test]
    fn the_default_folder_answer_reopens_without_a_home_flag() {
        let decision = HomeDecision {
            home: PathBuf::from("/srv/work"),
            why: HomeState::MissingStoreKey,
            default_home: PathBuf::from("/Users/me/.gents"),
            serving_home: Some(PathBuf::from("/Users/me/other")),
        };
        assert_eq!(
            answered(decision.clone(), 0),
            (
                HomeChoice {
                    home: PathBuf::from("/Users/me/.gents"),
                    explicit: false
                },
                None
            )
        );
        assert_eq!(
            answered(decision, 1),
            (
                HomeChoice {
                    home: PathBuf::from("/srv/work"),
                    explicit: false
                },
                Some(PathBuf::from("/srv/work"))
            )
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
    fn a_newer_running_node_is_not_a_node_that_could_not_start() {
        let newer = Blocker::NewerNode {
            running: "1.1.0".into(),
            ours: "1.0.0-rc1".into(),
        };
        assert_eq!(
            blocked_line(&newer),
            "Your local node runs Gents 1.1.0, newer than this Gents (1.0.0-rc1). Open Gents 1.1.0 or newer, then try again."
        );
        assert_eq!(
            blocked_line(&Blocker::OldNodeStillRunning { pid: 7 }),
            "Your local node couldn't start: the old background node (pid 7) did not stop. Quit it, then try again."
        );
    }

    #[test]
    fn continue_with_a_prompt_is_refused() {
        let args = |continuing: bool, prompt: bool| StartArgs {
            home: None,
            store_key_custody: None,
            agent: None,
            print: None,
            steer: false,
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
