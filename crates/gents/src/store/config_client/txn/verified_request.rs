use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

use defra_node::QueryRequest;
use serde_json::Value;

use super::apply_txn::TxnBackend;
#[cfg(test)]
use super::fault_injection::after_successful_mutation_for_test;
use super::native::execute_owned_native;
use super::{graphql, retry, ConfigApplyTxn, ConfigClientError, Result};
use crate::store::config_client::write_telemetry::{record_slow_native_phase, ConflictSource};

enum VerifiedGraph {
    Create(Arc<db::block::builder::VerifiedClientCreate>),
    Mutation(Arc<db::block::builder::VerifiedClientMutation>),
}

impl VerifiedGraph {
    fn signer_did(&self) -> &str {
        match self {
            Self::Create(graph) => graph.signer_did(),
            Self::Mutation(graph) => graph.signer_did(),
        }
    }
}

impl ConfigApplyTxn<'_> {
    /// The caller supplies only the already verified, exact client graph. Its
    /// signer determines the mutation actor; a managed Agent transaction may
    /// carry that graph for a separately authorized relationship operation.
    /// Native ACP remains authoritative for the graph's signer and the Agent.
    pub(crate) async fn execute_verified_client_create(
        &self,
        document: &str,
        variables: &Value,
        graph: Arc<db::block::builder::VerifiedClientCreate>,
    ) -> Result<Value> {
        self.execute_verified_client_graph(document, variables, VerifiedGraph::Create(graph))
            .await
    }

    pub(crate) async fn execute_verified_client_mutation(
        &self,
        document: &str,
        variables: &Value,
        graph: Arc<db::block::builder::VerifiedClientMutation>,
    ) -> Result<Value> {
        self.execute_verified_client_graph(document, variables, VerifiedGraph::Mutation(graph))
            .await
    }

    async fn execute_verified_client_graph(
        &self,
        document: &str,
        variables: &Value,
        graph: VerifiedGraph,
    ) -> Result<Value> {
        let TxnBackend::Embedded {
            node,
            handle,
            identity,
            native_operations,
            ..
        } = &self.backend
        else {
            return Err(ConfigClientError::EmbeddedAccessRequired);
        };
        let actor = identity::Did::new(graph.signer_did())
            .map_err(ConfigClientError::RelationshipTargetIdentity)?;
        if identity.as_ref() != Some(&actor) && self.managed_agent_signer().is_none() {
            return Err(ConfigClientError::TransactionActorMismatch);
        }
        let node = Arc::clone(node);
        let handle = handle.clone();
        let request = QueryRequest::new(document)
            .with_variables(variables.clone())
            .with_identity(Some(actor));
        self.client_signed_write_dispatched
            .store(true, Ordering::Release);
        let started = Instant::now();
        let response = execute_owned_native(
            async move {
                match graph {
                    VerifiedGraph::Create(graph) => {
                        node.execute_verified_create_in_txn(request, &handle, &graph)
                            .await
                    }
                    VerifiedGraph::Mutation(graph) => {
                        node.execute_verified_mutation_in_txn(request, &handle, &graph)
                            .await
                    }
                }
            },
            Arc::clone(native_operations),
            self.operation,
        )
        .await;
        record_slow_native_phase(self.operation, "execute", started.elapsed());
        let response = response?;
        if response.is_transaction_conflict() {
            return Err(retry::transaction_conflict(ConflictSource::StructuredCode));
        }
        if response.has_errors() {
            return Err(retry::transaction_storage_failure(
                ConfigClientError::EmbeddedGraphqlErrors {
                    errors: response.errors,
                },
            ));
        }
        let response = crate::store::graphql::into_data_envelope(response.data);
        self.affected_documents
            .fetch_add(graphql::affected_documents(&response), Ordering::Relaxed);
        #[cfg(test)]
        after_successful_mutation_for_test().await?;
        Ok(response)
    }
}
