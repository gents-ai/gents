//! Open-write transaction wrapper around `ConfigAccess`.
//!
//! `ConfigApplyTxn` is the only access type passed through the apply pipeline
//! once `config apply` has begun a transaction. The top-level orchestrator
//! drives `begin_apply_txn` → `apply_desired_state_changes` → `commit` (on
//! success) or `discard` (on error).
//!
//! Discard semantics differ between backends:
//! - **Embedded.** `runner.rollback_txn` returns `TransactionError` only in
//!   pathological cases (handle already finalized, lock poisoned). The
//!   underlying `db_txn` is dropped in any case.
//! - **HTTP.** `DELETE /api/v0/tx/{id}` is a network call; it can fail for
//!   reasons unrelated to the apply error. Even if the DELETE never reaches
//!   the server, DefraDB's tx GC will reclaim the handle on its own.
//!
//! Both return `Result<()>` so callers can log discrepancies, but neither
//! changes operator-facing behavior on failure: the apply error is what
//! surfaces, and the DB ends at the pre-apply snapshot.

use anyhow::{Context, Result};
use defra_agent::defra_node::QueryRequest;
use defra_agent_protocol::graphql::{execute_graphql_async_with_tx, GraphqlRequestOptions};
use query::TransactionHandle;
use serde_json::{json, Value};

use crate::config_writes::ConfigAccess;
use crate::graphql_access::graphql_diagnostic_hint;

#[derive(Debug)]
pub(crate) enum TxnHandle {
    /// Numeric txn id parsed from `POST /api/v0/tx/begin`.
    Graphql(String),
    /// Embedded transaction handle returned by `runner.begin_txn(false)`.
    Local(TransactionHandle),
}

pub(crate) struct ConfigApplyTxn<'a> {
    access: &'a ConfigAccess,
    handle: TxnHandle,
}

impl<'a> ConfigApplyTxn<'a> {
    pub(crate) fn new(access: &'a ConfigAccess, handle: TxnHandle) -> Self {
        Self { access, handle }
    }

    pub(crate) fn mode(&self) -> &'static str {
        self.access.mode()
    }

    /// Execute a GraphQL query within this transaction.
    pub(crate) async fn execute(&self, query: &str) -> Result<Value> {
        match (&self.access, &self.handle) {
            (ConfigAccess::Graphql(endpoint), TxnHandle::Graphql(id)) => {
                execute_graphql_async_with_tx(
                    endpoint,
                    query,
                    GraphqlRequestOptions {
                        timeout: std::time::Duration::from_secs(30),
                        max_attempts: 5,
                        retry_backoff: std::time::Duration::from_millis(100),
                    },
                    Some(id),
                )
                .await
                .map_err(|error| anyhow::anyhow!("{error}\n{}", graphql_diagnostic_hint(endpoint)))
            }
            (ConfigAccess::Local(node), TxnHandle::Local(handle)) => {
                let request = QueryRequest::new(query);
                let response = node.runner().execute_in_txn(request, handle).await;
                if response.has_errors() {
                    anyhow::bail!("graphql returned errors: {:?}", response.errors);
                }
                Ok(json!({ "data": response.data.unwrap_or(Value::Null) }))
            }
            _ => anyhow::bail!("ConfigApplyTxn backend/handle mismatch (internal bug)"),
        }
    }

    /// Commit the transaction. Apply is durable after this returns Ok.
    pub(crate) async fn commit(self) -> Result<()> {
        match (self.access, self.handle) {
            (ConfigAccess::Graphql(endpoint), TxnHandle::Graphql(id)) => {
                let client = reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(30))
                    .build()?;
                let response = client
                    .post(format!("{endpoint}api/v0/tx/{id}"))
                    .send()
                    .await
                    .with_context(|| format!("posting tx commit to {endpoint}"))?;
                if !response.status().is_success() {
                    anyhow::bail!(
                        "tx commit returned HTTP {} from {endpoint}",
                        response.status()
                    );
                }
                Ok(())
            }
            (ConfigAccess::Local(node), TxnHandle::Local(handle)) => node
                .runner()
                .commit_txn(&handle)
                .await
                .map_err(|error| anyhow::anyhow!("commit_txn: {error}")),
            _ => anyhow::bail!("ConfigApplyTxn backend/handle mismatch on commit (internal bug)"),
        }
    }

    /// Discard the transaction. Returns the underlying error if the explicit
    /// round-trip fails; callers are expected to log and swallow that error so
    /// the original apply error remains what surfaces to the operator.
    pub(crate) async fn discard(self) -> Result<()> {
        match (self.access, self.handle) {
            (ConfigAccess::Graphql(endpoint), TxnHandle::Graphql(id)) => {
                let client = reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(30))
                    .build()?;
                let response = client
                    .delete(format!("{endpoint}api/v0/tx/{id}"))
                    .send()
                    .await
                    .with_context(|| format!("posting tx discard to {endpoint}"))?;
                if !response.status().is_success() {
                    anyhow::bail!(
                        "tx discard returned HTTP {} from {endpoint}",
                        response.status()
                    );
                }
                Ok(())
            }
            (ConfigAccess::Local(node), TxnHandle::Local(handle)) => node
                .runner()
                .rollback_txn(&handle)
                .await
                .map_err(|error| anyhow::anyhow!("rollback_txn: {error}")),
            _ => anyhow::bail!("ConfigApplyTxn backend/handle mismatch on discard (internal bug)"),
        }
    }
}

impl ConfigAccess {
    /// Begin a write transaction on the underlying backend.
    pub(crate) async fn begin_apply_txn(&self) -> Result<ConfigApplyTxn<'_>> {
        match self {
            ConfigAccess::Graphql(endpoint) => {
                let client = reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(30))
                    .build()?;
                let response = client
                    .post(format!("{endpoint}api/v0/tx/begin"))
                    .send()
                    .await
                    .with_context(|| format!("posting tx begin to {endpoint}"))?;
                if !response.status().is_success() {
                    anyhow::bail!(
                        "tx begin returned HTTP {} from {endpoint}",
                        response.status()
                    );
                }
                let body: Value = response
                    .json()
                    .await
                    .with_context(|| format!("decoding tx begin body from {endpoint}"))?;
                let id = body
                    .get("id")
                    .and_then(Value::as_str)
                    .ok_or_else(|| anyhow::anyhow!("tx begin missing id: {body}"))?
                    .to_string();
                Ok(ConfigApplyTxn::new(self, TxnHandle::Graphql(id)))
            }
            ConfigAccess::Local(node) => {
                let handle = node
                    .runner()
                    .begin_txn(false)
                    .await
                    .map_err(|error| anyhow::anyhow!("begin_txn: {error}"))?;
                Ok(ConfigApplyTxn::new(self, TxnHandle::Local(handle)))
            }
        }
    }
}
