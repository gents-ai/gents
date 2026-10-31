use std::path::PathBuf;

use clap::{ArgAction, Subcommand};

use crate::cli::output_format::OutputFormat;
use crate::{BACKGROUND_AFTER_HELP, FLEET_AFTER_HELP, MCP_AFTER_HELP};

#[derive(Subcommand)]
pub(crate) enum BackgroundCommand {
    #[command(
        name = "list",
        about = "List backgrounded AgentToolCall rows",
        after_help = BACKGROUND_AFTER_HELP
    )]
    List(BackgroundListArgs),
}

#[derive(Subcommand)]
pub(crate) enum McpCommand {
    #[command(
        name = "register",
        about = "Create or update an MCP service registry entry",
        after_help = MCP_AFTER_HELP
    )]
    Register(McpRegisterArgs),
    #[command(
        name = "probe",
        about = "Run a one-shot health probe for registered MCP services",
        after_help = MCP_AFTER_HELP
    )]
    Probe(McpProbeArgs),
}

#[derive(clap::Args)]
pub(crate) struct McpRegisterArgs {
    #[arg(long, help = "Node home directory. Defaults to ~/.gents")]
    pub(crate) home: Option<PathBuf>,
    #[arg(long, help = "GraphQL endpoint to update instead of local home state")]
    pub(crate) graphql: Option<String>,
    #[arg(value_name = "SERVICE")]
    pub(crate) service: String,
    #[arg(long, value_name = "URL", help = "Streamable HTTP MCP endpoint")]
    pub(crate) endpoint: String,
    #[arg(long, value_name = "NAME")]
    pub(crate) display_name: Option<String>,
    #[arg(long, value_name = "TEXT")]
    pub(crate) description: Option<String>,
    #[arg(long, value_name = "VERSION", default_value = "unversioned")]
    pub(crate) version: String,
    #[arg(long, action = ArgAction::SetTrue)]
    pub(crate) send_node_did: bool,
}

#[derive(clap::Args)]
pub(crate) struct McpProbeArgs {
    #[arg(long, help = "Node home directory. Defaults to ~/.gents")]
    pub(crate) home: Option<PathBuf>,
    #[arg(
        long,
        help = "GraphQL endpoint to read registry rows instead of local home state"
    )]
    pub(crate) graphql: Option<String>,
    #[arg(long, action = ArgAction::SetTrue, help = "Probe every online MCP service")]
    pub(crate) all: bool,
    #[arg(long, default_value = "5s", value_name = "DURATION")]
    pub(crate) timeout: String,
    #[arg(long, value_enum, default_value_t = OutputFormat::Text)]
    pub(crate) output: OutputFormat,
    #[arg(value_name = "SERVICE")]
    pub(crate) service: Option<String>,
}

#[derive(Subcommand)]
pub(crate) enum FleetCommand {
    #[command(
        name = "slots",
        about = "Show derived fleet slot usage from the live runtime HTTP API",
        after_help = FLEET_AFTER_HELP
    )]
    Slots(FleetSlotsArgs),
}

#[derive(clap::Args)]
pub(crate) struct BackgroundListArgs {
    #[arg(long, help = "Node home directory. Defaults to ~/.gents")]
    pub(crate) home: Option<PathBuf>,
    #[arg(long, help = "GraphQL endpoint to read instead of local home state")]
    pub(crate) graphql: Option<String>,
    #[arg(
        long = "request",
        value_name = "ID",
        help = "Only show backgrounded tools for this parent request"
    )]
    pub(crate) request_id: Option<String>,
    #[arg(
        long,
        value_name = "STATE",
        help = "Only show tool calls whose displayed state matches this value"
    )]
    pub(crate) state: Option<String>,
    #[arg(
        long,
        value_name = "DURATION",
        help = "Only show calls older than this duration, e.g. 30s, 5m, 2h"
    )]
    pub(crate) age_gt: Option<String>,
    #[arg(long, value_enum, default_value_t = OutputFormat::Table)]
    pub(crate) output: OutputFormat,
}

#[derive(clap::Args)]
pub(crate) struct FleetSlotsArgs {
    #[arg(long, help = "Node home directory. Defaults to ~/.gents")]
    pub(crate) home: Option<PathBuf>,
    #[arg(long, help = "GraphQL endpoint for the live runtime")]
    pub(crate) graphql: Option<String>,
}

#[derive(clap::Args)]
pub(crate) struct StatusArgs {
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(long)]
    pub(crate) node_did: Option<String>,
}

#[derive(clap::Args)]
pub(crate) struct QueryArgs {
    #[arg(long, help = "BM25 keyword terms for search")]
    pub(crate) text: Option<String>,
    #[arg(long = "search-field", help = "String field to search (repeatable)")]
    pub(crate) search_fields: Vec<String>,
    #[arg(value_parser=["find","fields","count","search","explain","help"])]
    pub(crate) verb: Option<String>,
    #[arg(long, value_parser=["simple","execute"], help="Explain mode: simple inspects; execute measures the bounded read")]
    pub(crate) mode: Option<String>,
    #[arg(long, help = "Order as JSON, e.g. [{\"priority\":\"ASC\"}]")]
    pub(crate) order: Option<String>,
    #[arg(long, help = "Rows to skip, maximum 100000")]
    pub(crate) offset: Option<u32>,
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(
        long,
        default_value = "",
        help = "Collection (GraphQL type) to read, e.g. AgentRequest"
    )]
    pub(crate) collection: String,
    #[arg(
        long = "field",
        help = "Field to return (repeatable); at least one is required"
    )]
    pub(crate) fields: Vec<String>,
    #[arg(
        long,
        help = r#"DefraDB filter as JSON, e.g. '{"status":{"_eq":"completed"}}'"#
    )]
    pub(crate) filter: Option<String>,
    #[arg(
        long,
        help = "Maximum rows to return (find defaults to 50; search to 10; maximum 1000)"
    )]
    pub(crate) limit: Option<u32>,
    #[arg(
        long = "allow-collection",
        help = "Restrict the query to these collections (repeatable); omit for all"
    )]
    pub(crate) allow_collections: Vec<String>,
}

#[derive(clap::Args)]
pub(crate) struct WriteArgs {
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long)]
    pub(crate) graphql: Option<String>,
    #[arg(
        long,
        help = "Structured write tool call as JSON; argv:[help] lists syntax"
    )]
    pub(crate) call: String,
    #[arg(
        long = "allow-collection",
        required = true,
        help = "Exact application collection grant (repeatable)"
    )]
    pub(crate) collections: Vec<String>,
}

#[derive(Subcommand)]
pub(crate) enum DocumentCommand {
    #[command(
        about = "Create one document in a collection and print its document id",
        long_about = "Create one document in a collection and print its document id.\n\nThis is an operator command: it refuses protected eval/optimization and canonical configuration collections. Writes to the home runtime are signed as the home node; an unrelated explicit endpoint uses anonymous access with a warning. The mutation input schema is validated before writing."
    )]
    Create(DocumentCreateArgs),
}

#[derive(clap::Args)]
pub(crate) struct DocumentCreateArgs {
    #[arg(long)]
    pub(crate) home: Option<PathBuf>,
    #[arg(long, help = "GraphQL endpoint for the live runtime")]
    pub(crate) graphql: Option<String>,
    #[arg(
        value_name = "COLLECTION",
        help = "Collection (GraphQL type), e.g. Goal"
    )]
    pub(crate) collection: String,
    #[arg(
        long,
        help = r#"Document fields as a JSON object, e.g. '{"goal_id":"g1","status":"active"}'"#
    )]
    pub(crate) json: String,
}
