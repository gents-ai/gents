use std::path::Path;

use clap::Parser as _;
use gents_service::args::{P2pDiscoveryArg, P2pRelayModeArg, ServiceAction, ServiceNetworkArgs};
use gents_service::network_settings::{NetworkSettings, DEFAULT_HTTP_ADDR};

use crate::cli::{Cli, Command, RuntimeCommand, ServeArgs};

fn install_network(argv: &[&str]) -> ServiceNetworkArgs {
    let mut full = vec!["gents", "runtime", "service", "install"];
    full.extend_from_slice(argv);
    match Cli::try_parse_from(full)
        .expect("service install parses")
        .command
    {
        Some(Command::Runtime {
            command: RuntimeCommand::Service { command },
        }) => match command.into_parts().2 {
            ServiceAction::Install { network, .. } => network,
            _ => panic!("expected install"),
        },
        _ => panic!("expected service"),
    }
}

fn serve_args(argv: &[&str]) -> ServeArgs {
    let mut full = vec!["gents", "server"];
    full.extend_from_slice(argv);
    match Cli::try_parse_from(full).expect("server parses").command {
        Some(Command::Server(args)) => args,
        _ => panic!("expected server"),
    }
}

const TAILNET: &[&str] = &[
    "--http-addr",
    "100.64.0.7",
    "--http-port",
    "9393",
    "--p2p-bind-addr",
    "0.0.0.0",
    "--p2p-port",
    "4455",
    "--p2p-relay-mode",
    "default",
    "--p2p-discovery",
    "n0",
];

fn tailnet_settings() -> NetworkSettings {
    NetworkSettings {
        http_addr: Some("100.64.0.7".parse().unwrap()),
        http_port: Some(9393),
        p2p_bind_addr: Some("0.0.0.0".parse().unwrap()),
        p2p_port: Some(4455),
        p2p_relay_mode: Some(P2pRelayModeArg::Default),
        p2p_discovery: Some(P2pDiscoveryArg::N0),
    }
}

#[test]
fn service_install_parses_every_network_flag() {
    let network = install_network(TAILNET);
    assert_eq!(
        NetworkSettings::default().apply(&network),
        tailnet_settings()
    );
    assert!(!network.reset_network);
    assert!(install_network(&["--reset-network"]).reset_network);
}

#[test]
fn server_network_flags_default_to_unset_so_persisted_settings_can_fill_them() {
    let args = serve_args(&[]);
    assert_eq!(args.http_addr, None);
    assert_eq!(args.http_port, None);
    assert_eq!(args.p2p_bind_addr, None);
    assert_eq!(args.p2p_port, None);
    assert_eq!(args.p2p_relay_mode, None);
    assert_eq!(args.p2p_discovery, None);
}

#[test]
fn explicit_server_flags_win_over_persisted_settings() {
    let mut args = serve_args(&["--http-addr", "127.0.0.1", "--p2p-port", "5000"]);
    args.fill_unset_network(&tailnet_settings());
    assert_eq!(args.http_addr, Some(DEFAULT_HTTP_ADDR));
    assert_eq!(args.http_port, Some(9393));
    assert_eq!(args.p2p_bind_addr, Some("0.0.0.0".parse().unwrap()));
    assert_eq!(args.p2p_port, Some(5000));
    assert_eq!(args.p2p_relay_mode, Some(P2pRelayModeArg::Default));
    assert_eq!(args.p2p_discovery, Some(P2pDiscoveryArg::N0));
}

#[test]
fn install_accepts_an_explicit_environment_file() {
    let cli = Cli::try_parse_from([
        "gents",
        "runtime",
        "service",
        "install",
        "--environment-file",
        "operator.json",
    ])
    .unwrap();
    let Some(Command::Runtime {
        command: RuntimeCommand::Service { command },
    }) = cli.command
    else {
        panic!("service command");
    };
    let ServiceAction::Install {
        environment_file, ..
    } = command.into_parts().2
    else {
        panic!("service install");
    };
    assert_eq!(
        environment_file.as_deref(),
        Some(Path::new("operator.json"))
    );
}

#[test]
fn server_keychain_dialog_prevention_is_opt_in() {
    for (arguments, expected) in [
        (vec!["gents", "server"], false),
        (vec!["gents", "server", "--no-keychain-prompts"], true),
    ] {
        let cli = Cli::try_parse_from(arguments).unwrap();
        let Some(Command::Server(args)) = cli.command else {
            panic!("expected server command");
        };
        assert_eq!(args.no_keychain_prompts, expected);
        assert!(!args.keychain_startup_approval);
    }
    assert!(
        Cli::try_parse_from(["gents", "prepare-runtime", "--keychain-startup-approval"])
            .is_err()
    );
}
