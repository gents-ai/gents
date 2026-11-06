use clap::error::ErrorKind;
use clap::CommandFactory;

use super::*;

fn parse_init(extra: &[&str]) -> InitArgs {
    let mut argv = vec!["gents", "init"];
    argv.extend_from_slice(extra);
    let cli = Cli::try_parse_from(argv).expect("init should parse");
    match cli.command {
        Some(Command::Init(args)) => args,
        _ => panic!("expected `init`"),
    }
}

#[test]
fn init_uses_the_runtime_backend_defaults_and_preserves_overrides() {
    let defaults = parse_init(&[]);
    assert_eq!(defaults.max_concurrent, None);
    assert_eq!(
        defaults.max_queue_depth,
        gents::inference::backends::registry::DEFAULT_MAX_QUEUE_DEPTH
    );
    let explicit = parse_init(&["--max-concurrent", "7", "--max-queue-depth", "0"]);
    assert_eq!(explicit.max_concurrent, Some(7));
    assert_eq!(explicit.max_queue_depth, 0);
}

#[test]
fn grouped_commands_parse() {
    const PATHS: &[&[&str]] = &[
        &["status"],
        &["sessions", "list"],
        &["sessions", "show"],
        &["sessions", "fork"],
        &["automation", "goal"],
        &["automation", "task"],
        &["automation", "job"],
        &["automation", "mailbox"],
        &["accounts", "login", "codex"],
        &["accounts", "login", "grok"],
        &["accounts", "login", "claude"],
        &["accounts", "login", "claude-gateway"],
        &["accounts", "login", "cloud"],
        &["accounts", "probe", "codex"],
        &["accounts", "probe", "grok"],
        &["accounts", "list"],
        &["accounts", "label"],
        &["accounts", "disable"],
        &["accounts", "remove"],
        &["config", "agent"],
        &["config", "tools", "explain"],
        &["config", "tools", "set"],
        &["config", "mcp", "register"],
        &["config", "mcp", "probe"],
        &["config", "mcp", "list"],
        &["config", "pack", "plugin"],
        &["config", "pack", "ux"],
        &["runtime", "server"],
        &["runtime", "service"],
        &["runtime", "provision"],
        &["runtime", "reset"],
        &["runtime", "diagnose"],
        &["runtime", "p2p"],
        &["runtime", "desktop"],
        &["runtime", "version"],
        &["utils", "data", "query"],
        &["utils", "debug", "request"],
        &["utils", "eval"],
        &["utils", "optimization"],
        &["init"],
        &["prepare-runtime"],
        &["codex"],
    ];
    for path in PATHS {
        let argv = [&["gents"], *path, &["--help"]].concat();
        let Err(error) = Cli::try_parse_from(argv) else {
            panic!("{path:?} --help should stop at help");
        };
        assert_eq!(error.kind(), ErrorKind::DisplayHelp, "{path:?}");
    }
}

#[test]
fn print_sends_one_message_to_the_chosen_agent() {
    let cli = Cli::try_parse_from(["gents", "-p", "hi", "--agent", "a"]).expect("-p parses");
    assert_eq!(cli.print.as_deref(), Some("hi"));
    assert_eq!(cli.agent.as_deref(), Some("a"));
    assert!(cli.command.is_none());
    let cli = Cli::try_parse_from(["gents", "--yolo", "--print", "hi"]).expect("--yolo -p parses");
    assert!(cli.yolo);
    assert_eq!(cli.print.as_deref(), Some("hi"));
    let empty = Cli::try_parse_from(["gents", "-p", "  "])
        .err()
        .expect("an empty message");
    assert_eq!(empty.kind(), ErrorKind::ValueValidation);
    let conflict = Cli::try_parse_from(["gents", "--yolo", "--agent", "a"])
        .err()
        .expect("--yolo names its own agent");
    assert_eq!(conflict.kind(), ErrorKind::ArgumentConflict);
    assert!(Cli::try_parse_from(["gents", "-p", "hi", "status"]).is_err());
}

#[test]
fn a_bare_prompt_opens_the_interface_with_it_and_commands_still_win() {
    let cli =
        Cli::try_parse_from(["gents", "hello there", "--agent", "a"]).expect("a prompt parses");
    assert_eq!(cli.prompt.as_deref(), Some("hello there"));
    assert_eq!(cli.agent.as_deref(), Some("a"));
    assert!(cli.command.is_none());
    let cli = Cli::try_parse_from(["gents", "--yolo", "hi"]).expect("--yolo with a prompt parses");
    assert_eq!(cli.prompt.as_deref(), Some("hi"));
    let cli = Cli::try_parse_from(["gents", "status"]).expect("a command name stays a command");
    assert!(cli.prompt.is_none());
    assert!(matches!(cli.command, Some(Command::Status(_))));
    let conflict = Cli::try_parse_from(["gents", "hi", "-p", "there"])
        .err()
        .expect("a prompt opens an interface, -p does not");
    assert_eq!(conflict.kind(), ErrorKind::ArgumentConflict);
}

#[test]
fn continue_picks_up_a_session_with_its_own_agent() {
    let cli = Cli::try_parse_from(["gents", "--continue", "s1"]).expect("--continue parses");
    assert_eq!(cli.continue_session.as_deref(), Some("s1"));
    let cli = Cli::try_parse_from(["gents", "--continue", "s1", "-p", "next"])
        .expect("--continue with -p parses");
    assert_eq!(cli.print.as_deref(), Some("next"));
    let cli = Cli::try_parse_from(["gents", "--continue", "s1", "next step"])
        .expect("--continue with a prompt parses");
    assert_eq!(cli.prompt.as_deref(), Some("next step"));
    for other in [&["--agent", "a"][..], &["--yolo"][..]] {
        let argv = [&["gents", "--continue", "s1"][..], other].concat();
        let conflict = Cli::try_parse_from(argv)
            .err()
            .expect("the session names its own agent");
        assert_eq!(conflict.kind(), ErrorKind::ArgumentConflict, "{other:?}");
    }
}

#[test]
fn installed_services_and_updaters_keep_their_root_names() {
    let cli = Cli::try_parse_from(["gents", "server", "--home", "/h"]).expect("server parses");
    assert!(matches!(cli.command, Some(Command::Server(_))));
    let cli = Cli::try_parse_from(["gents", "service", "update-check", "--home", "/h"])
        .expect("the update preflight parses");
    assert!(matches!(cli.command, Some(Command::Service { .. })));
}

#[test]
fn old_names_stop_working() {
    const OLD_COMMANDS: &[&[&str]] = &[
        &["codex-login", "--manual"],
        &["claude-login", "--manual"],
        &["query", "find"],
        &["plugin", "list"],
        &["auth", "login", "claude"],
        &["cloud", "login"],
        &["session", "list"],
        &["goal", "show"],
        &["task", "list"],
        &["job", "list"],
        &["mailbox", "list"],
        &["tools", "explain"],
        &["mcp", "probe"],
        &["pack", "install"],
        &["provision", "--root", "r"],
        &["reset", "--home", "h"],
        &["diagnose", "--home", "h"],
        &["p2p", "status"],
        &["desktop", "reset"],
        &["version", "--json"],
        &["data", "query"],
        &["debug", "request"],
        &["eval", "run"],
        &["optimization", "run"],
        &["accounts", "accounts"],
        &["chat", "--session-id", "s"],
    ];
    for path in OLD_COMMANDS {
        let argv = [&["gents"], *path].concat();
        assert!(
            Cli::try_parse_from(argv).is_err(),
            "`gents {path:?}` should no longer parse"
        );
    }
    for (former, now) in [
        ("reset", "gents runtime reset"),
        ("auth", "gents accounts"),
        ("query", "gents utils data query"),
        ("chat", "gents -p MESSAGE"),
    ] {
        let refused = Cli::try_parse_from(["gents", former])
            .err()
            .expect("a former command's name is not sent as a prompt");
        assert_eq!(refused.kind(), ErrorKind::ValueValidation, "{former}");
        assert!(refused.to_string().contains(now), "{former}: {refused}");
    }
    let misplaced = Cli::try_parse_from(["gents", "--home", "h", "status"])
        .err()
        .expect("a command after a root option is not a prompt");
    assert!(
        misplaced.to_string().contains("gents status --home"),
        "{misplaced}"
    );
    let cli = Cli::try_parse_from(["gents", "reset the demo"]).expect("more words are a prompt");
    assert_eq!(cli.prompt.as_deref(), Some("reset the demo"));
}

#[test]
fn root_help_lists_every_visible_command_exactly_once() {
    let template = crate::ROOT_HELP_TEMPLATE;
    let cli = Cli::command();
    let rows: Vec<(&str, &str)> = template
        .lines()
        .filter_map(|line| line.strip_prefix("  "))
        .map(|row| {
            let (name, about) = row.split_once(' ').expect("a row names and describes");
            (name, about.trim_start())
        })
        .collect();
    assert_eq!(rows.len(), 7, "{rows:?}");
    assert!(
        rows.windows(2).all(|pair| pair[0].0 < pair[1].0),
        "root help lists commands alphabetically: {rows:?}"
    );
    for hidden in [
        "init",
        "prepare-runtime",
        "__native-fs-runner",
        "server",
        "service",
        "codex",
    ] {
        assert!(
            !rows.iter().any(|(name, _)| *name == hidden),
            "{hidden} is hidden: {rows:?}"
        );
    }
    for (name, about) in &rows {
        let command = cli
            .find_subcommand(name)
            .unwrap_or_else(|| panic!("{name} is a command"));
        assert_eq!(
            command.get_about().map(ToString::to_string).as_deref(),
            Some(*about),
            "{name}'s row says what its own help says"
        );
        assert!(
            "  ".len() + 12 + about.chars().count() <= 80,
            "{name}'s row fits an 80 column terminal"
        );
    }

    let mut listed: Vec<&str> = rows.iter().map(|(name, _)| *name).collect();
    let mut visible: Vec<&str> = cli
        .get_subcommands()
        .filter(|command| !command.is_hide_set())
        .map(|command| command.get_name())
        .collect();
    listed.sort_unstable();
    visible.sort_unstable();
    assert_eq!(listed, visible);
}
