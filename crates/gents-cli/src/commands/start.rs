use std::future::Future;
use std::io::IsTerminal as _;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::time::{Duration, Instant};

use anyhow::{bail, ensure, Context, Result};
use clap::CommandFactory as _;
use gents_store::config_client::GraphqlEndpoint;
use tokio::runtime::Handle;
use gents_host::args::StoreKeyCustodyArg;

use crate::cli::args::ShellArgs;
use crate::cli::Cli;
use crate::config_writes::ConfigAccess;
use crate::home_state::{default_home_ignoring_env, read_finished_init_config, resolve_home_dir};
use crate::repair::home::{classify_home, BrokenHome, HomeState};
use crate::repair::{Blocker, HomeChoice, HomeDecision, Prepared, Report, Started};
use crate::shared::StoredInitConfig;
use gents_protocol::node_readiness::{
    AgentReadinessState, AgentReadinessUnavailableReason, NodeReadinessSnapshot,
};
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
    let (home, config, graphql, home_flag) =
        ready_node(args.home.take(), args.store_key_custody, terminal).await?;
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
    let (home, config, graphql, _) = ready_node(args.home, args.store_key_custody, true).await?;
    wait_for_its_agent(&graphql, &config.node_did, None, PUBLISH_WAIT).await;
    let shown = crate::commands::shell::open(&home, None, None, None, runtime).await?;
    if let Some(hint) = continue_hint(shown.as_deref(), None) {
        eprintln!("{hint}");
    }
    Ok(())
}

/// Repairs what stands between this home and a running node, sets the home up
/// when it needs it, and starts its node; also returns the `--home` that
/// reopens that node. Setup asks its questions on the terminal, so a home that
/// needs it and has none is refused.
async fn ready_node(
    home: Option<PathBuf>,
    custody: Option<StoreKeyCustodyArg>,
    terminal: bool,
) -> Result<(PathBuf, StoredInitConfig, GraphqlEndpoint, Option<PathBuf>)> {
    let mut progress = TerminalProgress::new();
    let ready = ready_node_with(home, custody, terminal, &mut progress).await;
    if ready.is_err() {
        progress.finish();
    }
    ready
}

async fn ready_node_with(
    home: Option<PathBuf>,
    custody: Option<StoreKeyCustodyArg>,
    terminal: bool,
    progress: &mut TerminalProgress,
) -> Result<(PathBuf, StoredInitConfig, GraphqlEndpoint, Option<PathBuf>)> {
    let mut home_flag = home.map(crate::absolutize).transpose()?;
    let flag = home_flag.clone();
    let mut choice = tokio::task::spawn_blocking(move || home_choice(flag.as_deref())).await??;
    // A broken folder that was named on purpose is not set aside where no one
    // can be asked first, since setup could not run after it.
    if !terminal && choice.explicit {
        let home = choice.home.clone();
        if let Ok(HomeState::Broken(why)) =
            tokio::task::spawn_blocking(move || classify_home(&home, None)).await?
        {
            bail!("{}", cannot_load_without_terminal(&choice.home, why));
        }
    }
    let mut setup_home = home_flag.clone();
    loop {
        let mut set_up = false;
        let (moved_from, older_data) = loop {
            let report = crate::repair::prepare(
                &choice,
                &(std::sync::Arc::new(crate::repair::current_program)
                    as crate::repair::ServiceProgram),
                crate::repair::ServiceMove::AfterSetup,
                progress,
            )
            .await?;
            match report.outcome {
                Prepared::Ready => break (report.serving_home, report.older_data),
                Prepared::NeedsSetup => {
                    ensure!(!set_up, "setup left this home uninitialized");
                    ensure!(terminal, "{}", not_set_up(&choice.home));
                    progress.finish();
                    choice = HomeChoice {
                        home: setup::setup(setup_home.take(), custody).await?,
                        explicit: false,
                    };
                    set_up = true;
                }
                Prepared::Decide(decision) => {
                    (choice, home_flag) = ask(decision, terminal, progress).await?;
                    setup_home = Some(choice.home.clone());
                }
                Prepared::Blocked(blocker) => blocked(progress, &blocker),
            }
        };
        let serving_home = moved_from.clone().filter(|_| choice.explicit);
        let mut set_up = false;
        loop {
            match crate::repair::start(
                &choice.home,
                serving_home.as_deref(),
                older_data.as_ref(),
                progress,
            )
            .await?
            .outcome
            {
                Started::Answering(graphql) => {
                    progress.finish();
                    let config = read_finished_init_config(&choice.home)?
                        .context("setup left this home uninitialized")?;
                    return Ok((choice.home, config, graphql, home_flag));
                }
                Started::NeedsSetup => {
                    ensure!(!set_up, "setup left this home uninitialized");
                    ensure!(terminal, "{}", not_set_up(&choice.home));
                    progress.finish();
                    setup::setup(Some(choice.home.clone()), custody).await?;
                    set_up = true;
                }
                Started::Decide(decision) => {
                    (choice, home_flag) =
                        restore_then_ask(decision, terminal, progress, move_service_back).await?;
                    setup_home = Some(choice.home.clone());
                    break;
                }
                Started::Blocked(blocker) => {
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
) -> Pin<Box<dyn Future<Output = Result<Report<Prepared>>> + Send + 'a>>;

/// `prepare` already moved the node away from the folder it was serving when
/// `start` asks, so that folder gets its node back first: a question left
/// unanswered (Ctrl-C, no terminal) leaves it working.
async fn restore_then_ask(
    decision: HomeDecision,
    terminal: bool,
    progress: &mut TerminalProgress,
    move_back: MoveBack,
) -> Result<(HomeChoice, Option<PathBuf>)> {
    give_back(Some(&decision.serving_home), progress, move_back).await;
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
) -> Pin<Box<dyn Future<Output = Result<Report<Prepared>>> + Send + '_>> {
    Box::pin(async move { crate::repair::restore(&choice, progress).await })
}

fn not_restarted(serving: &Path, moved: &Result<Report<Prepared>>) -> Option<String> {
    let reason = match moved {
        Ok(report) => match &report.outcome {
            Prepared::Ready => return None,
            Prepared::Blocked(blocker) => blocker.to_string(),
            Prepared::NeedsSetup | Prepared::Decide(_) => "it needs setup".to_string(),
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

fn cannot_load_without_terminal(home: &Path, why: BrokenHome) -> String {
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
            crate::native_service::incompatible_store_exit_code(*kind)
        }
        _ => 1,
    }
}

/// A newer node is running, so it is named on its own.
fn blocked_line(blocker: &Blocker) -> String {
    match blocker {
        Blocker::NewerNode { .. } => blocker.to_string(),
        _ => format!("Your local node couldn't start: {blocker}"),
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
    use crate::repair::reason::no_service_manager_message;
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

    static RESTORED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    fn decision() -> HomeDecision {
        HomeDecision {
            home: PathBuf::from("/srv/work"),
            why: BrokenHome::StoreKeyMissing,
            default_home: PathBuf::from("/Users/me/.gents"),
            serving_home: PathBuf::from("/Users/me/other"),
        }
    }

    fn record_move_back(
        choice: HomeChoice,
        _progress: &mut TerminalProgress,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Report<Prepared>>> + Send + '_>>
    {
        assert_eq!(
            choice,
            HomeChoice {
                home: PathBuf::from("/Users/me/other"),
                explicit: false
            },
            "the folder that was serving, not the default"
        );
        RESTORED.store(true, std::sync::atomic::Ordering::SeqCst);
        Box::pin(async {
            Ok(Report {
                repaired: Vec::new(),
                outcome: Prepared::NeedsSetup,
                serving_home: None,
                older_data: None,
            })
        })
    }

    #[tokio::test]
    async fn the_serving_folder_comes_back_before_the_question() {
        let error = restore_then_ask(
            decision(),
            false,
            &mut TerminalProgress::default(),
            record_move_back,
        )
        .await
        .unwrap_err();
        assert!(RESTORED.load(std::sync::atomic::Ordering::SeqCst));
        assert!(
            error
                .to_string()
                .contains("can't be loaded: the key for its data is missing"),
            "{error}"
        );
    }

    static GIVEN_BACK: std::sync::Mutex<Option<HomeChoice>> = std::sync::Mutex::new(None);

    fn record_give_back(
        choice: HomeChoice,
        _progress: &mut TerminalProgress,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<Report<Prepared>>> + Send + '_>>
    {
        *GIVEN_BACK.lock().unwrap() = Some(choice);
        Box::pin(async {
            Ok(Report {
                repaired: Vec::new(),
                outcome: Prepared::Ready,
                serving_home: None,
                older_data: None,
            })
        })
    }

    #[tokio::test]
    async fn a_blocked_start_gives_the_serving_folder_its_node_back() {
        let mut progress = TerminalProgress::default();
        give_back(None, &mut progress, record_give_back).await;
        assert_eq!(*GIVEN_BACK.lock().unwrap(), None, "nothing was moved");
        give_back(
            Some(Path::new("/srv/serving")),
            &mut progress,
            record_give_back,
        )
        .await;
        assert_eq!(
            *GIVEN_BACK.lock().unwrap(),
            Some(HomeChoice {
                home: PathBuf::from("/srv/serving"),
                explicit: false
            })
        );
    }

    #[test]
    fn a_node_that_does_not_come_back_is_named_in_one_line() {
        let serving = Path::new("/Users/me/other");
        let report = |outcome| {
            Ok(Report {
                repaired: Vec::new(),
                outcome,
                serving_home: None,
                older_data: None,
            })
        };
        assert_eq!(not_restarted(serving, &report(Prepared::Ready)), None);
        assert_eq!(
            not_restarted(serving, &report(Prepared::NeedsSetup)).as_deref(),
            Some("Your node for /Users/me/other could not be restarted: it needs setup")
        );
        assert_eq!(
            not_restarted(
                serving,
                &report(Prepared::Blocked(Blocker::OldNodeStillRunning { pid: 7 }))
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
        use gents_store::local::storage_backend::IncompatibleStoreKind::*;
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
            why: BrokenHome::StoreKeyMissing,
            default_home: PathBuf::from("/Users/me/.gents"),
            serving_home: PathBuf::from("/Users/me/other"),
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
