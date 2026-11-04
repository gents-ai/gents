use clap::error::ErrorKind;

use super::*;

fn parse_init(extra: &[&str]) -> InitArgs {
    let mut argv = vec!["gents", "init"];
    argv.extend_from_slice(extra);
    let cli = Cli::try_parse_from(argv).expect("init should parse");
    match cli.command {
        Command::Init(args) => args,
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
