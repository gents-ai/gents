use std::net::IpAddr;
use std::path::PathBuf;

use clap::ValueEnum;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Serialize, Deserialize, ValueEnum, PartialEq, Eq)]
pub enum ToolCeilingArg {
    MetaOnly,
    Readonly,
    Readwrite,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, ValueEnum, PartialEq, Eq)]
pub enum ToolPackageArg {
    Minimal,
    Introspection,
    Readonly,
    Write,
    Yolo,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum P2pRelayModeArg {
    Default,
    Disabled,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum P2pDiscoveryArg {
    #[value(name = "n0")]
    N0,
    Disabled,
}

#[derive(clap::Args)]
pub struct ServiceUpdateArgs {
    #[arg(long)]
    pub home: Option<PathBuf>,
    #[arg(
        long,
        help = "Local verified Gents executable to stage; omit for configuration-only updates"
    )]
    pub executable: Option<PathBuf>,
    #[arg(
        long,
        requires = "executable",
        help = "Expected SHA-256 of --executable, obtained from a trusted source"
    )]
    pub sha256: Option<String>,
    #[arg(
        long,
        help = "Idempotency key; reuse the same UUID only for the same update"
    )]
    pub operation_id: Option<uuid::Uuid>,
    #[command(flatten)]
    pub network: ServiceNetworkArgs,
    #[arg(long)]
    pub environment_file: Option<PathBuf>,
    #[arg(long, default_value_t = 300, value_parser = clap::value_parser!(u64).range(1..=3600))]
    pub drain_timeout_secs: u64,
}

#[derive(clap::Args, Default)]
pub struct ServiceNetworkArgs {
    #[arg(
        long,
        help = "HTTP listen address: loopback or one interface, such as this host's tailnet address. Requests still need a DID-signed bearer the node's access control admits"
    )]
    pub http_addr: Option<IpAddr>,
    #[arg(long, help = "HTTP listen port")]
    pub http_port: Option<u16>,
    #[arg(long, help = "P2P listen address")]
    pub p2p_bind_addr: Option<IpAddr>,
    #[arg(long, help = "P2P listen port; 0 picks a random port at each start")]
    pub p2p_port: Option<u16>,
    #[arg(long, value_enum)]
    pub p2p_relay_mode: Option<P2pRelayModeArg>,
    #[arg(long, value_enum)]
    pub p2p_discovery: Option<P2pDiscoveryArg>,
    #[arg(
        long,
        help = "Clear the persisted network settings before applying the other network flags"
    )]
    pub reset_network: bool,
}

pub enum ServiceAction {
    Install {
        network: ServiceNetworkArgs,
        environment_file: Option<PathBuf>,
    },
    Start {
        enable: bool,
    },
    Stop {
        keep_enabled: bool,
    },
    Restart,
    Enable,
    Disable,
    Status,
    Uninstall,
}
