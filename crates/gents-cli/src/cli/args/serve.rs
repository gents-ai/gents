use std::net::IpAddr;
use std::path::PathBuf;

use super::{
    parse_env_var_name, P2pDiscoveryArg, P2pRelayModeArg, P2pTransportArg, ToolCeilingArg,
};
use crate::cli::output_format::OutputFormat;

#[derive(clap::Args)]
pub(crate) struct ServeArgs {
    #[arg(long, help = "Node home directory. Defaults to ~/.gents")]
    pub(crate) home: Option<PathBuf>,
    #[arg(
        long,
        help = "Fail instead of displaying macOS Keychain access dialogs"
    )]
    pub(crate) no_keychain_prompts: bool,
    #[arg(skip)]
    pub(crate) keychain_startup_approval: bool,
    #[arg(long, hide = true)]
    pub(crate) data_dir: Option<PathBuf>,
    #[arg(
        long,
        help = "HTTP listen address. Defaults to the home's network.json (`gents service install --http-addr`), else 127.0.0.1"
    )]
    pub(crate) http_addr: Option<IpAddr>,
    #[arg(
        long,
        help = "HTTP port. Defaults to the home's network.json, else 9191, where the desktop and CLI expect the default home's runtime"
    )]
    pub(crate) http_port: Option<u16>,
    #[arg(long)]
    pub(crate) node_name: Option<String>,
    #[arg(long)]
    pub(crate) key_path: Option<PathBuf>,
    #[arg(
        long,
        value_enum,
        help = "Operator safety cap that clamps the selected Tools document at runtime"
    )]
    pub(crate) tool_ceiling: Option<ToolCeilingArg>,
    #[arg(
        long,
        default_value_t = 120,
        help = "Foreground Bash command timeout in seconds: applied when a call omits timeout_secs, and the foreground cap unless --command-timeout-max-secs raises it (backgrounded runs use the built-in background lifetime budget)"
    )]
    pub(crate) command_timeout_secs: u64,
    #[arg(
        long,
        help = "Foreground cap in seconds for explicit Bash timeout_secs requests. Defaults to --command-timeout-secs; values below it are raised to it (#1018)"
    )]
    pub(crate) command_timeout_max_secs: Option<u64>,
    #[arg(long = "cli-tool")]
    pub(crate) cli_tools: Vec<String>,
    #[arg(
        long,
        default_value_t = false,
        help = "Expose the read-only defra_query MCP tool at /mcp. Off by default: this is an unauthenticated read surface (same listener exposure as the GraphQL endpoint)"
    )]
    pub(crate) enable_mcp: bool,
    #[arg(
        long = "mcp-query-collection",
        help = "When --enable-mcp is set, restrict the /mcp defra_query tool to these collections (repeatable); omit for all"
    )]
    pub(crate) mcp_query_collections: Vec<String>,
    #[arg(
        long = "mcp-write-collection",
        requires = "enable_mcp",
        help = "Expose write for exactly these application collections; each call must forward a caller-signed DefraDB bearer (repeatable)"
    )]
    pub(crate) mcp_write_collections: Vec<String>,
    #[arg(
        long,
        help = "Root directory for readonly/readwrite tool ceilings. Readonly defaults to the current working directory when unset"
    )]
    pub(crate) tool_root: Option<PathBuf>,
    #[arg(
        long,
        default_value_t = false,
        help = "Disable the Codex TUI endpoint (`gents codex` needs it)"
    )]
    pub(crate) no_codex_shim: bool,
    #[arg(
        long,
        default_value = "127.0.0.1",
        help = "Address for the app-server shim to listen on; non-loopback addresses require --codex-shim-auth-token-env"
    )]
    pub(crate) codex_shim_bind_addr: IpAddr,
    #[arg(long, default_value_t = crate::DEFAULT_CODEX_SHIM_PORT)]
    pub(crate) codex_shim_port: u16,
    #[arg(
        long,
        value_name = "ENV_VAR",
        value_parser = parse_env_var_name,
        conflicts_with = "no_codex_shim",
        help = "Environment variable containing the bearer token required by the app-server WebSocket"
    )]
    pub(crate) codex_shim_auth_token_env: Option<String>,
    #[arg(
        long,
        value_name = "WSS_URL",
        requires = "codex_shim_auth_token_env",
        conflicts_with = "no_codex_shim",
        help = "Public wss:// app-server URL advertised when TLS terminates in a reverse proxy"
    )]
    pub(crate) codex_shim_public_url: Option<String>,
    #[arg(long, help = "Optional GENTS agent override for Codex turns")]
    pub(crate) codex_shim_agent_id: Option<String>,
    #[arg(long, default_value_t = crate::DEFAULT_CODEX_SHIM_TIMEOUT_SECS)]
    pub(crate) codex_shim_timeout_secs: u64,
    #[arg(long, default_value_t = 250)]
    pub(crate) codex_shim_poll_ms: u64,
    #[arg(
        long,
        default_value_t = false,
        help = "Expose the Grok TUI leader socket (Unix only) so stock Grok can attach as the pager client: start this server first, then run `grok --leader --leader-socket <path>`"
    )]
    pub(crate) grok_shim: bool,
    #[arg(
        long,
        requires = "grok_shim",
        help = "Unix socket path for the Grok TUI leader. Defaults to $XDG_RUNTIME_DIR/gents/grok.sock, falling back to /tmp/gents-grok.sock"
    )]
    pub(crate) grok_shim_socket_path: Option<PathBuf>,
    #[arg(
        long,
        requires = "grok_shim",
        help = "Optional GENTS agent override for Grok pager turns"
    )]
    pub(crate) grok_shim_agent_id: Option<String>,
    #[arg(
        long,
        value_enum,
        default_value_t = P2pTransportArg::Iroh,
        help = "P2P transport for this server. Use `none` for local-only demos that only need GraphQL/Codex shim"
    )]
    pub(crate) p2p_transport: P2pTransportArg,
    #[arg(
        long,
        help = "P2P listen address. Defaults to the home's network.json, else 127.0.0.1"
    )]
    pub(crate) p2p_bind_addr: Option<IpAddr>,
    #[arg(
        long,
        help = "P2P listen port. Defaults to the home's network.json, else a random port"
    )]
    pub(crate) p2p_port: Option<u16>,
    #[arg(long)]
    pub(crate) p2p_secret_key_path: Option<PathBuf>,
    #[arg(
        long,
        value_enum,
        help = "P2P relay mode. Defaults to the home's network.json, else disabled"
    )]
    pub(crate) p2p_relay_mode: Option<P2pRelayModeArg>,
    #[arg(
        long,
        value_enum,
        help = "P2P discovery. Defaults to the home's network.json, else disabled"
    )]
    pub(crate) p2p_discovery: Option<P2pDiscoveryArg>,
    #[arg(
        long,
        env = "DEFRA_P2P_MAX_PENDING_DAGS",
        help = "Maximum pending DAG registrations retained while Bitswap resolves missing links (must be > 0)"
    )]
    pub(crate) p2p_max_pending_dags: Option<usize>,
    #[arg(
        long,
        env = "DEFRA_P2P_MAX_CONCURRENT_PUSH_TASKS",
        help = "Maximum concurrent outbound PushLog worker slots (must be > 0). This is the hub fan-out semaphore that must free on timeout"
    )]
    pub(crate) p2p_max_concurrent_push_tasks: Option<usize>,
    #[arg(
        long,
        env = "DEFRA_P2P_MAX_CONCURRENT_DAG_FETCHES",
        help = "Maximum concurrent Bitswap DAG fetches while resolving missing links (must be > 0)"
    )]
    pub(crate) p2p_max_concurrent_dag_fetches: Option<usize>,
    #[arg(
        long,
        env = "DEFRA_P2P_RATE_LIMIT_BURST",
        help = "Per-peer P2P rate-limit burst capacity (must be > 0)"
    )]
    pub(crate) p2p_rate_limit_burst: Option<u32>,
    #[arg(
        long,
        env = "DEFRA_P2P_RATE_LIMIT_RATE",
        help = "Per-peer P2P rate-limit refill rate in tokens per second (must be finite and > 0)"
    )]
    pub(crate) p2p_rate_limit_rate: Option<f64>,
    #[arg(
        long = "apply-root",
        value_name = "ROOT",
        help = "After the server is ready, run config apply on this pack root (schemas/ then desired-state) against the in-process node. Rebinds pack placeholder DIDs to the home node (same as config apply --bind-node-did home --force-rebind-concrete-did)"
    )]
    pub(crate) apply_root: Option<PathBuf>,
    #[arg(
        long = "apply-prune",
        default_value_t = false,
        requires = "apply_root",
        help = "With --apply-root, prune live-only config docs absent from the pack (same as config apply --prune)"
    )]
    pub(crate) apply_prune: bool,
}

#[derive(clap::Args)]
pub(crate) struct CodexArgs {
    #[arg(
        long,
        default_value = crate::DEFAULT_CODEX_REMOTE,
        help = "Codex shim endpoint (ws://HOST:PORT, wss://HOST:PORT, or unix://PATH)"
    )]
    pub(crate) remote: String,
    #[arg(
        long,
        value_name = "ENV_VAR",
        value_parser = parse_env_var_name,
        help = "Environment variable containing the bearer token for the remote app server; requires wss:// or loopback ws://"
    )]
    pub(crate) remote_auth_token_env: Option<String>,
    #[arg(
        long,
        default_value_t = false,
        help = "Run inline, preserving terminal scrollback, instead of the alternate screen"
    )]
    pub(crate) no_alt_screen: bool,
    #[arg(
        value_name = "PROMPT",
        help = "Optional prompt to start the session with"
    )]
    pub(crate) prompt: Option<String>,
}

#[derive(clap::Args)]
pub(crate) struct ChatArgs {
    #[arg(long, help = "Node home directory. Defaults to ~/.gents")]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(long)]
    pub(crate) node_did: Option<String>,
    #[arg(long)]
    pub(crate) node_name: Option<String>,
    #[arg(
        long,
        help = "Continue an existing session instead of starting a fresh one"
    )]
    pub(crate) session_id: Option<String>,
    #[arg(long, help = "Override the agent for this one-off turn or session")]
    pub(crate) agent_id: Option<String>,
    #[arg(
        long,
        value_name = "OBJECTIVE",
        requires = "session_id",
        help = "Atomically create this durable session goal before the first request; omit on later turns"
    )]
    pub(crate) goal_objective: Option<String>,
    #[arg(
        long,
        value_name = "TOKENS",
        requires = "goal_objective",
        value_parser = clap::value_parser!(i64).range(1..),
        help = "Optional positive token budget for --goal-objective"
    )]
    pub(crate) goal_token_budget: Option<i64>,
    #[arg(long = "message-file", help = "Read the user message from a file")]
    pub(crate) message_file: Option<PathBuf>,
    #[arg(long = "output", value_enum, default_value_t = OutputFormat::Text)]
    pub(crate) output_format: OutputFormat,
    #[arg(long = "output-file", help = "Write the final response to a file")]
    pub(crate) output_file: Option<PathBuf>,
    #[arg(long, default_value_t = crate::DEFAULT_INTERACTIVE_WAIT_TIMEOUT_SECS)]
    pub(crate) timeout_secs: u64,
    #[arg(long, default_value_t = 1)]
    pub(crate) poll_secs: u64,
    #[arg(
        long,
        help = "Print raw tool call arguments/results as JSON instead of the short summary line"
    )]
    pub(crate) verbose: bool,
    #[arg(value_name = "MESSAGE")]
    pub(crate) message: Vec<String>,
}
