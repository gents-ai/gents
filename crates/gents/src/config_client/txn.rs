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
use gents_protocol::graphql::{execute_graphql_async_with_tx, GraphqlRequestOptions};
use identity::Did;
use query::TransactionHandle;
use serde_json::{json, Value};

use super::{graphql_api_base, graphql_diagnostic_hint, ConfigAccess};

enum TxnBackend<'a> {
    /// Numeric txn id parsed from `POST /api/v0/tx`. Identity cannot ride
    /// this path (`QueryRequest.identity` is `#[serde(skip)]`): without an
    /// authenticated HTTP bearer the ACP actor is anonymous, while committed
    /// mutations are still signed by the server node.
    Graphql {
        endpoint: &'a str,
        id: String,
        http_client: reqwest::Client,
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
    rollback_on_drop: Option<LocalRollback>,
}

/// The registry owns open transactions independently of their handles. Keep
/// cleanup armed across awaits, including a cancelled commit/discard future.
/// Remove this shim after upstream adoption: https://github.com/gents-ai/gents/issues/1372.
struct LocalRollback {
    runner: std::sync::Arc<dyn query::QueryExecutor>,
    handle: Option<TransactionHandle>,
    write_guard: Option<tokio::sync::OwnedMutexGuard<()>>,
}

impl Drop for LocalRollback {
    fn drop(&mut self) {
        let Some(handle) = self.handle.take() else {
            return;
        };
        let Ok(runtime) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let runner = self.runner.clone();
        let write_guard = self.write_guard.take();
        runtime.spawn(async move {
            let _write_guard = write_guard;
            if let Err(error) = runner.rollback_txn(&handle).await {
                tracing::debug!(%error, transaction = %handle, "dropped transaction was already finalized or rollback failed");
            }
        });
    }
}

impl<'a> ConfigApplyTxn<'a> {
    /// Begin an embedded-node transaction, optionally executing under a
    /// specific DID identity so document ACP applies to every statement.
    /// When omitted, DefraDB supplies the embedded node DID and signer.
    ///
    /// This is the runtime self-config entry point; the CLI apply path goes
    /// through [`ConfigAccess::begin_apply_txn`]. Embedded access defaults to
    /// the node DID; HTTP access needs bearer authentication for a caller ACP
    /// identity even though the server node signs its commits.
    pub async fn begin_local(node: &'a EmbeddedNode, identity: Option<Did>) -> Result<Self> {
        let write_guard = crate::graphql::mutation_write_gate(node).lock_owned().await;
        let runner = node.runner().clone();
        // Native begin registers an independently owned transaction. Let it
        // finish even if this caller is cancelled, so the returned rollback
        // guard cleans up the handle before releasing the shared write gate.
        let rollback = tokio::spawn(async move {
            let mut rollback = LocalRollback {
                runner,
                handle: None,
                write_guard: Some(write_guard),
            };
            let handle = rollback
                .runner
                .begin_txn(false)
                .await
                .map_err(|error| anyhow::anyhow!("begin_txn: {error}"))?;
            rollback.handle = Some(handle);
            Ok::<_, anyhow::Error>(rollback)
        })
        .await
        .context("begin embedded transaction task")??;
        let handle = rollback
            .handle
            .as_ref()
            .expect("successful begin has a handle")
            .clone();
        Ok(Self {
            rollback_on_drop: Some(rollback),
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
            TxnBackend::Graphql {
                endpoint,
                id,
                http_client: _,
            } => execute_graphql_async_with_tx(
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
            .map_err(|error| anyhow::anyhow!("{error}\n{}", graphql_diagnostic_hint(endpoint))),
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

    /// Return the active collection version through the same local-or-HTTP
    /// backend as this transaction. DefraDB schema registration is additive
    /// and lives outside document transactions, so this is the authoritative
    /// schema-readiness view for transactional package checks.
    pub async fn collection_version(&self, collection: &str) -> Result<Option<Value>> {
        crate::graphql::validate_collection_identifier(collection)?;
        match &self.backend {
            TxnBackend::Local { node, .. } => node
                .get_collection(collection)?
                .map(serde_json::to_value)
                .transpose()
                .context("serializing active collection version"),
            TxnBackend::Graphql {
                endpoint,
                http_client,
                ..
            } => {
                let api_base = graphql_api_base(endpoint)?;
                let url = format!("{api_base}/collections/versions");
                let versions: Value = http_client
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

    /// Execute against an embedded transaction while preserving the native
    /// response envelope, including composite-version metadata. Runtime
    /// lifecycle transitions need that exact commit identity; converting to
    /// JSON would discard the typed version join used by rendered-request
    /// capture. HTTP config-apply callers intentionally cannot use this seam.
    pub(crate) async fn execute_local_response(
        &self,
        query: &str,
    ) -> Result<defra_node::QueryResponse> {
        let TxnBackend::Local {
            node,
            handle,
            identity,
        } = &self.backend
        else {
            anyhow::bail!("native transaction responses require embedded access");
        };
        let request = QueryRequest::new(query).with_identity(identity.clone());
        let response = node.execute_request_in_txn(request, handle).await;
        if response.has_errors() {
            anyhow::bail!("graphql returned errors: {:?}", response.errors);
        }
        Ok(response)
    }

    /// Commit the transaction. Apply is durable after this returns Ok.
    pub async fn commit(mut self) -> Result<()> {
        let result = match self.backend {
            TxnBackend::Graphql {
                endpoint,
                id,
                http_client,
            } => {
                let api_base = graphql_api_base(endpoint)?;
                let status;
                let bytes;
                {
                    let response = http_client
                        .post(format!("{api_base}/tx/{id}"))
                        .send()
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
        };
        if result.is_ok() {
            if let Some(guard) = self.rollback_on_drop.as_mut() {
                guard.handle = None;
            }
        }
        result
    }

    /// Discard the transaction. Returns the underlying error if the explicit
    /// round-trip fails; callers are expected to log and swallow that error so
    /// the original apply error remains what surfaces to the operator.
    pub async fn discard(mut self) -> Result<()> {
        let result = match self.backend {
            TxnBackend::Graphql {
                endpoint,
                id,
                http_client,
            } => {
                let api_base = graphql_api_base(endpoint)?;
                let status;
                let bytes;
                {
                    let response = http_client
                        .delete(format!("{api_base}/tx/{id}"))
                        .send()
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
        };
        if result.is_ok() {
            if let Some(guard) = self.rollback_on_drop.as_mut() {
                guard.handle = None;
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::commit_signer_identity_for_did;
    use crate::{AgentIdentity, KeyIdentity};

    struct AfterFinalization<'a> {
        entered: std::sync::atomic::AtomicBool,
        node: &'a EmbeddedNode,
        handle: TransactionHandle,
    }

    impl crate::graphql::GraphqlExecution for AfterFinalization<'_> {
        async fn execute(
            &self,
            graphql: &str,
            policy: defra_node::ExecuteRetryPolicy,
        ) -> defra_node::QueryResponse {
            self.entered
                .store(true, std::sync::atomic::Ordering::SeqCst);
            // This runs only after the ordinary mutation has acquired the
            // shared gate. Cleanup must have finished before it enters.
            let old_transaction = self
                .node
                .execute_request_in_txn(
                    QueryRequest::new("{ GatedTransactionFact { value } }"),
                    &self.handle,
                )
                .await;
            assert!(
                old_transaction.has_errors(),
                "write entered before native transaction finalized"
            );
            self.node.execute_with_retry(graphql, policy).await
        }
    }

    #[tokio::test]
    async fn explicit_transaction_excludes_ordinary_writes_until_commit_discard_or_abort() {
        use std::future::Future;
        for finish in ["commit", "discard", "abort"] {
            let node = std::sync::Arc::new(EmbeddedNode::builder().build().await.unwrap());
            node.add_schema("type GatedTransactionFact { value: String }")
                .await
                .unwrap();
            let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
            let (finish_tx, finish_rx) = tokio::sync::oneshot::channel();
            let worker_node = node.clone();
            let worker = tokio::spawn(async move {
                let txn = ConfigApplyTxn::begin_local(&worker_node, None)
                    .await
                    .unwrap();
                txn.execute(r#"mutation { create_GatedTransactionFact(input: {value: "explicit"}) { _docID } }"#).await.unwrap();
                let TxnBackend::Local { handle, .. } = &txn.backend else {
                    unreachable!()
                };
                ready_tx.send(handle.clone()).unwrap();
                finish_rx.await.unwrap();
                match finish {
                    "commit" => txn.commit().await.unwrap(),
                    "discard" => txn.discard().await.unwrap(),
                    _ => unreachable!(),
                }
            });
            let handle = ready_rx.await.unwrap();
            let executor = AfterFinalization {
                entered: std::sync::atomic::AtomicBool::new(false),
                node: &node,
                handle,
            };
            let mut ordinary_write = Box::pin(
                crate::graphql::graphql_mutation_with_transaction_retry_using(
                    &node,
                    &executor,
                    r#"mutation { create_GatedTransactionFact(input: {value: "ordinary"}) { _docID } }"#,
                    "write after explicit transaction",
                ),
            );
            // Poll the actual writer into the gate wait; no timing assumption.
            std::future::poll_fn(|cx| {
                assert!(ordinary_write.as_mut().poll(cx).is_pending());
                std::task::Poll::Ready(())
            })
            .await;
            assert!(
                !executor.entered.load(std::sync::atomic::Ordering::SeqCst),
                "ordinary executor entered while explicit transaction held the gate"
            );
            if finish == "abort" {
                worker.abort();
                assert!(worker.await.unwrap_err().is_cancelled());
            } else {
                finish_tx.send(()).unwrap();
                worker.await.unwrap();
            }
            tokio::time::timeout(std::time::Duration::from_secs(5), ordinary_write)
                .await
                .expect("write gate remained held after finalization")
                .unwrap();
            assert!(executor.entered.load(std::sync::atomic::Ordering::SeqCst));
            let response = node.execute("{ GatedTransactionFact { value } }").await;
            let rows = crate::graphql::rows::<Value>(&response, "GatedTransactionFact").unwrap();
            assert_eq!(rows.len(), if finish == "commit" { 2 } else { 1 });
            assert!(rows.iter().any(|row| row["value"] == "ordinary"));
            node.shutdown().await;
        }
    }

    #[tokio::test]
    async fn aborted_local_transaction_is_removed_from_registry_without_committing() {
        let node = std::sync::Arc::new(EmbeddedNode::builder().build().await.unwrap());
        node.add_schema("type CancelledTransactionFact { value: String }")
            .await
            .unwrap();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let worker_node = node.clone();
        let worker = tokio::spawn(async move {
            let txn = ConfigApplyTxn::begin_local(&worker_node, None)
                .await
                .unwrap();
            txn.execute(r#"mutation { create_CancelledTransactionFact(input: {value: "uncommitted"}) { _docID } }"#)
                .await.unwrap();
            let TxnBackend::Local { handle, .. } = &txn.backend else {
                unreachable!()
            };
            ready_tx.send(handle.clone()).unwrap();
            std::future::pending::<()>().await;
            txn.commit().await.unwrap();
        });
        let handle = ready_rx.await.unwrap();
        worker.abort();
        assert!(worker.await.unwrap_err().is_cancelled());
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let response = node
                    .execute_request_in_txn(
                        QueryRequest::new("{ CancelledTransactionFact { value } }"),
                        &handle,
                    )
                    .await;
                if response.has_errors() {
                    assert!(
                        response
                            .errors
                            .iter()
                            .any(|error| error.message == format!("transaction '{handle}' not found or has been committed/rolled back")),
                        "unexpected transaction query error: {:?}",
                        response.errors
                    );
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("aborted transaction remained in registry");
        let response = node.execute("{ CancelledTransactionFact { value } }").await;
        assert!(!response.has_errors());
        assert_eq!(
            response.data.unwrap()["CancelledTransactionFact"],
            json!([])
        );
        node.shutdown().await;
    }

    #[tokio::test]
    async fn local_transaction_is_signed_by_the_node_identity() {
        let dir = tempfile::tempdir().unwrap();
        let identity = KeyIdentity::load_or_create(dir.path().join("node.key"), None).unwrap();
        let did = identity.did().to_string();
        let expected_signer = commit_signer_identity_for_did(&did).unwrap();
        let node = EmbeddedNode::builder()
            .with_node_identity_did(&did)
            .build()
            .await
            .unwrap();
        node.add_schema("type SignedTransactionFact { value: String }")
            .await
            .unwrap();

        let txn = ConfigApplyTxn::begin_local(&node, None).await.unwrap();
        let response = txn
            .execute(
                r#"mutation {
                    create_SignedTransactionFact(input: {value: "durable"}) { _docID }
                }"#,
            )
            .await
            .unwrap();
        let doc_id = response["data"]["add_SignedTransactionFact"][0]["_docID"]
            .as_str()
            .expect("created document ID")
            .to_string();
        txn.commit().await.unwrap();

        let commits =
            crate::graphql::composite_commits(&node, &doc_id, "load signed explicit transaction")
                .await
                .unwrap();
        assert!(commits.iter().any(|commit| {
            commit
                .signature
                .as_ref()
                .map(|signature| signature.identity.as_str())
                == Some(expected_signer.as_str())
        }));

        node.shutdown().await;
    }
}

impl ConfigAccess {
    /// Execute and commit one statement through the authoritative path for the
    /// selected backend.
    ///
    /// Embedded nodes use `EmbeddedNode::execute_with_retry` via
    /// `ConfigAccess::execute`, which installs the node's commit-signing
    /// context. HTTP uses an explicit transaction so the server publishes the
    /// commit event consumed by a running runtime.
    pub async fn execute_committed(&self, query: &str) -> Result<Value> {
        if matches!(self, Self::Local(_)) {
            return self.execute(query).await;
        }
        let txn = self.begin_apply_txn().await?;
        match txn.execute(query).await {
            Ok(response) => {
                txn.commit().await?;
                Ok(response)
            }
            Err(error) => {
                let _ = txn.discard().await;
                Err(error)
            }
        }
    }

    /// Begin a write transaction on the underlying backend.
    pub async fn begin_apply_txn(&self) -> Result<ConfigApplyTxn<'_>> {
        match self {
            ConfigAccess::Graphql(endpoint) => {
                let api_base = graphql_api_base(endpoint)?;
                let client = reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(30))
                    .build()?;
                let response = client
                    .post(format!("{api_base}/tx"))
                    .send()
                    .await
                    .with_context(|| format!("posting tx begin to {endpoint}"))?;
                let status = response.status();
                let bytes = response
                    .bytes()
                    .await
                    .with_context(|| format!("reading tx begin body from {endpoint}"))?;
                if !status.is_success() {
                    anyhow::bail!(
                        "tx begin returned HTTP {status} from {endpoint}: {}",
                        String::from_utf8_lossy(&bytes)
                    );
                }
                let body: Value = serde_json::from_slice(&bytes)
                    .with_context(|| format!("decoding tx begin body from {endpoint}"))?;
                // DefraDB returns `{"id": uint64}` (numeric); accept both string
                // and number forms so the recording test harness can use either.
                let id = body
                    .get("id")
                    .and_then(|v| {
                        v.as_str()
                            .map(ToOwned::to_owned)
                            .or_else(|| v.as_u64().map(|n| n.to_string()))
                    })
                    .ok_or_else(|| anyhow::anyhow!("tx begin missing id: {body}"))?;
                Ok(ConfigApplyTxn {
                    rollback_on_drop: None,
                    backend: TxnBackend::Graphql {
                        endpoint,
                        id,
                        http_client: client,
                    },
                })
            }
            ConfigAccess::Local(node) => ConfigApplyTxn::begin_local(node, None).await,
        }
    }
}
