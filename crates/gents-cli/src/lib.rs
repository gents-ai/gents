use gents_service::home_state::default_data_dir;
use gents_service::home_state::home_graphql_endpoint;
use gents_service::home_state::init_config_path;
use gents_service::home_state::load_initialized_home_identity;
use gents_service::home_state::read_init_config;
use gents_service::home_state::read_runtime_state;
use gents_service::home_state::resolve_graphql_endpoint;
use gents_service::home_state::resolve_home_dir;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::Parser;
#[cfg(test)]
use gents::defra_node::NodeBuilder;

pub mod accounts {
    pub use crate::commands::accounts::{remove_account, request_usage_reads, UsageReads};
}
/// The job next-call rules the desktop shares with `gents automation job`.
pub mod jobs {
    pub use crate::commands::job::next_calls;
}
mod cli;
mod commands;
mod config_bundle;
mod config_import;
mod config_writes;
mod desired_state;
mod graphql_access;
mod interactive_backend;
mod next_call;
mod operation_failure;
pub mod packs;
mod request_helpers;
mod resolve_helpers;
pub mod server_host;
mod shared;
mod version;

use cli::*;

use config_bundle::*;
use config_import::*;
use config_writes::ConfigAccess;
use graphql_access::*;
use request_helpers::*;
use resolve_helpers::*;

#[cfg(test)]
#[ctor::ctor]
fn select_unit_test_homes() {
    gents_service::home_state::use_unit_test_homes();
}

const DEFAULT_INIT_ENDPOINT: &str = "http://127.0.0.1:8080/v1";
const DEFAULT_INIT_MODEL_NAME: &str = "google/gemma-4-12B-it-qat-q4_0-gguf";
const DEFAULT_OLLAMA_ENDPOINT: &str = "http://localhost:11434/v1";
const DEFAULT_OLLAMA_MODEL_NAME: &str = "hf.co/google/gemma-4-12B-it-qat-q4_0-gguf";
const DEFAULT_CHATGPT_CODEX_MODEL_NAME: &str = "gpt-6-astra";
const DEFAULT_XAI_GROK_OAUTH_MODEL_NAME: &str = "grok-4.7";
const DEFAULT_INTERACTIVE_WAIT_TIMEOUT_SECS: u64 = 86_400;
const TOKIO_WORKER_STACK_SIZE: usize = 16 * 1024 * 1024;
const DEFAULT_LOG_FILTER: &str = concat!(
    "warn,",
    "gents_host::serve=info,",
    "gents::shutdown=info,",
    "gents::runtime::agent::runtime=info,",
    "gents::runtime::agent::daemon=info,",
    "gents::runtime::agent::reconcile=info,",
    "gents::runtime::agent::loop_stream=info,",
    "gents::turn::provider_input=info,",
    "gents::inference::clients::chatgpt_codex=info,",
    "gents::tools::call::hook=info,",
    "gents::session::sessions=info,",
    "gents::request::streaming=info,",
    "gents::triggers::engine=info"
);
const ROOT_HELP_TEMPLATE: &str = "\
{about-with-newline}
{usage-heading} {usage}

\x1b[1;4mCommands:\x1b[0m
  accounts    Sign in to providers and gents cloud, and manage accounts
  automation  Goals, tasks, plugin jobs and the mailbox
  config      Agents, backends, tools, MCP services and packs
  runtime     Run, install, diagnose and reset the local runtime
  sessions    List, show and fork sessions
  status      Show the current local runtime status
  utils       Data, debugging, evals and optimization

Run `gents` with no command to set up a home and open a conversation.
Run `gents PROMPT` to open it with PROMPT as the first message.
Run `gents -p MESSAGE` to send one message and print the reply.
Run `gents --continue SESSION_ID` to pick a session back up with its agent.
Run `gents <command> --help` for details.

\x1b[1;4mOptions:\x1b[0m
{options}";
const INIT_AFTER_HELP: &str = "\
Bootstrap a local home directory with one default backend, one default agent, and a safe read-only Tools document.

Examples:
  gents init
  gents init --inference-url http://HOST:PORT/v1 --model-name MODEL
  gents init --backend-preset openrouter --model-name MODEL
  gents init --backend-preset openai --model-name MODEL
  gents init --backend-preset claude-cli-subscription   # then: gents accounts login claude
  gents init --write
  gents init --yolo
  gents init --inference-url $INFERENCE_ENDPOINT --model-name MODEL --write
  gents init --defra-query-collection AgentRequest
  gents init --identity-only
  gents init --identity-only --identity-backend macos-keychain --keychain-label LABEL
  gents init --identity-only --identity-backend macos-secure-enclave --secure-enclave-label LABEL

Next:
  llama-server -hf google/gemma-4-12B-it-qat-q4_0-gguf
  gents runtime server
  gents";
const PROVISION_AFTER_HELP: &str = "\
Provision binds a portable manifest root to this host's initialized identity,
applies it locally, and verifies an exact post-apply diff.

Examples:
  gents runtime provision --home /path/to/home --root infra/agents/HOST/AGENT
  gents runtime provision --root infra/agents/mini-1/mini-1-engineer
  gents runtime provision --root infra/agents/dev/dev-agent --bootstrap-file-identity
  gents runtime provision --root infra/agents/mini-1/mini-1-engineer --bootstrap-macos-keychain --keychain-label LABEL
  gents runtime provision --root infra/agents/mini-1/mini-1-engineer --bootstrap-macos-secure-enclave --secure-enclave-label LABEL

Production low-level flow:
  gents init --identity-only --identity-backend macos-keychain --keychain-label LABEL
  gents init --identity-only --identity-backend macos-secure-enclave --secure-enclave-label LABEL
  gents config apply --root <root> --home <home> --bind-node-did home
  gents config diff --root <root> --home <home> --bind-node-did home

File-key development flow:
  gents runtime provision --root <root> --bootstrap-file-identity";
const RESET_AFTER_HELP: &str = "\
Examples:
  gents runtime reset
  gents runtime reset --home /path/to/home";
const SERVER_AFTER_HELP: &str = "\
`server` reads the initialized home directory, starts the embedded DefraDB runtime, serves GraphQL locally, and starts IROH P2P for desktop pairing.

Common flow:
  gents

Identity note:
  Standalone server startup supports file keys, macOS keychain software-key homes initialized with identity_backend=macos-keychain, and macOS Secure Enclave homes initialized with identity_backend=macos-secure-enclave.
  Homes with a real node DID and no key_path must include a supported identity_backend and label in init.json.";
const P2P_AFTER_HELP: &str = "\
Examples:
  gents runtime p2p status
  gents runtime p2p peers --home /path/to/home
  gents runtime p2p diagnose

  # Authenticated enrollment operator decisions and observation:
  gents runtime p2p enrollment approve <request-id>
  gents runtime p2p enrollment deny <request-id>
  gents runtime p2p pairings list

  # Service discovery:
  gents runtime p2p network register
  gents runtime p2p network list
  gents runtime p2p templates list

  # Low-level non-authoritative live wiring for diagnostics/repair:
  gents runtime p2p admin connect --peer <peer-id-or-address>
  gents runtime p2p admin replicators add --peer <peer-id-or-address> --profile discovery";
const SCHEMA_AFTER_HELP: &str = "\
Apply app-specific DefraDB collection schemas to a running or local store.

Examples:
  gents utils data schema apply ./app-schemas
  gents utils data schema apply ./app-schemas --graphql http://127.0.0.1:9191/api/v0/graphql
  gents utils data schema apply ./schemas/action_request.graphql --patch ./schemas/action_request.patch.json

Directory inputs apply *.graphql and *.gql files, then additive patch files
named *.patch.json or *.json-patch. Patch files contain a JSON Patch array, or
an object with Patch/patch.";
const STATUS_AFTER_HELP: &str = "\
Status reads the local runtime by default.

Examples:
  gents status
  gents status --home /path/to/home
  gents status --graphql http://127.0.0.1:9191/api/v0/graphql";
const BACKGROUND_AFTER_HELP: &str = "\
Lists AgentToolCall rows persisted with await_mode=background and enriches them with live runtime liveness when available.
Live native-process enrichment requires --graphql pointing at the running runtime; local --home reads print NATIVE_TOOL=unknown.
Native-process matches are scoped by tool name because runtime liveness does not expose per-call native process IDs.

Examples:
  gents utils debug background list
  gents utils debug background list --home /path/to/home
  gents utils debug background list --graphql http://127.0.0.1:9191/api/v0/graphql
  gents utils debug background list --request REQUEST_ID
  gents utils debug background list --state running --age-gt 5m
  gents utils debug background list --output json";
const MCP_AFTER_HELP: &str = "\
Examples:
  gents config mcp register web --endpoint http://127.0.0.1:9213/mcp
  gents config mcp register web --endpoint http://127.0.0.1:9213/mcp --send-node-did
  gents config mcp probe SERVICE_ID
  gents config mcp probe --all
  gents config mcp probe SERVICE_ID --timeout 10s
  gents config mcp probe --all --graphql http://127.0.0.1:9191/api/v0/graphql --output json";
const FLEET_AFTER_HELP: &str = "\
Shows the live fleet slot-accounting snapshot exposed by the local runtime HTTP API.

Examples:
  gents utils debug fleet slots
  gents utils debug fleet slots --home /path/to/home
  gents utils debug fleet slots --graphql http://127.0.0.1:9191/api/v0/graphql";
const CLOUD_AFTER_HELP: &str = "\
Signs in through another device: the cloud prints a short code, you approve it on the
console page it names, and the workspace token is stored as an OAuthCredential document
for this node DID. Google authenticates you on that page; gents never sees a password.

Examples:
  gents accounts login cloud --cloud app.dev.gents.xyz
  GENTS_CLOUD=app.dev.gents.xyz gents accounts login cloud
  gents accounts login cloud --cloud http://127.0.0.1:9192 --home /path/to/home";
const TASK_AFTER_HELP: &str = "\
Inspect configured Task documents and create pending AgentRequests with manual trigger lineage.
For a Task with a durable-goal declaration, --session-id is a stable invocation key;
the emitted session_id is the deterministic Task/fire session derived from it.

Examples:
  gents automation task list
  gents automation task show host-check
  gents automation task run host-check
  gents automation task run host-check --args '{\"scope\":\"host\"}' --wait
  gents automation task run durable-task --session-id stable-invocation-key --wait
  gents automation task run --task-id host-check --graphql http://127.0.0.1:9191/api/v0/graphql";
const TRACE_AFTER_HELP: &str = "\
Exports one JSON object per persisted AgentToolCall row. The command reads
AgentSession, AgentRequest, AgentMessage, AgentOutputSegment, and
AgentToolCall rows, then infers Amy baseline fields without mutating runtime
state.

Examples:
  gents utils debug trace export --home /path/to/home
  gents utils debug trace export --graphql http://127.0.0.1:9191/api/v0/graphql
  gents utils debug trace export --graphql http://100.69.4.79:9191/api/v0/graphql --run-id amy-readonly-001 --limit 200 > amy-tool-calls.jsonl
  gents utils debug trace timeline --request-id REQUEST_ID --home /path/to/home
  gents utils debug trace project --projection atif --request-id REQUEST_ID --format native-json --output-file /logs/agent/trajectory.json --home /path/to/home
  gents utils debug trace project --projection openai-codex --request-id REQUEST_ID --redaction public --home /path/to/home
  gents utils debug trace project --projection langgraph --request-id REQUEST_ID --format jsonl --home /path/to/home
  gents utils debug trace project --projection multi-agent --request-id REQUEST_ID --scope-node-did DID --home /path/to/home
  gents utils debug trace project --projection multi-agent --request-id REQUEST_ID --format eval-jsonl --home /path/to/home
  gents utils debug trace project-schema --projection multi-agent --format eval-jsonl";
const CONFIG_AFTER_HELP: &str = "\
Examples:
  gents config validate --root infra/agents/default
  gents config validate --root infra/agents/default --home /path/to/home --bind-node-did home
  gents config diff --root infra/agents/default --home /path/to/home
  gents config apply --root infra/agents/default --home /path/to/home --bind-node-did home
  gents config backend set --graphql URL --file backend.json
  gents config backend discover-models --backend-preset openrouter
  gents config agent set --graphql URL --node-did <NODE_DID> --context-id <context-id> --inference-profile-id <profile-id>
  gents config tools set --graphql URL --file tools.json";
const REQUEST_AFTER_HELP: &str = "\
`utils debug request` is the low-level document path. Most users should prefer `gents -p MESSAGE`.

Examples:
  gents utils debug request submit --content \"summarize this repo\"
  gents utils debug request show REQUEST_ID";
const RESPONSE_AFTER_HELP: &str = "\
Views the canonical terminal output of a request: the request row's
lifecycle_state and terminal_output selection.

Examples:
  gents utils debug response wait REQUEST_ID
  gents utils debug response show REQUEST_ID";
const SESSION_AFTER_HELP: &str = "\
Fork a conversation into a new session seeded from a user-turn prefix \
of the source. Child inherits node identity; agent can be swapped with \
--agent.";
const DIAGNOSE_AFTER_HELP: &str = "\
Examples:
  gents runtime diagnose
  gents runtime diagnose --home /path/to/home
  gents runtime diagnose --graphql http://127.0.0.1:9191/api/v0/graphql";
const TOOLS_AFTER_HELP: &str = "\
Examples:
  gents config tools explain
  gents config tools explain --agent-did AGENT_DID
  gents config tools explain --graphql http://127.0.0.1:9191/api/v0/graphql

The explain output separates model-callable tools from operator HTTP/MCP surfaces
and includes warnings for confusing defaults such as empty allowlists that mean
all, or built-in read tools that are always included today.";
const CONFIG_EXPORT_AFTER_HELP: &str = "\
Exports the desired configuration documents for one node as a
manifest root directory (`pack_config.json` with optional prompt sidecars).
The output is designed to be committed to version control and applied with
`config apply --root <dir>`.

Examples:
  gents config export --root ./my-agent
  gents config export --root ./my-agent --force
  gents config export --root ./my-agent --node-did <NODE_DID>
  gents config export --root ./my-agent --home /path/to/home --bind-node-did home";
pub(crate) const CONFIG_EXPORT_FORMAT: &str = "gents-config/v2";

pub use commands::prepare_runtime::prepare_runtime_after_unlock;

/// Embedded editor launches must never trigger a system authentication dialog.
/// Explicit credential access remains with the existing browser or store-unlock owner.
pub fn run_cli_without_keychain_prompts() -> Result<()> {
    without_keychain_prompts(run_cli)
}

/// The platform interaction prohibition is process-wide and retains any inherited
/// prohibition. Native startup uses it when creating fresh credential custody.
pub fn without_keychain_prompts<T>(operation: impl FnOnce() -> Result<T>) -> Result<T> {
    let _interaction = gents_service::process_resources::prevent_keychain_prompts(true)?;
    operation()
}

pub fn run_cli() -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_stack_size(TOKIO_WORKER_STACK_SIZE)
        .build()
        .context("building tokio runtime")?;
    runtime.block_on(async_main(runtime.handle().clone()))
}

async fn async_main(runtime: tokio::runtime::Handle) -> Result<()> {
    let cli = Cli::parse();
    let command = match cli.command {
        None => {
            return commands::start::run(
                commands::start::StartArgs {
                    home: cli.home,
                    store_key_custody: cli.store_key_custody,
                    agent: cli.agent,
                    print: cli.print,
                    steer: cli.steer,
                    prompt: cli.prompt,
                    continue_session: cli.continue_session,
                    yolo: cli.yolo,
                },
                runtime,
            )
            .await
        }
        Some(Command::NativeFsRunner(args)) => {
            return commands::native_fs_runner::native_fs_runner(args);
        }
        Some(Command::PluginBuild(args)) => {
            return packs::plugin_build(args.dir, args.test).await;
        }
        Some(Command::DocsBundle(args)) => return commands::docs_bundle::docs_bundle(args),
        Some(Command::Shell(args)) => return commands::start::shell(args, runtime).await,
        Some(command) => command,
    };
    gents_tools::native::enable_self_runner();
    if let Command::Utils {
        command: UtilsCommand::Eval { command },
    } = &command
    {
        if let Some(message) = commands::eval::usage_error(command) {
            use clap::CommandFactory;
            Cli::command()
                .error(clap::error::ErrorKind::ArgumentConflict, message)
                .exit();
        }
    }

    // A read-only look at a home an older Gents made gives the repair's report
    // of it, before anything is asked of a node that home cannot have.
    let read_only_home = match &command {
        Command::Status(args) if args.graphql.is_none() => Some(args.home.clone()),
        Command::Service {
            command: ServiceCommand::Status(target),
        }
        | Command::Runtime {
            command:
                RuntimeCommand::Service {
                    command: ServiceCommand::Status(target),
                },
        } => Some(target.home.clone()),
        _ => None,
    };
    if let Some(home) = read_only_home {
        gents_service::repair::home::refuse_older_home(&resolve_home_dir(home.as_deref())).await?;
    }
    let telemetry = gents_service::telemetry::init(DEFAULT_LOG_FILTER)?;
    let result = match command {
        Command::Init(args) => commands::init::init(args).await,
        Command::Server(args) => {
            gents_host::serve::serve(
                args,
                runtime,
                std::sync::Arc::new(commands::serve_client::CliServeClient),
            )
            .await
        }
        Command::Service { command } => service_command(command).await,
        Command::PrepareRuntime(args) => {
            gents_host::prepare_runtime::prepare(
                args,
                runtime,
                std::sync::Arc::new(commands::serve_client::CliServeClient),
            )
            .await
        }
        Command::Status(args) => commands::status::status(args).await,
        Command::Sessions { command } => commands::session::dispatch(command).await,
        Command::Automation { command } => match command {
            AutomationCommand::Goal { command } => commands::goal::dispatch(command).await,
            AutomationCommand::Task { command } => commands::task::dispatch(command).await,
            AutomationCommand::Job { command } => commands::job::dispatch(command).await,
            AutomationCommand::Mailbox { command } => commands::mailbox::dispatch(command).await,
        },
        Command::Accounts { command } => match command {
            AuthCommand::Login { command } => match command {
                AuthLoginCommand::Codex(args) => commands::codex_login::codex_login(args).await,
                AuthLoginCommand::Grok(args) => commands::grok_login::grok_login(args).await,
                AuthLoginCommand::Claude(args) => commands::claude_login::claude_login(args).await,
                AuthLoginCommand::ClaudeGateway(args) => {
                    commands::claude_gateway_login::claude_gateway_login(args).await
                }
                AuthLoginCommand::Cloud(args) => commands::cloud::cloud_login(args).await,
            },
            AuthCommand::Probe { command } => match command {
                AuthProbeCommand::Codex(args) => {
                    commands::codex_auth_probe::codex_auth_probe(args).await
                }
                AuthProbeCommand::Grok(args) => {
                    commands::grok_auth_probe::grok_auth_probe(args).await
                }
            },
            AuthCommand::Accounts(command) => commands::accounts::dispatch(command).await,
        },
        Command::Config { command } => commands::config::dispatch(command).await,
        Command::Runtime { command } => match command {
            RuntimeCommand::Server(args) => {
                gents_host::serve::serve(
                    *args,
                    runtime,
                    std::sync::Arc::new(commands::serve_client::CliServeClient),
                )
                .await
            }
            RuntimeCommand::Service { command } => service_command(command).await,
            RuntimeCommand::Provision(args) => commands::provision::provision(args).await,
            RuntimeCommand::Reset(args) => commands::reset::reset(args).await,
            RuntimeCommand::Diagnose(args) => commands::diagnose::diagnose(args).await,
            RuntimeCommand::P2p { command } => commands::p2p::dispatch(command).await,
            RuntimeCommand::Desktop { command } => commands::desktop::run(command).await,
            RuntimeCommand::Version => {
                print!("{}", version::version_text());
                Ok(())
            }
        },
        Command::Utils { command } => match command {
            UtilsCommand::Data { command } => match command {
                DataCommand::Query(args) => commands::query::query(args).await,
                DataCommand::Write(args) => commands::write::write(args).await,
                DataCommand::Export(args) => commands::export::export(args).await,
                DataCommand::Document { command } => commands::document::dispatch(command).await,
                DataCommand::Schema { command } => commands::schema::dispatch(command).await,
            },
            UtilsCommand::Debug { command } => match command {
                DebugCommand::Request { command } => commands::request::dispatch(command).await,
                DebugCommand::Response { command } => commands::response::dispatch(command).await,
                DebugCommand::Trace { command } => commands::trace::dispatch(command).await,
                DebugCommand::Background { command } => {
                    commands::background::dispatch(command).await
                }
                DebugCommand::Fleet { command } => commands::fleet::dispatch(command).await,
            },
            UtilsCommand::Eval { command } => commands::eval::dispatch(command, runtime).await,
            UtilsCommand::Optimization { command } => {
                commands::optimization::dispatch(command, runtime).await
            }
        },
        Command::Shell(_)
        | Command::NativeFsRunner(_)
        | Command::PluginBuild(_)
        | Command::DocsBundle(_) => {
            unreachable!("handled before telemetry initialization")
        }
    };
    telemetry.shutdown();
    result
}

async fn service_command(command: ServiceCommand) -> Result<()> {
    let command = match command {
        ServiceCommand::Update(args) => return gents_service::service_update::submit(args).await,
        ServiceCommand::UpdateStatus { home, operation_id } => {
            return gents_service::service_update::status(home, operation_id).await
        }
        ServiceCommand::UpdateRun { home, operation_id } => {
            return gents_service::service_update::run(home, operation_id).await
        }
        ServiceCommand::UpdateCheck { home } => {
            return gents_service::service_update::check(home).await
        }
        ServiceCommand::Repoint { home } => return commands::start::repoint(home).await,
        command => command,
    };
    use gents_service::lifecycle::ServiceLifecycleReceipt;
    use gents_service::native_service::{NativeServiceConfig, NativeServiceManager};

    tokio::task::spawn_blocking(move || -> Result<()> {
        let (home, executable, action) = command.into_parts();
        let home = absolutize(resolve_home_dir(home.as_deref()))?;
        let executable = match executable {
            Some(path) => absolutize(path)?,
            None => std::env::current_exe().context("resolving the gents executable")?,
        };
        let manager = NativeServiceManager::new(NativeServiceConfig::new(home, executable)?)?;
        match gents_service::lifecycle::perform(&manager, &action)? {
            ServiceLifecycleReceipt::Status { service, network } => {
                print!("{}\n{}\n", service.summary(), network.summary());
            }
            ServiceLifecycleReceipt::Uninstalled {
                definition,
                removed,
            } => {
                for line in commands::service::uninstall_summary(
                    manager.home(),
                    &definition,
                    removed,
                    manager.config().stderr_path.as_deref(),
                ) {
                    println!("{line}");
                }
            }
            ServiceLifecycleReceipt::Done | ServiceLifecycleReceipt::Installed(_) => {}
        }
        Ok(())
    })
    .await?
}

fn absolutize(path: PathBuf) -> Result<PathBuf> {
    if path.is_absolute() {
        Ok(path)
    } else {
        Ok(std::env::current_dir()
            .context("resolving current directory")?
            .join(path))
    }
}

/// A path as one shell word, for a command printed for a person to paste. Only an
/// interior NUL cannot be quoted, and no path has one.
pub(crate) fn shell_word(path: &Path) -> String {
    let path = path.to_string_lossy();
    shlex::try_quote(&path).map_or_else(|_| path.to_string(), |quoted| quoted.into_owned())
}

pub(crate) async fn resolve_config_access(
    home: Option<&Path>,
    explicit_graphql: Option<&str>,
) -> Result<(ConfigAccess, PathBuf)> {
    let (access, home, _) =
        resolve_config_access_inner(home, explicit_graphql, false, None).await?;
    Ok((access, home))
}

/// [`resolve_config_access`] for a caller holding the home's exclusive plugin
/// store lock across the access, so an offline open reads installed plugins
/// under that guard instead of contending with it.
pub(crate) async fn resolve_config_access_under_plugin_store(
    home: &Path,
    guard: &gents_pack::plugin::store::StoreGuard,
) -> Result<(ConfigAccess, PathBuf)> {
    let (access, home, _) =
        resolve_config_access_inner(Some(home), None, false, Some(guard)).await?;
    Ok((access, home))
}

pub(crate) async fn resolve_pack_config_access(
    home: Option<&Path>,
    explicit_graphql: Option<&str>,
) -> Result<(
    ConfigAccess,
    PathBuf,
    Option<gents_store::config_client::GraphqlEndpoint>,
)> {
    resolve_config_access_inner(home, explicit_graphql, true, None).await
}

async fn resolve_config_access_inner(
    home: Option<&Path>,
    explicit_graphql: Option<&str>,
    local_http: bool,
    plugin_guard: Option<&gents_pack::plugin::store::StoreGuard>,
) -> Result<(
    ConfigAccess,
    PathBuf,
    Option<gents_store::config_client::GraphqlEndpoint>,
)> {
    use gents_store::identity::NodeIdentity as _;
    let home_dir = resolve_home_dir(home);
    if let Some(guard) = plugin_guard {
        guard.for_home(&home_dir)?;
    }
    let client_identity = gents_service::home_state::client_identity_for(&home_dir).await?;
    if let Some(graphql) = explicit_graphql
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let endpoint = resolve_graphql_endpoint(Some(graphql), home).await?;
        return Ok((
            ConfigAccess::Graphql(endpoint.with_client_signed_agent_creates()),
            home_dir,
            None,
        ));
    }
    if let Some(runtime_state) = read_runtime_state(&home_dir)? {
        if graphql_endpoint_available(&runtime_state.graphql).await {
            let endpoint = home_graphql_endpoint(&home_dir, runtime_state.graphql).await?;
            return Ok((
                ConfigAccess::Graphql(endpoint.with_client_signed_agent_creates()),
                home_dir,
                None,
            ));
        }
    }

    let data_dir = default_data_dir(&home_dir);
    anyhow::ensure!(
        tokio::fs::metadata(&data_dir)
            .await
            .is_ok_and(|metadata| metadata.is_dir()),
        "the initialized data directory {} is unavailable",
        data_dir.display()
    );
    let claim = gents_store::local::home::lock_store(&home_dir, &data_dir).map_err(|error| {
        let held = error.lock_held().is_some();
        let error = anyhow::Error::from(error);
        if held {
            error.context(
                "the home's store is in use; to act through the running runtime, rerun with --graphql",
            )
        } else {
            error
        }
    })?;
    let store_key =
        gents_store::local::store_key::open_home_store_key(&home_dir, &data_dir).await?;
    let protected = gents_store::local::backend_protection::ProtectedBackendStore::open(
        &data_dir, &store_key, &claim,
    )
    .await?;
    let config = read_init_config(&home_dir)?.context("home is not initialized")?;
    let runtime_identity = load_initialized_home_identity(&home_dir, &config)?;
    let protected = protected
        .preflight(&store_key, Some(runtime_identity.did()))
        .await?;
    // Offline client writes are signed by the client key and travel through
    // this owned node's loopback endpoint, as `gents init` publishes them.
    let (listener, address) = tokio::task::spawn_blocking(|| -> std::io::Result<_> {
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        Ok((listener, address))
    })
    .await??;
    let endpoint = gents_store::config_client::GraphqlEndpoint::as_principal(
        format!("http://{address}/api/v0/graphql"),
        client_identity.did(),
    )
    .with_client_signing_capability(client_identity.client_signing_capability()?)?
    .with_client_signed_agent_creates();
    // Owner-staged relationship changes of those writes are decided by this
    // node's own route, as the serving runtime decides them.
    let owner_staged = gents_store::config_client::OwnerStagedRoute::new();
    let http = Some(
        gents::defra_node::HttpConfig::with_listener(listener)?
            .with_extra_routes(owner_staged.router()),
    );
    let (node, _admission) = match plugin_guard {
        Some(guard) => {
            gents::runtime::node_start::start_node_under_plugin_store(
                protected,
                &store_key,
                runtime_identity.clone(),
                http,
                None,
                gents_tools::surface::ToolCeiling::meta_only(),
                guard,
            )
            .await?
        }
        None => {
            gents::runtime::node_start::start_node(
                protected,
                &store_key,
                runtime_identity.clone(),
                http,
                None,
                gents_tools::surface::ToolCeiling::meta_only(),
                &home_dir,
            )
            .await?
        }
    };
    let migration_node = node.clone();
    let access = ConfigAccess::claimed_local_as(
        node.clone(),
        claim,
        identity::Did::new(client_identity.did().to_owned())?,
    )?
    .with_client_signed_writes_through(endpoint.clone())?;
    let local_endpoint = local_http.then_some(endpoint);
    gents_store::schema::migration::ensure_all_runtime_migrations(migration_node)
        .await
        .map_err(|error| {
            gents_store::local::storage_backend::classify_store_error(error, &data_dir)
        })?;
    gents_store::local::store_key::upgrade::finish(&data_dir)?;
    owner_staged.serve(&node, runtime_identity.clone());
    Ok((access, home_dir, local_endpoint))
}

#[cfg(test)]
pub(crate) async fn persistent_node_builder(
    home_dir: &Path,
    data_dir: &Path,
) -> Result<NodeBuilder> {
    gents_store::local::storage_backend::reject_legacy_store(data_dir)?;
    let key = gents_store::local::store_key::open_home_store_key(home_dir, data_dir).await?;
    Ok(gents_store::local::store_key::persistent_builder(
        data_dir, &key,
    )?)
}

#[cfg(test)]
pub(crate) async fn persistent_node_builder_with_stored_identity(
    home_dir: &Path,
    data_dir: &Path,
) -> Result<NodeBuilder> {
    let config = read_init_config(home_dir)?.ok_or_else(|| {
        anyhow::anyhow!(
            "gents home {} is not initialized; run `gents --home {}` first",
            home_dir.display(),
            home_dir.display()
        )
    })?;
    let identity = load_initialized_home_identity(home_dir, &config)?;
    Ok(persistent_node_builder(home_dir, data_dir)
        .await?
        .with_node_identity_did(identity.did().to_string()))
}

pub(crate) fn require_non_empty<'a>(field: &str, value: &'a str) -> Result<&'a str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        anyhow::bail!("--{field} must not be empty");
    }
    Ok(trimmed)
}

/// `home` must come from [`overwritable_home`]; the held store lock is kept so
/// a runtime cannot take the store while it is wiped.
pub(crate) fn dangerously_overwrite_home(home: &Path, keep: &Path) -> Result<()> {
    remove_tree_except(home, keep)
        .with_context(|| format!("dangerously overwriting {}", home.display()))
}

pub(crate) fn overwritable_home(
    home_dir: &Path,
    user_home: Option<&Path>,
) -> Result<Option<PathBuf>> {
    if home_dir.as_os_str().is_empty() {
        anyhow::bail!("refusing to dangerously overwrite an empty home path");
    }
    let home = match fs::canonicalize(home_dir) {
        Ok(home) => home,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("resolving {}", home_dir.display()))
        }
    };
    if home.parent().is_none() {
        anyhow::bail!(
            "refusing to dangerously overwrite {} (the filesystem root)",
            home_dir.display()
        );
    }
    let Some(user_home) = user_home.filter(|path| !path.as_os_str().is_empty()) else {
        anyhow::bail!(
            "refusing to dangerously overwrite {}: the user home directory cannot be determined, so it cannot be shown not to be inside the home",
            home_dir.display()
        );
    };
    let user_home = fs::canonicalize(user_home).with_context(|| {
        format!(
            "refusing to dangerously overwrite {}: the user home directory {} cannot be resolved, so it cannot be shown not to be inside the home",
            home_dir.display(),
            user_home.display()
        )
    })?;
    if user_home.starts_with(&home) {
        anyhow::bail!(
            "refusing to dangerously overwrite {}: it resolves to {}, which is or contains the user home directory; pass a dedicated gents home instead",
            home_dir.display(),
            home.display()
        );
    }
    if !home.is_dir() {
        anyhow::bail!("{} is not a directory", home_dir.display());
    }
    Ok(Some(home))
}

fn remove_tree_except(dir: &Path, keep: &Path) -> Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path == keep {
            continue;
        }
        let file_type = entry.file_type()?;
        if file_type.is_dir() && keep.starts_with(&path) {
            remove_tree_except(&path, keep)?;
        } else if file_type.is_dir() {
            fs::remove_dir_all(&path)?;
        } else {
            fs::remove_file(&path)?;
        }
    }
    Ok(())
}

pub(crate) fn server_start_failure_hint(home_dir: &Path) -> String {
    format!(
        "Next:\n  1. For the default local backend, run `llama-server -hf {DEFAULT_INIT_MODEL_NAME}` and make sure it is listening on {DEFAULT_INIT_ENDPOINT}\n  2. Point the backend elsewhere: save the output of `gents config backend show <ID> --home {home}` without its `_docID`, edit `endpoint`, and load it with `gents config backend set --file <backend.json> --home {home}` (`gents config backend list --home {home}` names the backend)\n  3. Inspect the initialized home at {}\n  4. If persisted runtime state is stale, run `gents runtime reset --home {}`",
        init_config_path(home_dir).display(),
        home_dir.display(),
        home = shell_word(home_dir),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents_service::args::ToolCeilingArg;
    use gents_service::home_state::default_key_path;
    use gents_service::home_state::write_init_config;
    use gents_service::state::StoredInitConfig;
    use gents_store::identity::NodeIdentity as _;

    #[tokio::test]
    async fn initialized_offline_node_reuses_the_home_signer() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let data = default_data_dir(&home);
        let key_path = default_key_path(&home, "default");
        std::fs::create_dir_all(key_path.parent().unwrap()).unwrap();
        let identity = gents_store::identity::KeyIdentity::load_or_create(&key_path, None).unwrap();
        let did = identity.did().to_string();
        write_init_config(
            &home,
            &StoredInitConfig {
                home: home.to_string_lossy().to_string(),
                node_name: "default".to_string(),
                node_did: did.clone(),
                key_path: Some(key_path.to_string_lossy().to_string()),
                identity_backend: None,
                keychain_label: None,
                secure_enclave_label: None,
                tool_package: None,
                tool_ceiling: ToolCeilingArg::Readonly,
                tool_root: None,
                store_encryption: Some(
                    gents_store::local::store_key::StoreEncryption::prepare(
                        gents_store::local::store_key::StoreKeyCustodyChoice::File,
                        &gents_store::local::store_key::home_key_file(&home),
                        &default_data_dir(&home),
                    )
                    .unwrap(),
                ),
            },
        )
        .unwrap();

        let node = persistent_node_builder_with_stored_identity(&home, &data)
            .await
            .unwrap()
            .build()
            .await
            .unwrap();
        assert_eq!(node.node_identity_did(), Some(did.as_str()));
    }

    #[tokio::test]
    async fn an_unrecorded_key_is_not_adopted_when_opening_an_older_home() {
        let temp = tempfile::tempdir().unwrap();
        let home = initialized_home(temp.path());
        let mut config = read_init_config(&home).unwrap().unwrap();
        config.store_encryption = None;
        write_init_config(&home, &config).unwrap();
        let data = default_data_dir(&home);
        let key_file = gents_store::local::store_key::home_key_file(&home);
        let before = std::fs::read(&key_file).unwrap();

        let error = match persistent_node_builder_with_stored_identity(&home, &data).await {
            Ok(_) => panic!("an unrecorded key must not be adopted"),
            Err(error) => error,
        };
        assert!(error.to_string().contains("unrecorded file"), "{error:#}");
        assert_eq!(std::fs::read(key_file).unwrap(), before);
        assert!(!data.join("MANIFEST").exists(), "the store is never opened");
    }

    fn initialized_home(temp: &Path) -> PathBuf {
        let home = temp.join("home");
        let key_path = default_key_path(&home, "default");
        std::fs::create_dir_all(key_path.parent().unwrap()).unwrap();
        let identity = gents_store::identity::KeyIdentity::load_or_create(&key_path, None).unwrap();
        write_init_config(
            &home,
            &StoredInitConfig {
                home: home.to_string_lossy().to_string(),
                node_name: "default".to_string(),
                node_did: identity.did().to_string(),
                key_path: Some(key_path.to_string_lossy().to_string()),
                identity_backend: None,
                keychain_label: None,
                secure_enclave_label: None,
                tool_package: None,
                tool_ceiling: ToolCeilingArg::Readonly,
                tool_root: None,
                store_encryption: Some(
                    gents_store::local::store_key::StoreEncryption::prepare(
                        gents_store::local::store_key::StoreKeyCustodyChoice::File,
                        &gents_store::local::store_key::home_key_file(&home),
                        &default_data_dir(&home),
                    )
                    .unwrap(),
                ),
            },
        )
        .unwrap();
        read_init_config(&home)
            .unwrap()
            .unwrap()
            .store_encryption
            .unwrap()
            .initialize(
                &gents_store::local::store_key::home_key_file(&home),
                &default_data_dir(&home),
            )
            .unwrap();
        home
    }

    /// A home as `gents init` leaves it: its config, store key and a fresh
    /// protected store whose provenance names the runtime.
    async fn initialized_home_with_store(temp: &Path) -> PathBuf {
        let home = initialized_home(temp);
        let config = read_init_config(&home).unwrap().unwrap();
        let identity = load_initialized_home_identity(&home, &config).unwrap();
        let data = default_data_dir(&home);
        std::fs::create_dir_all(&data).unwrap();
        let key = gents_store::local::store_key::open_home_store_key(&home, &data)
            .await
            .unwrap();
        let lock = gents_store::local::home::lock_store(&home, &data).unwrap();
        gents_store::local::backend_protection::FreshBackendStore::observe(&data, &lock)
            .await
            .unwrap()
            .initialize(&key, Some(identity.did()))
            .await
            .unwrap();
        drop(lock);
        home
    }

    #[tokio::test]
    async fn an_offline_access_claims_its_store_until_the_node_is_released() {
        let temp = tempfile::tempdir().unwrap();
        let home = initialized_home_with_store(temp.path()).await;

        let (access, _) = resolve_config_access(Some(&home), None).await.unwrap();
        let node = access
            .local_node()
            .expect("an offline home opens its store");
        let node = std::sync::Arc::downgrade(node);

        let error = gents_store::local::home::lock_store(&home, &default_data_dir(&home))
            .expect_err("an opened store excludes another runtime");
        assert!(
            error.lock_held().is_some(),
            "{}",
            gents_protocol::error_chain::ErrorChain(&error)
        );

        access
            .local_node()
            .unwrap()
            .shutdown()
            .await
            .expect("node shutdown");
        drop(access);
        assert_eq!(node.strong_count(), 0, "the access held the only node");
        let (reopened, _) = resolve_config_access(Some(&home), None)
            .await
            .expect("a shut down and dropped access releases the store");
        assert_eq!(reopened.mode(), "local");
        reopened
            .local_node()
            .unwrap()
            .shutdown()
            .await
            .expect("node shutdown");
    }

    #[tokio::test]
    async fn an_offline_access_names_the_holder_of_a_claimed_store() {
        let temp = tempfile::tempdir().unwrap();
        let home = initialized_home_with_store(temp.path()).await;
        let _held = gents_store::local::home::lock_home_store(&home).unwrap();

        let error = match resolve_config_access(Some(&home), None).await {
            Ok(_) => panic!("a claimed store must not be opened a second time"),
            Err(error) => error,
        };

        assert!(
            gents_store::local::home::store_lock_held(error.as_ref()).is_some(),
            "{error:#}"
        );
    }

    #[tokio::test]
    async fn uninitialized_offline_home_requires_init() {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        let data = default_data_dir(&home);

        let error = match persistent_node_builder_with_stored_identity(&home, &data).await {
            Ok(_) => panic!("uninitialized home should not produce an unsigned node builder"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("run `gents --home"));
    }

    #[test]
    fn canonical_import_rejects_observation_fields_instead_of_silently_dropping_them() {
        let service = serde_json::json!({"node_did":"owner","service_id":"mcp","hostname":"host"});
        for (field, value) in [
            ("status", serde_json::json!("offline")),
            ("tools", serde_json::json!([{"name":"tool"}])),
            ("version", serde_json::json!("1.2.3")),
            ("updated_at", serde_json::json!("2026-01-01T00:00:00Z")),
        ] {
            let mut input = service.clone();
            input[field] = value;
            assert!(
                gents_documents::config_projection(
                    gents_store::collection::Collection::ToolServiceRegistry,
                    Some(&input)
                )
                .is_err(),
                "{field} is not configuration"
            );
        }
    }

    #[test]
    fn canonical_service_import_preserves_optional_endpoint_and_exact_values() {
        let input = serde_json::json!({"node_did":"owner","service_id":"mcp","hostname":"host","mcp_path":null});
        let projected = gents_documents::config_projection(
            gents_store::collection::Collection::ToolServiceRegistry,
            Some(&input),
        )
        .unwrap()
        .1
        .unwrap();
        assert_eq!(projected["hostname"], "host");
        assert!(projected["mcp_path"].is_null());
        assert!(projected.get("status").is_none());
        assert_eq!(projected["node_did"], "owner");
        let mut missing_owner = input;
        missing_owner.as_object_mut().unwrap().remove("node_did");
        assert!(gents_documents::config_projection(
            gents_store::collection::Collection::ToolServiceRegistry,
            Some(&missing_owner)
        )
        .is_err());
    }
}
