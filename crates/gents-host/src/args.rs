use std::net::{IpAddr, SocketAddr};
use std::path::PathBuf;

use clap::ValueEnum;
use gents_service::args::{P2pDiscoveryArg, P2pRelayModeArg, ToolCeilingArg};

pub fn parse_env_var_name(value: &str) -> Result<String, String> {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return Err("environment variable name cannot be empty".to_string());
    };
    if !(first == '_' || first.is_ascii_alphabetic())
        || !chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
    {
        return Err(format!("invalid environment variable name {value:?}"));
    }
    Ok(value.to_string())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum P2pTransportArg {
    None,
    Iroh,
}

impl P2pTransportArg {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Iroh => "iroh",
        }
    }
}

/// Custody of a new home's store encryption key (`gents::store::local::store_key`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum StoreKeyCustodyArg {
    File,
    Keychain,
}

pub fn store_key_custody(
    arg: Option<StoreKeyCustodyArg>,
) -> gents_store::local::store_key::StoreKeyCustodyChoice {
    match arg {
        Some(StoreKeyCustodyArg::File) => {
            gents_store::local::store_key::StoreKeyCustodyChoice::File
        }
        Some(StoreKeyCustodyArg::Keychain) => {
            gents_store::local::store_key::StoreKeyCustodyChoice::Keychain
        }
        None => gents_store::local::store_key::StoreKeyCustodyChoice::default(),
    }
}

#[derive(clap::Args)]
pub struct PrepareRuntimeArgs {
    #[command(flatten)]
    pub serve: ServeArgs,
    #[arg(
        long,
        value_name = "DIRECTORY",
        conflicts_with_all = ["tool_root", "tool_ceiling"],
        help = "Explicitly change the managed runtime's tool root after safely retiring idle work"
    )]
    pub change_tool_root: Option<PathBuf>,
}

#[derive(clap::Args)]
pub struct ServeArgs {
    #[arg(long, help = "Node home directory. Defaults to ~/.gents")]
    pub home: Option<PathBuf>,
    #[arg(
        long,
        help = "Fail instead of displaying macOS Keychain access dialogs"
    )]
    pub no_keychain_prompts: bool,
    #[arg(skip)]
    pub keychain_startup_approval: bool,
    #[arg(long, hide = true)]
    pub data_dir: Option<PathBuf>,
    #[arg(
        long,
        help = "HTTP listen address. Defaults to the home's network.json (`gents runtime service install --http-addr`), else 127.0.0.1"
    )]
    pub http_addr: Option<IpAddr>,
    #[arg(
        long,
        help = "HTTP port. Defaults to the home's network.json, else 9191, where the desktop and CLI expect the default home's runtime"
    )]
    pub http_port: Option<u16>,
    #[arg(
        long,
        value_name = "URL",
        help = "Explicit routable GraphQL URL to publish in the signed peer endpoint"
    )]
    pub advertise_graphql_url: Option<String>,
    #[arg(long)]
    pub node_name: Option<String>,
    #[arg(long)]
    pub key_path: Option<PathBuf>,
    #[arg(
        long,
        value_enum,
        help = "Operator safety cap that clamps the selected Tools document at runtime"
    )]
    pub tool_ceiling: Option<ToolCeilingArg>,
    #[arg(
        long,
        default_value_t = 120,
        help = "Foreground Bash command timeout in seconds: applied when a call omits timeout_secs, and the foreground cap unless --command-timeout-max-secs raises it (backgrounded runs use the built-in background lifetime budget)"
    )]
    pub command_timeout_secs: u64,
    #[arg(
        long,
        help = "Foreground cap in seconds for explicit Bash timeout_secs requests. Defaults to --command-timeout-secs; values below it are raised to it (#1018)"
    )]
    pub command_timeout_max_secs: Option<u64>,
    #[arg(long = "cli-tool")]
    pub cli_tools: Vec<String>,
    #[arg(
        long,
        default_value_t = false,
        help = "Expose the read-only defra_query MCP tool at /mcp. Off by default: this is an unauthenticated read surface (same listener exposure as the GraphQL endpoint)"
    )]
    pub enable_mcp: bool,
    #[arg(
        long = "mcp-query-collection",
        help = "When --enable-mcp is set, restrict the /mcp defra_query tool to these collections (repeatable); omit for all"
    )]
    pub mcp_query_collections: Vec<String>,
    #[arg(
        long = "mcp-write-collection",
        requires = "enable_mcp",
        help = "Expose write for exactly these application collections; each call must forward a caller-signed DefraDB bearer (repeatable)"
    )]
    pub mcp_write_collections: Vec<String>,
    #[arg(
        long,
        value_name = "ADDR:PORT",
        env = "GENTS_METRICS_ADDR",
        help = "Also serve Prometheus /metrics on this dedicated listener, without runtime operator authorization. Off by default. Loopback addresses serve unauthenticated; a specific non-loopback interface requires --metrics-auth-token-env; wildcard (unspecified) addresses are refused. The runtime API's /metrics still requires the operator"
    )]
    pub metrics_addr: Option<SocketAddr>,
    #[arg(
        long,
        value_name = "ENV_VAR",
        value_parser = parse_env_var_name,
        requires = "metrics_addr",
        help = "Environment variable containing the bearer token scrapers must send to the --metrics-addr listener"
    )]
    pub metrics_auth_token_env: Option<String>,
    #[arg(
        long,
        help = "Root directory for readonly/readwrite tool ceilings. Readonly defaults to the current working directory when unset"
    )]
    pub tool_root: Option<PathBuf>,
    #[arg(
        long,
        value_enum,
        default_value_t = P2pTransportArg::Iroh,
        help = "P2P transport for this server. Use `none` for local-only demos that only need GraphQL"
    )]
    pub p2p_transport: P2pTransportArg,
    #[arg(
        long,
        help = "P2P listen address. Defaults to the home's network.json, else 127.0.0.1"
    )]
    pub p2p_bind_addr: Option<IpAddr>,
    #[arg(
        long,
        help = "P2P listen port. Defaults to the home's network.json, else a random port"
    )]
    pub p2p_port: Option<u16>,
    #[arg(long)]
    pub p2p_secret_key_path: Option<PathBuf>,
    #[arg(
        long,
        value_enum,
        help = "P2P relay mode. Defaults to the home's network.json, else disabled"
    )]
    pub p2p_relay_mode: Option<P2pRelayModeArg>,
    #[arg(
        long,
        value_enum,
        help = "P2P discovery. Defaults to the home's network.json, else disabled"
    )]
    pub p2p_discovery: Option<P2pDiscoveryArg>,
    #[arg(
        long,
        env = "DEFRA_P2P_MAX_PENDING_DAGS",
        help = "Maximum pending DAG registrations retained while Bitswap resolves missing links (must be > 0)"
    )]
    pub p2p_max_pending_dags: Option<usize>,
    #[arg(
        long,
        env = "DEFRA_P2P_MAX_CONCURRENT_PUSH_TASKS",
        help = "Maximum concurrent outbound PushLog worker slots (must be > 0). This is the hub fan-out semaphore that must free on timeout"
    )]
    pub p2p_max_concurrent_push_tasks: Option<usize>,
    #[arg(
        long,
        env = "DEFRA_P2P_MAX_CONCURRENT_DAG_FETCHES",
        help = "Maximum concurrent Bitswap DAG fetches while resolving missing links (must be > 0)"
    )]
    pub p2p_max_concurrent_dag_fetches: Option<usize>,
    #[arg(
        long,
        env = "DEFRA_P2P_RATE_LIMIT_BURST",
        help = "Per-peer P2P rate-limit burst capacity (must be > 0)"
    )]
    pub p2p_rate_limit_burst: Option<u32>,
    #[arg(
        long,
        env = "DEFRA_P2P_RATE_LIMIT_RATE",
        help = "Per-peer P2P rate-limit refill rate in tokens per second (must be finite and > 0)"
    )]
    pub p2p_rate_limit_rate: Option<f64>,
    #[arg(
        long = "apply-root",
        value_name = "ROOT",
        help = "After the server is ready, run config apply on this pack root (schemas/ then desired-state) against the in-process node. Rebinds pack placeholder DIDs to the home node (same as config apply --bind-node-did home --force-rebind-concrete-did)"
    )]
    pub apply_root: Option<PathBuf>,
    #[arg(
        long = "apply-inference-slot",
        value_name = "SLOT=PROFILE",
        requires = "apply_root"
    )]
    pub apply_inference_slots: Vec<String>,
    #[arg(
        long = "apply-prune",
        default_value_t = false,
        requires = "apply_root",
        help = "With --apply-root, prune live-only config docs absent from the pack (same as config apply --prune)"
    )]
    pub apply_prune: bool,
}

impl ServeArgs {
    /// Explicit `gents runtime server` flags win over persisted settings.
    pub fn fill_unset_network(
        &mut self,
        settings: &gents_service::network_settings::NetworkSettings,
    ) {
        self.http_addr = self.http_addr.or(settings.http_addr);
        self.http_port = self.http_port.or(settings.http_port);
        self.p2p_bind_addr = self.p2p_bind_addr.or(settings.p2p_bind_addr);
        self.p2p_port = self.p2p_port.or(settings.p2p_port);
        self.p2p_relay_mode = self.p2p_relay_mode.or(settings.p2p_relay_mode);
        self.p2p_discovery = self.p2p_discovery.or(settings.p2p_discovery);
    }
}
