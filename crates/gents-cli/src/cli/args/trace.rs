use std::path::PathBuf;

use clap::{Subcommand, ValueEnum};

use super::ManifestNodeDidBindingArg;

#[derive(clap::Args)]
pub(crate) struct DiagnoseArgs {
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(long)]
    pub(crate) node_did: Option<String>,
    #[arg(long = "bind-node-did", value_enum)]
    pub(crate) bind_node_did: Option<ManifestNodeDidBindingArg>,
}

#[derive(Subcommand)]
pub(crate) enum TraceCommand {
    #[command(name = "export", about = "Export Amy-style tool-call JSONL")]
    Export(TraceExportArgs),
    #[command(
        name = "timeline",
        about = "Export a reconstructed run timeline for one AgentRequest"
    )]
    Timeline(TraceTimelineArgs),
    #[command(
        name = "project",
        about = "Project a reconstructed run into an adapter-facing interop shape"
    )]
    Project(TraceProjectArgs),
    #[command(
        name = "project-schema",
        about = "Print the JSON Schema for an adapter projection output"
    )]
    ProjectSchema(TraceProjectSchemaArgs),
    #[command(
        name = "capture",
        about = "Fetch rendered-request capture metadata, with the request_json field-commit CID"
    )]
    Capture(TraceCaptureArgs),
}

#[derive(clap::Args)]
pub(crate) struct TraceCaptureArgs {
    #[arg(long, help = "Node home directory. Defaults to ~/.gents")]
    pub(crate) home: Option<PathBuf>,
    #[arg(long, help = "GraphQL endpoint to read instead of local home state")]
    pub(crate) graphql: Option<String>,
    #[arg(
        long = "capture-key",
        conflicts_with = "request_id",
        help = "Fetch one capture by its unique capture_key"
    )]
    pub(crate) capture_key: Option<String>,
    #[arg(long = "request-id", help = "Fetch the captures of one request")]
    pub(crate) request_id: Option<String>,
    #[arg(
        long,
        requires = "request_id",
        help = "Narrow to one capture scope, e.g. inference.1 or compaction.2"
    )]
    pub(crate) scope: Option<String>,
    #[arg(long, requires = "request_id", help = "Narrow to one turn_index")]
    pub(crate) turn: Option<i64>,
    #[arg(long, requires = "request_id", help = "Narrow to one attempt")]
    pub(crate) attempt: Option<i64>,
    #[arg(
        long,
        help = "List every match as metadata instead of requiring exactly one"
    )]
    pub(crate) list: bool,
    #[arg(
        long = "include-body",
        help = "Include the recovered provider request_json, raw provenance manifest, and decoded provenance payload in the output"
    )]
    pub(crate) include_body: bool,
    #[arg(long = "output-file", help = "Write JSON to a file instead of stdout")]
    pub(crate) output_file: Option<PathBuf>,
}

#[derive(clap::Args)]
pub(crate) struct TraceExportArgs {
    #[arg(long, help = "Node home directory. Defaults to ~/.gents")]
    pub(crate) home: Option<PathBuf>,
    #[arg(long, help = "GraphQL endpoint to read instead of local home state")]
    pub(crate) graphql: Option<String>,
    #[arg(long, help = "Restrict export to one session_id")]
    pub(crate) session_id: Option<String>,
    #[arg(long, help = "Restrict export to one request_id")]
    pub(crate) request_id: Option<String>,
    #[arg(long, help = "Run id to stamp on exported JSONL records")]
    pub(crate) run_id: Option<String>,
    #[arg(long, help = "Case id to stamp on exported JSONL records")]
    pub(crate) case_id: Option<String>,
    #[arg(
        long,
        default_value_t = 500,
        help = "Maximum recent AgentToolCall rows to export"
    )]
    pub(crate) limit: usize,
    #[arg(long = "output-file", help = "Write JSONL to a file instead of stdout")]
    pub(crate) output_file: Option<PathBuf>,
}

#[derive(clap::Args)]
pub(crate) struct TraceTimelineArgs {
    #[arg(long, help = "Node home directory. Defaults to ~/.gents")]
    pub(crate) home: Option<PathBuf>,
    #[arg(long, help = "GraphQL endpoint to read instead of local home state")]
    pub(crate) graphql: Option<String>,
    #[arg(long = "request-id", help = "Request id to reconstruct")]
    pub(crate) request_id: String,
    #[arg(long = "output-file", help = "Write JSON to a file instead of stdout")]
    pub(crate) output_file: Option<PathBuf>,
}

#[derive(clap::Args)]
pub(crate) struct TraceProjectArgs {
    #[arg(long, help = "Node home directory. Defaults to ~/.gents")]
    pub(crate) home: Option<PathBuf>,
    #[arg(long, help = "GraphQL endpoint to read instead of local home state")]
    pub(crate) graphql: Option<String>,
    #[arg(long = "request-id", help = "Request id to reconstruct and project")]
    pub(crate) request_id: String,
    #[arg(long, value_enum, help = "Adapter projection to export")]
    pub(crate) projection: TraceProjectionArg,
    #[arg(
        long,
        value_enum,
        default_value_t = TraceProjectionRedactionArg::Full,
        help = "Redaction policy to apply before serializing adapter output"
    )]
    pub(crate) redaction: TraceProjectionRedactionArg,
    #[arg(
        long = "actor-did",
        help = "Actor identity used for projection provenance"
    )]
    pub(crate) actor_did: Option<String>,
    #[arg(
        long = "acp-policy-id",
        help = "DefraDB Document ACP policy id used to enforce row-level read decisions before projection. Requires --graphql and --actor-did."
    )]
    pub(crate) acp_policy_id: Option<String>,
    #[arg(
        long = "scope-node-did",
        help = "Require the root request to match this node DID and omit content-bearing events for other agents"
    )]
    pub(crate) scope_node_did: Option<String>,
    #[arg(
        long = "scope-agent-id",
        help = "Require the root request to match this agent id and omit content-bearing events for other agents"
    )]
    pub(crate) scope_agent_id: Option<String>,
    #[arg(
        long = "scope-session-id",
        help = "Require the root request to belong to this session id"
    )]
    pub(crate) scope_session_id: Option<String>,
    #[arg(
        long,
        value_enum,
        default_value_t = TraceProjectionFormatArg::Json,
        help = "Adapter projection output format; native-json omits the Gents envelope"
    )]
    pub(crate) format: TraceProjectionFormatArg,
    #[arg(
        long = "output-file",
        help = "Write projection output to a file instead of stdout"
    )]
    pub(crate) output_file: Option<PathBuf>,
}

#[derive(clap::Args)]
pub(crate) struct TraceProjectSchemaArgs {
    #[arg(long, value_enum, help = "Adapter projection schema to print")]
    pub(crate) projection: TraceProjectionArg,
    #[arg(
        long,
        value_enum,
        default_value_t = TraceProjectionFormatArg::Json,
        help = "Output schema for a projection envelope, native projection, or JSONL record"
    )]
    pub(crate) format: TraceProjectionFormatArg,
    #[arg(
        long = "output-file",
        help = "Write JSON Schema to a file instead of stdout"
    )]
    pub(crate) output_file: Option<PathBuf>,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum TraceProjectionArg {
    Atif,
    OpenaiCodex,
    Langgraph,
    MultiAgent,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum TraceProjectionRedactionArg {
    Full,
    TrainingSafe,
    Public,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub(crate) enum TraceProjectionFormatArg {
    Json,
    NativeJson,
    Jsonl,
    EvalJsonl,
}
