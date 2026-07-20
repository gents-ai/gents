//! Typed per-collection control-plane write client (#654).
//!
//! One proven write path for config collections, shared by two consumers:
//! the CLI `config apply`/imperative commands (which historically owned these
//! writers under `defra-agent-cli/src/config_writes`) and the runtime
//! self-configuration tools (`crate::self_config`).
//!
//! The client is DID-parameterized: [`ConfigApplyTxn::begin_local`] accepts an
//! optional `identity::Did`, and every statement executed inside that
//! transaction carries it as the DefraDB ACP actor — authorization is
//! enforced at the node, not by app-level ownership checks. The CLI paths
//! ([`ConfigAccess::execute`] / [`ConfigAccess::begin_apply_txn`]) remain
//! identity-less, preserving their existing behavior.
//!
//! Write conventions (load-bearing — see `CLAUDE.md`):
//! - every interpolated value goes through
//!   [`crate::graphql::escape_graphql_string`];
//! - list fields never render `[]` (typed as `JsonArray`, corrupts nillable
//!   array columns) — the shared field encoders emit `null` instead;
//! - `Option` fields omitted from an `update:` clause preserve the stored
//!   value; explicit clearing requires `field: null`.

mod agent_behavior;
mod common;
mod event_trigger;
mod inference_backend;
mod schedule;
mod task;
mod txn;

pub mod patch;

pub use agent_behavior::write_agent_behavior_document;
pub use common::{mint_recreate_identity, mint_recreate_identity_timestamp};
pub use event_trigger::write_event_trigger_document;
pub use inference_backend::{write_inference_backend_document, InferenceBackendUpsertDocument};
pub use schedule::write_schedule_document;
pub use task::write_task_document;
pub use tool_selection::{
    write_tool_selection_document, write_tool_selection_document_with_clear_fields,
};
pub use txn::ConfigApplyTxn;

mod tool_selection;

use std::sync::Arc;

use anyhow::Result;
use defra_agent_protocol::graphql::{execute_graphql_async, GraphqlRequestOptions};
use defra_node::EmbeddedNode;
use serde_json::{json, Value};

pub enum ConfigAccess {
    /// HTTP GraphQL endpoint. **Must end with `/graphql`** — transaction
    /// begin/commit/discard derive the REST API base by stripping that suffix.
    Graphql(String),
    /// Shared so callers that already hold the node (desktop client) can
    /// construct access without moving it; `EmbeddedNode` is not `Clone`.
    Local(Arc<EmbeddedNode>),
}

impl ConfigAccess {
    pub fn mode(&self) -> &'static str {
        match self {
            Self::Graphql(_) => "graphql",
            Self::Local(_) => "local",
        }
    }

    pub async fn execute(&self, query: &str) -> Result<Value> {
        match self {
            Self::Graphql(graphql) => post_graphql(graphql, query).await,
            Self::Local(node) => {
                let response = node.execute(query).await;
                if response.has_errors() {
                    anyhow::bail!("graphql returned errors: {:?}", response.errors);
                }
                Ok(json!({
                    "data": response.data.unwrap_or(Value::Null),
                }))
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct ExistingDocumentRef {
    pub doc_id: String,
    pub deleted: bool,
}

/// Derive the REST API base URL from a GraphQL endpoint.
///
/// The GraphQL endpoint is expected to end with `/graphql` (e.g.
/// `http://host:port/api/v0/graphql`). Stripping that suffix gives the API
/// base `http://host:port/api/v0`, from which paths like `/tx` (begin) and
/// `/tx/{id}` (commit/discard) are appended.
pub(crate) fn graphql_api_base(graphql: &str) -> Result<String> {
    graphql
        .trim()
        .strip_suffix("/graphql")
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            anyhow::anyhow!("expected GraphQL endpoint ending in /graphql, got {graphql}")
        })
}

fn is_probably_local_graphql_endpoint(graphql: &str) -> bool {
    let graphql = graphql.trim();
    graphql.contains("127.0.0.1") || graphql.contains("localhost")
}

/// Operator guidance appended to HTTP GraphQL errors. CLI-flavored on
/// purpose; non-CLI constructors of `Graphql` access (desktop
/// `request_timeline`) strip these hint lines before surfacing the error.
/// Never reaches agent-facing tool errors (the runtime always writes
/// through the embedded node).
pub(crate) fn graphql_diagnostic_hint(graphql: &str) -> String {
    if is_probably_local_graphql_endpoint(graphql) {
        "Next:\n  1. If this home is not initialized, run `defra-agent init`\n  2. Start the runtime with `defra-agent server`\n  3. Inspect it with `defra-agent status`".to_string()
    } else {
        format!(
            "Next:\n  1. Verify the GraphQL endpoint {graphql}\n  2. Retry with `--graphql {graphql}` or point the command at the correct runtime"
        )
    }
}

async fn post_graphql(graphql: &str, query: &str) -> Result<Value> {
    execute_graphql_async(
        graphql,
        query,
        GraphqlRequestOptions {
            timeout: std::time::Duration::from_secs(30),
            max_attempts: 5,
            retry_backoff: std::time::Duration::from_millis(100),
        },
    )
    .await
    .map_err(|error| anyhow::anyhow!("{error}\n{}", graphql_diagnostic_hint(graphql)))
}
