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
        &["chat"],
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
fn installed_services_and_updaters_keep_their_root_names() {
    let cli = Cli::try_parse_from(["gents", "server", "--home", "/h"]).expect("server parses");
    assert!(matches!(cli.command, Some(Command::Server(_))));
    let cli = Cli::try_parse_from(["gents", "service", "update-check", "--home", "/h"])
        .expect("the update preflight parses");
    assert!(matches!(cli.command, Some(Command::Service { .. })));
}

#[test]
fn old_names_stop_working() {
    const OLD_NAMES: &[&[&str]] = &[
        &["codex-login"],
        &["claude-login"],
        &["query"],
        &["plugin"],
        &["auth"],
        &["cloud"],
        &["session"],
        &["goal"],
        &["task"],
        &["job"],
        &["mailbox"],
        &["tools"],
        &["mcp"],
        &["pack"],
        &["provision"],
        &["reset"],
        &["diagnose"],
        &["p2p"],
        &["desktop"],
        &["version"],
        &["data"],
        &["debug"],
        &["eval"],
        &["optimization"],
        &["accounts", "accounts"],
    ];
    for path in OLD_NAMES {
        let argv = [&["gents"], *path].concat();
        let Err(error) = Cli::try_parse_from(argv) else {
            panic!("`gents {path:?}` should no longer parse");
        };
        assert_eq!(error.kind(), ErrorKind::InvalidSubcommand, "{path:?}");
    }
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
    assert_eq!(rows.len(), 8, "{rows:?}");
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
