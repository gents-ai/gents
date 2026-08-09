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
//!   externally-visible mutations. The orphaned handle is bounded by
//!   DefraDB's per-request HTTP timeout (30s default), not an active idle-GC
//!   sweep.
//!
//! Both return `Result<()>` so callers can log discrepancies, but neither
//! changes operator-facing behavior on failure: the apply error is what
//! surfaces, and the DB ends at the pre-apply snapshot via atomicity.

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
        match &self.backend {
            TxnBackend::Graphql { access, id } => access
                .execute_with_tx(
                    query,
                    GraphqlRequestOptions {
                        timeout: std::time::Duration::from_secs(30),
                        max_attempts: 5,
                        retry_backoff: std::time::Duration::from_millis(100),
                    },
                    id,
                )
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

    /// Commit the transaction. Apply is durable after this returns Ok.
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
}
