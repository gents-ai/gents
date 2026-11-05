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
fn pairing_reader_commands_require_a_physical_document_and_reason() {
    let base = [
        "gents",
        "p2p",
        "pairings",
        "grant-reader",
        "--observation",
        "applied",
        "--doc-id",
        "physical-document",
        "--reason",
        "tool-view",
        "--agent-did",
        "did:key:agent",
    ];
    let cli = Cli::try_parse_from(base).expect("scoped reader command should parse");
    let Command::P2p {
        command:
            P2pCommand::Pairings {
                command: P2pPairingsCommand::GrantReader(args),
            },
    } = cli.command
    else {
        panic!("expected a pairing reader grant")
    };
    assert_eq!(args.doc_id, "physical-document");
    assert!(matches!(args.observation, PairingObservationArg::Applied));
    assert!(matches!(args.reason, PairingReadReasonArg::ToolView));
    assert!(Cli::try_parse_from([
        "gents",
        "p2p",
        "pairings",
        "revoke-reader",
        "--observation",
        "desired",
        "--agent-did",
        "did:key:agent",
    ])
    .is_err());
}
