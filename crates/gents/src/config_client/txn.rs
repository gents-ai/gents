//! Open-write transaction wrapper around [`ConfigAccess`].
//!
//! `ConfigApplyTxn` is the only access type passed through the apply pipeline
//! once `config apply` has begun a transaction. The top-level orchestrator
//! drives `begin_apply_txn` → `apply_desired_state_changes` → `commit` (on
//! success) or `discard` (on error). The runtime self-config tools drive the
//! same shape per patch via [`ConfigApplyTxn::begin_local`], with the agent
//! DID attached so DefraDB ACP checks every statement.
//!
//! Discard semantics differ between backends:
//! - **Embedded.** `runner.rollback_txn` returns `TransactionError` only in
//!   pathological cases (handle already finalized, lock poisoned). The
//!   underlying `db_txn` is dropped in any case.
//! - **HTTP.** `DELETE /api/v0/tx/{id}` is a network call; it can fail for
//!   reasons unrelated to the apply error. Even if the DELETE never reaches
//!   the server, transaction **atomicity** guarantees the apply has no
//!   committed effect: a transaction that never sees a `commit` yields no
//!   externally-visible mutations. DefraDB's configured stale-transaction
//!   cleanup owns any orphaned server handle.
//!
//! A commit attempt is different: both DefraDB's embedded registry and its
//! HTTP commit handler remove the handle and take the underlying transaction
//! before durable finalization. A returned commit error therefore already
//! consumes/releases that transaction, so callers must not attempt a second
//! discard. A transport failure remains commit-ambiguous and must not be
//! converted into a retry; callers may only recover by observing an immutable
//! logical key outside the finalized transaction.
//!
//! Discard returns `Result<()>` so callers can log cleanup discrepancies while
//! preserving the original apply error. Commit errors are surfaced or resolved
//! by exact post-commit observation; they are never blindly retried.

use anyhow::{Context, Result};
use defra_node::{EmbeddedNode, QueryRequest};
use gents_protocol::graphql::GraphqlRequestOptions;
use identity::Did;
use query::TransactionHandle;
use serde_json::{json, Value};

use super::{graphql_api_base, graphql_diagnostic_hint, AuthenticatedGraphql, ConfigAccess};

enum TxnBackend<'a> {
    /// Numeric txn id parsed from `POST /api/v0/tx`. The same authenticated
    /// client attaches its identity bearer to begin, every statement, commit,
    /// and discard, so the transaction never falls back to an anonymous actor.
    Graphql {
        access: &'a AuthenticatedGraphql,
        id: String,
    },
    /// Embedded transaction handle returned by `runner.begin_txn(false)`.
    /// When `identity` is set, every statement carries it as the DefraDB
    /// document-ACP actor.
    Local {
        node: &'a EmbeddedNode,
        handle: TransactionHandle,
        identity: Option<Did>,
    },
}

pub struct ConfigApplyTxn<'a> {
    backend: TxnBackend<'a>,
}

fn retryable_read_options() -> GraphqlRequestOptions {
    GraphqlRequestOptions {
        timeout: std::time::Duration::from_secs(30),
        max_attempts: 5,
        retry_backoff: std::time::Duration::from_millis(100),
    }
}

fn non_idempotent_write_options() -> GraphqlRequestOptions {
    GraphqlRequestOptions {
        timeout: std::time::Duration::from_secs(30),
        max_attempts: 1,
        retry_backoff: std::time::Duration::ZERO,
    }
}

impl<'a> ConfigApplyTxn<'a> {
    /// Begin an embedded-node transaction, optionally executing under a
    /// specific DID identity so document ACP applies to every statement.
    ///
    /// This is the runtime self-config entry point; the CLI apply path goes
    /// through [`ConfigAccess::begin_apply_txn`] with its HTTP identity bearer.
    pub async fn begin_local(node: &'a EmbeddedNode, identity: Option<Did>) -> Result<Self> {
        let handle = node
            .runner()
            .begin_txn(false)
            .await
            .map_err(|error| anyhow::anyhow!("begin_txn: {error}"))?;
        Ok(Self {
            backend: TxnBackend::Local {
                node,
                handle,
                identity,
            },
        })
    }

    /// Execute a GraphQL query within this transaction.
    pub async fn execute(&self, query: &str) -> Result<Value> {
        self.execute_with_options(query, retryable_read_options())
            .await
    }

    /// Execute a non-idempotent statement exactly once within this
    /// transaction. A lost mutation response must not transparently create a
    /// second immutable document; callers resolve commit ambiguity by reading
    /// the logical key after the transaction has finalized.
    pub async fn execute_once(&self, query: &str) -> Result<Value> {
        self.execute_with_options(query, non_idempotent_write_options())
            .await
    }

    async fn execute_with_options(
        &self,
        query: &str,
        options: GraphqlRequestOptions,
    ) -> Result<Value> {
        match &self.backend {
            TxnBackend::Graphql { access, id } => access
                .execute_with_tx(query, options, id)
                .await
                .map_err(|error| {
                    anyhow::anyhow!("{error}\n{}", graphql_diagnostic_hint(access.endpoint()))
                }),
            TxnBackend::Local {
                node,
                handle,
                identity,
            } => {
                let request = QueryRequest::new(query).with_identity(identity.clone());
                let response = node.execute_request_in_txn(request, handle).await;
                if response.has_errors() {
                    anyhow::bail!("graphql returned errors: {:?}", response.errors);
                }
                Ok(json!({ "data": response.data.unwrap_or(Value::Null) }))
            }
        }
    }

    /// Commit the transaction. Apply is durable after this returns `Ok`.
    ///
    /// This consumes `self` deliberately. DefraDB removes the transaction
    /// handle before attempting durable finalization, including on explicit
    /// conflicts. Consequently an `Err` is already finalized from the client
    /// perspective and cannot be followed by `discard`.
    pub async fn commit(self) -> Result<()> {
        match self.backend {
            TxnBackend::Graphql { access, id } => {
                let endpoint = access.endpoint();
                let api_base = graphql_api_base(endpoint)?;
                let status;
                let bytes;
                {
                    let response = access
                        .post(format!("{api_base}/tx/{id}"))
                        .await
                        .with_context(|| format!("posting tx commit to {endpoint}"))?;
                    status = response.status();
                    bytes = response
                        .bytes()
                        .await
                        .with_context(|| format!("reading tx commit body from {endpoint}"))?;
                }
                if !status.is_success() {
                    anyhow::bail!(
                        "tx commit returned HTTP {status} from {endpoint}: {}",
                        String::from_utf8_lossy(&bytes)
                    );
                }
                Ok(())
            }
            TxnBackend::Local { node, handle, .. } => node
                .runner()
                .commit_txn(&handle)
                .await
                .map_err(|error| anyhow::anyhow!("commit_txn: {error}")),
        }
    }

    /// Discard the transaction. Returns the underlying error if the explicit
    /// round-trip fails; callers are expected to log and swallow that error so
    /// the original apply error remains what surfaces to the operator.
    pub async fn discard(self) -> Result<()> {
        match self.backend {
            TxnBackend::Graphql { access, id } => {
                let endpoint = access.endpoint();
                let api_base = graphql_api_base(endpoint)?;
                let status;
                let bytes;
                {
                    let response = access
                        .delete(format!("{api_base}/tx/{id}"))
                        .await
                        .with_context(|| format!("posting tx discard to {endpoint}"))?;
                    status = response.status();
                    bytes = response
                        .bytes()
                        .await
                        .with_context(|| format!("reading tx discard body from {endpoint}"))?;
                }
                if !status.is_success() {
                    anyhow::bail!(
                        "tx discard returned HTTP {status} from {endpoint}: {}",
                        String::from_utf8_lossy(&bytes)
                    );
                }
                Ok(())
            }
            TxnBackend::Local { node, handle, .. } => node
                .runner()
                .rollback_txn(&handle)
                .await
                .map_err(|error| anyhow::anyhow!("rollback_txn: {error}")),
        }
    }
}

impl ConfigAccess {
    /// Begin a write transaction on the underlying backend.
    pub async fn begin_apply_txn(&self) -> Result<ConfigApplyTxn<'_>> {
        match self {
            ConfigAccess::Graphql(endpoint) => {
                let api_base = graphql_api_base(endpoint.endpoint())?;
                let response = endpoint
                    .post(format!("{api_base}/tx"))
                    .await
                    .with_context(|| format!("posting tx begin to {}", endpoint.endpoint()))?;
                let status = response.status();
                let bytes = response.bytes().await.with_context(|| {
                    format!("reading tx begin body from {}", endpoint.endpoint())
                })?;
                if !status.is_success() {
                    anyhow::bail!(
                        "tx begin returned HTTP {status} from {}: {}",
                        endpoint.endpoint(),
                        String::from_utf8_lossy(&bytes)
                    );
                }
                let body: Value = serde_json::from_slice(&bytes).with_context(|| {
                    format!("decoding tx begin body from {}", endpoint.endpoint())
                })?;
                let id = decode_txn_id(&body)?;
                Ok(ConfigApplyTxn {
                    backend: TxnBackend::Graphql {
                        access: endpoint,
                        id,
                    },
                })
            }
            ConfigAccess::Local(node) => {
                let handle = node
                    .runner()
                    .begin_txn(false)
                    .await
                    .map_err(|error| anyhow::anyhow!("begin_txn: {error}"))?;
                Ok(ConfigApplyTxn {
                    backend: TxnBackend::Local {
                        node,
                        handle,
                        identity: None,
                    },
                })
            }
        }
    }
}

/// Decode DefraDB's numeric transaction identifier and canonicalize it before
/// it is interpolated into either a URL path or the transaction header.
fn decode_txn_id(body: &Value) -> Result<String> {
    let value = body
        .get("id")
        .ok_or_else(|| anyhow::anyhow!("tx begin missing id: {body}"))?;
    let id = if let Some(id) = value.as_u64() {
        id
    } else if let Some(id) = value.as_str() {
        id.parse::<u64>()
            .with_context(|| format!("tx begin returned non-numeric id {id:?}"))?
    } else {
        anyhow::bail!("tx begin returned non-numeric id: {body}");
    };
    Ok(id.to_string())
}

#[cfg(test)]
mod tests {
    use crate::identity::{AgentIdentity, KeyIdentity};

    use super::*;

    #[test]
    fn transaction_id_is_numeric_and_canonical_before_transport() {
        assert_eq!(decode_txn_id(&json!({"id": 42})).unwrap(), "42");
        assert_eq!(decode_txn_id(&json!({"id": "0042"})).unwrap(), "42");

        for body in [
            json!({"id": "42/commit"}),
            json!({"id": "42?redirect=https://attacker.invalid"}),
            json!({"id": -1}),
            json!({"id": null}),
            json!({}),
        ] {
            assert!(decode_txn_id(&body).is_err(), "accepted {body}");
        }
    }

    #[test]
    fn non_idempotent_transaction_writes_are_never_retried() {
        let options = non_idempotent_write_options();
        assert_eq!(options.max_attempts, 1);
        assert_eq!(options.retry_backoff, std::time::Duration::ZERO);
    }

    #[tokio::test]
    async fn local_transaction_uses_the_configured_node_signer() {
        let tempdir = tempfile::tempdir().unwrap();
        let identity = KeyIdentity::load_or_create(tempdir.path().join("node.key"), None).unwrap();
        let did = identity.did().to_string();
        let expected_signer = defra_core::signing::get_identity(&did)
            .expect("KeyIdentity registers a DefraDB signer")
            .public_key_hex
            .clone();
        let node = EmbeddedNode::builder()
            .with_node_identity_did(&did)
            .build()
            .await
            .unwrap();
        node.add_schema("type Widget { name: String }")
            .await
            .unwrap();

        let txn = ConfigApplyTxn::begin_local(&node, None).await.unwrap();
        let created = txn
            .execute(r#"mutation { create_Widget(input: {name: "signed"}) { _docID } }"#)
            .await
            .unwrap();
        let doc_id = created
            .pointer("/data/add_Widget/0/_docID")
            .or_else(|| created.pointer("/data/create_Widget/0/_docID"))
            .and_then(Value::as_str)
            .expect("created document id")
            .to_string();
        txn.commit().await.unwrap();

        let response = node
            .execute(&format!(
                r#"query {{ _commits(docID: "{doc_id}", filter: {{fieldName: {{_eq: "_C"}}}}) {{ signature {{ type identity }} }} }}"#
            ))
            .await;
        assert!(response.errors.is_empty(), "{:?}", response.errors);
        let commits = response
            .data
            .as_ref()
            .and_then(|data| data.get("_commits"))
            .and_then(Value::as_array)
            .expect("composite commit rows");
        assert!(commits.iter().any(|commit| {
            commit
                .pointer("/signature/identity")
                .and_then(Value::as_str)
                == Some(expected_signer.as_str())
        }));

        node.shutdown().await;
    }

    #[tokio::test]
    async fn failed_local_commit_releases_transaction_and_staged_writes() {
        let tempdir = tempfile::tempdir().unwrap();
        let identity = KeyIdentity::load_or_create(tempdir.path().join("node.key"), None).unwrap();
        let did = identity.did().to_string();
        let node = EmbeddedNode::builder()
            .with_node_identity_did(&did)
            .build()
            .await
            .unwrap();
        node.add_schema("type CleanupWidget { name: String }")
            .await
            .unwrap();

        let created = node
            .execute(r#"mutation { create_CleanupWidget(input: {name: "initial"}) { _docID } }"#)
            .await;
        assert!(created.errors.is_empty(), "{:?}", created.errors);
        let doc_id = created
            .data
            .as_ref()
            .and_then(|data| {
                data.pointer("/create_CleanupWidget/0/_docID")
                    .or_else(|| data.pointer("/add_CleanupWidget/0/_docID"))
            })
            .and_then(Value::as_str)
            .expect("created cleanup widget document id")
            .to_string();
        let escaped_doc_id = crate::graphql::escape_graphql_string(&doc_id);

        let txn = ConfigApplyTxn::begin_local(&node, None).await.unwrap();
        txn.execute(&format!(
            r#"{{ CleanupWidget(filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }}) {{ name }} }}"#
        ))
        .await
        .unwrap();
        txn.execute_once(&format!(
            r#"mutation {{ update_CleanupWidget(filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }}, input: {{ name: "staged" }}) {{ _docID }} }}"#
        ))
        .await
        .unwrap();

        let concurrent = node
            .execute(&format!(
                r#"mutation {{ update_CleanupWidget(filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }}, input: {{ name: "concurrent" }}) {{ _docID }} }}"#
            ))
            .await;
        assert!(concurrent.errors.is_empty(), "{:?}", concurrent.errors);

        let conflict = txn
            .commit()
            .await
            .expect_err("stale transaction must fail its commit");
        assert!(
            conflict
                .to_string()
                .to_ascii_lowercase()
                .contains("conflict"),
            "unexpected commit failure: {conflict:#}"
        );

        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            let replacement = ConfigApplyTxn::begin_local(&node, None).await?;
            replacement
                .execute_once(&format!(
                    r#"mutation {{ update_CleanupWidget(filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }}, input: {{ name: "replacement" }}) {{ _docID }} }}"#
                ))
                .await?;
            replacement.commit().await
        })
        .await
        .expect("failed commit must not leave locks or an active transaction")
        .expect("replacement transaction should commit");

        let observed = node
            .execute(&format!(
                r#"{{ CleanupWidget(filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }}) {{ name }} }}"#
            ))
            .await;
        assert!(observed.errors.is_empty(), "{:?}", observed.errors);
        assert_eq!(
            observed
                .data
                .as_ref()
                .and_then(|data| data.pointer("/CleanupWidget/0/name"))
                .and_then(Value::as_str),
            Some("replacement"),
            "the failed transaction's staged value must never become visible"
        );

        node.shutdown().await;
    }
}
