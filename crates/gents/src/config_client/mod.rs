//! Typed per-collection control-plane write client (#654).
//!
//! One proven write path for config collections, shared by two consumers:
//! the CLI `config apply`/imperative commands (which historically owned these
//! writers under `gents-cli/src/config_writes`) and the runtime
//! self-configuration tools (`crate::self_config`).
//!
//! The client is DID-parameterized: [`ConfigApplyTxn::begin_local`] accepts an
//! optional `identity::Did`, and every statement executed inside that
//! transaction carries it as the DefraDB ACP actor — authorization is
//! enforced at the node, not by app-level ownership checks. Embedded CLI paths
//! default to the node DID and signer. HTTP paths require bearer
//! authentication for a caller ACP identity; without it, the server still
//! signs committed mutations as the node but evaluates the query anonymously.
//!
//! Write conventions (load-bearing — see `AGENTS.md`):
//! - every interpolated value goes through
//!   [`crate::graphql::escape_graphql_string`];
//! - list fields never render `[]` (typed as `JsonArray`, corrupts nillable
//!   array columns) — the shared field encoders emit `null` instead;
//! - `Option` fields omitted from an `update:` clause preserve the stored
//!   value; explicit clearing requires `field: null`.

mod agent_behavior;
mod approval;
mod common;
mod desired_state;
mod event_trigger;
mod inference_backend;
mod inference_profile;
mod schedule;
mod schema_contract;
mod task;
mod txn;

pub mod patch;

pub(crate) use agent_behavior::load_agent_behavior_in_txn;
pub use agent_behavior::write_agent_behavior_document;
pub use approval::{list_held_tool_calls, write_tool_approval, HeldToolCall, ToolApprovalVerdict};
pub use common::{mint_recreate_identity, mint_recreate_identity_timestamp};
pub use desired_state::{
    apply_desired_state_plan, DesiredStateApplyCounts, DesiredStateApplyDocument,
    DesiredStateApplyPlan,
};
pub(crate) use desired_state::{
    desired_state_document_digest, read_desired_state_document_in_txn,
    verify_existing_desired_state_plan,
};
pub use event_trigger::write_event_trigger_document;
pub use inference_backend::{
    load_inference_backend_in_txn, write_inference_backend_document, InferenceBackendUpsertDocument,
};
pub(crate) use inference_profile::effective_inference_profile;
pub use inference_profile::write_inference_profile_document;
pub use schedule::write_schedule_document;
pub(crate) use schema_contract::collection_schema_contract_digest;
pub use task::write_task_document;
pub(crate) use tool_selection::effective_tool_selection;
pub use tool_selection::{
    write_tool_selection_document, write_tool_selection_document_with_clear_fields,
};
pub use txn::ConfigApplyTxn;

mod tool_selection;

use std::collections::BTreeSet;
use std::sync::Arc;

use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use gents_protocol::graphql::{execute_graphql_async, GraphqlRequestOptions};
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
                let response =
                    crate::graphql::graphql_with_transaction_retry(node, query, "config GraphQL")
                        .await?;
                Ok(json!({
                    "data": response.data.unwrap_or(Value::Null),
                }))
            }
        }
    }

    pub async fn execute_mutation(&self, mutation: &str, operation: &str) -> Result<Value> {
        match self {
            Self::Graphql(graphql) => post_graphql(graphql, mutation).await,
            Self::Local(node) => {
                let response = crate::graphql::graphql_mutation_with_transaction_retry(
                    node, mutation, operation,
                )
                .await?;
                Ok(json!({
                    "data": response.data.unwrap_or(Value::Null),
                }))
            }
        }
    }

    /// Apply schema SDL through the same local-or-HTTP control-plane seam as
    /// configuration writes. Callers must still perform their own structural
    /// compatibility check before invoking this operation.
    pub async fn add_schema(&self, sdl: &str) -> Result<()> {
        match self {
            Self::Graphql(graphql) => {
                let api_base = graphql_api_base(graphql)?;
                let url = format!("{api_base}/schema");
                let client = reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(30))
                    .build()?;
                let response = client
                    .post(&url)
                    .header(reqwest::header::CONTENT_TYPE, "text/plain; charset=utf-8")
                    .body(sdl.to_owned())
                    .send()
                    .await
                    .with_context(|| format!("posting schema SDL to {url}"))?;
                let status = response.status();
                let bytes = response
                    .bytes()
                    .await
                    .with_context(|| format!("reading schema SDL response from {url}"))?;
                if !status.is_success() {
                    anyhow::bail!(
                        "schema SDL request to {url} failed with {status}: {}",
                        String::from_utf8_lossy(&bytes)
                    );
                }
                Ok(())
            }
            Self::Local(node) => node.add_schema(sdl).await.context("adding schema SDL"),
        }
    }

    /// Return the declared field shape for one collection. HTTP callers use
    /// DefraDB's read-only collection-version API, which EmbeddedNode exposes
    /// without enabling destructive collection management.
    pub async fn collection_fields(&self, collection: &str) -> Result<Option<BTreeSet<String>>> {
        Ok(self.collection_version(collection).await?.map(|version| {
            version
                .get("Fields")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(|field| field.get("Name").and_then(Value::as_str))
                .map(ToOwned::to_owned)
                .collect()
        }))
    }

    /// Return the active DefraDB collection version so callers that own a
    /// schema contract can compare field kinds, directives, and indexes—not
    /// merely the set of field names.
    pub async fn collection_version(&self, collection: &str) -> Result<Option<Value>> {
        crate::graphql::validate_collection_identifier(collection)?;
        match self {
            Self::Local(node) => node
                .get_collection(collection)?
                .map(serde_json::to_value)
                .transpose()
                .context("serializing active collection version"),
            Self::Graphql(graphql) => {
                let api_base = graphql_api_base(graphql)?;
                let url = format!("{api_base}/collections/versions");
                let versions: Value = reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(30))
                    .build()?
                    .get(&url)
                    .send()
                    .await
                    .with_context(|| format!("fetching collection versions from {url}"))?
                    .error_for_status()
                    .with_context(|| format!("fetching collection versions from {url}"))?
                    .json()
                    .await
                    .with_context(|| format!("decoding collection versions from {url}"))?;
                Ok(versions.as_array().and_then(|versions| {
                    versions
                        .iter()
                        .find(|version| {
                            version.get("Name").and_then(Value::as_str) == Some(collection)
                                && version
                                    .get("IsActive")
                                    .and_then(Value::as_bool)
                                    .unwrap_or(true)
                        })
                        .cloned()
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
pub fn graphql_api_base(graphql: &str) -> Result<String> {
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
pub fn graphql_diagnostic_hint(graphql: &str) -> String {
    if is_probably_local_graphql_endpoint(graphql) {
        "Next:\n  1. If this home is not initialized, run `gents init`\n  2. Start the runtime with `gents server`\n  3. Inspect it with `gents status`".to_string()
    } else {
        format!(
            "Next:\n  1. Verify the GraphQL endpoint {graphql}\n  2. Retry with `--graphql {graphql}` or point the command at the correct runtime"
        )
    }
}

pub async fn post_graphql(graphql: &str, query: &str) -> Result<Value> {
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
