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
        &["auth", "login", "codex"],
        &["auth", "login", "grok"],
        &["auth", "login", "claude"],
        &["auth", "login", "claude-gateway"],
        &["auth", "probe", "codex"],
        &["auth", "probe", "grok"],
        &["auth", "accounts"],
        &["data", "query"],
        &["data", "write"],
        &["data", "export"],
        &["data", "document"],
        &["data", "schema"],
        &["debug", "request"],
        &["debug", "response"],
        &["debug", "trace"],
        &["debug", "background"],
        &["debug", "fleet"],
        &["pack", "plugin"],
        &["pack", "ux"],
        &["init"],
        &["prepare-runtime"],
        &["chat"],
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
fn old_names_stop_working() {
    const OLD_NAMES: &[&str] = &[
        "codex-login",
        "grok-login",
        "claude-login",
        "claude-gateway-login",
        "codex-auth-probe",
        "grok-auth-probe",
        "accounts",
        "query",
        "write",
        "export",
        "document",
        "schema",
        "request",
        "response",
        "trace",
        "background",
        "fleet",
        "plugin",
        "ux",
    ];
    for name in OLD_NAMES {
        let Err(error) = Cli::try_parse_from(["gents", name]) else {
            panic!("`gents {name}` should no longer parse");
        };
        assert_eq!(error.kind(), ErrorKind::InvalidSubcommand, "{name}");
    }
}

#[test]
fn root_help_lists_every_visible_command_exactly_once() {
    let template = crate::ROOT_HELP_TEMPLATE;
    for section in [
        "Start",
        "Work",
        "Accounts",
        "Configure",
        "Runtime",
        "Data and debugging",
        "Measure",
    ] {
        assert!(
            template.contains(&format!("\x1b[1;4m{section}\x1b[0m\n  ")),
            "{section} is a styled heading above its commands"
        );
    }
    let cli = Cli::command();
    let rows: Vec<(&str, &str)> = template
        .lines()
        .filter_map(|line| line.strip_prefix("  "))
        .map(|row| {
            let (name, about) = row.split_once(' ').expect("a row names and describes");
            (name, about.trim_start())
        })
        .collect();
    assert_eq!(rows.len(), 26, "{rows:?}");
    for hidden in ["init", "prepare-runtime", "__native-fs-runner"] {
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
            "  ".len() + 14 + about.chars().count() <= 80,
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
