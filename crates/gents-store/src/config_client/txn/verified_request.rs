use crate::node_scope::ContextCalls as _;
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Instant;

use defra_node::QueryRequest;
use serde_json::Value;

use super::apply_txn::TxnBackend;
#[cfg(any(test, feature = "test-fixtures"))]
use super::fault_injection::after_successful_mutation_for_test;
use super::native::execute_owned_native;
use super::{graphql, retry, ConfigApplyTxn, ConfigClientError, Result};
use crate::config_client::user_write::{self, Carrier, WriteKind};
use crate::config_client::write_telemetry::{record_slow_native_phase, ConflictSource};

enum VerifiedGraph {
    Create(Arc<db::block::builder::VerifiedClientCreate>),
    Mutation(Arc<db::block::builder::VerifiedClientMutation>, WriteKind),
}

impl VerifiedGraph {
    fn signer_did(&self) -> &str {
        match self {
            Self::Create(graph) => graph.signer_did(),
            Self::Mutation(graph, _) => graph.signer_did(),
        }
    }

    fn write(&self) -> WriteKind {
        match self {
            Self::Create(_) => WriteKind::Create,
            Self::Mutation(_, write) => *write,
        }
    }
}

impl ConfigApplyTxn<'_> {
    /// Whether this transaction may make, for the User `signer`, the `write`
    /// a stored command carries, asked before anything is written. A stored
    /// command carries no delegation yet.
    pub fn admit_stored_user_write(
        &self,
        signer: &str,
        write: WriteKind,
    ) -> std::result::Result<(), user_write::UserWriteRefusal> {
        let TxnBackend::Embedded { node, .. } = &self.backend else {
            return Ok(());
        };
        user_write::signer_credential(
            node,
            user_write::author_of(node, signer, false),
            Carrier::Stored,
            write,
            signer,
            None,
        )
        .map(|_| ())
    }

    /// Whether this transaction may make the request create a stored command
    /// carries for its User, with the request-bound `credentials` it carries,
    /// asked before anything is written; an admitted create is made under
    /// them.
    pub fn admit_stored_user_create(
        &self,
        create: &gents_protocol::request_admission::AgentRequestCreate,
        doc_id: &str,
        credentials: &[String],
    ) -> Result<std::result::Result<(), user_write::UserWriteRefusal>> {
        let TxnBackend::Embedded { node, .. } = &self.backend else {
            return Ok(Ok(()));
        };
        let stored = user_write::StoredCreate::new(
            crate::config_client::StoredCreateCommands::of_request(
                create,
                doc_id,
                crate::local::document_acp::node_policy_ids(node),
            )?,
            credentials.to_vec(),
        );
        if let Err(refusal) =
            user_write::stored_create_credential(node, &create.admission.signer_did, &stored)
        {
            return Ok(Err(refusal));
        }
        if !credentials.is_empty() {
            self.stored_creates
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .insert(doc_id.to_owned(), Arc::new(stored));
        }
        Ok(Ok(()))
    }

    pub(crate) fn stored_create(&self, doc_id: &str) -> Option<Arc<user_write::StoredCreate>> {
        self.stored_creates
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(doc_id)
            .cloned()
    }

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
        write: WriteKind,
    ) -> Result<Value> {
        self.execute_verified_client_graph(
            document,
            variables,
            VerifiedGraph::Mutation(graph, write),
        )
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
        let stored = self.managed_agent_signer().is_some();
        if identity.as_ref() != Some(&actor) && !stored {
            return Err(ConfigClientError::TransactionActorMismatch);
        }
        let stored_create = match &graph {
            VerifiedGraph::Create(create) if stored => self.stored_create(create.doc_id()),
            _ => None,
        };
        let credential = match stored_create {
            Some(create) => user_write::stored_create_credential(node, actor.as_str(), &create)?,
            None => user_write::signer_credential(
                node,
                user_write::author_of(node, actor.as_str(), false),
                if stored {
                    Carrier::Stored
                } else {
                    Carrier::Direct
                },
                graph.write(),
                actor.as_str(),
                self.presented_delegation.as_ref(),
            )?,
        };
        let node = Arc::clone(node);
        let handle = handle.clone();
        let request = QueryRequest::new(document)
            .with_variables(variables.clone())
            .with_identity(Some(actor.clone()))
            .with_context(self.request_context(Some(actor)));
        self.client_signed_write_dispatched
            .store(true, Ordering::Release);
        let started = Instant::now();
        let response = execute_owned_native(
            async move {
                match graph {
                    VerifiedGraph::Create(graph) => {
                        node.execute_verified_create_in_txn(request, &handle, &graph, credential)
                            .await
                    }
                    VerifiedGraph::Mutation(graph, _) => {
                        node.execute_verified_mutation_in_txn(request, &handle, &graph, credential)
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
        let response = crate::graphql::into_data_envelope(response.data);
        self.affected_documents
            .fetch_add(graphql::affected_documents(&response), Ordering::Relaxed);
        #[cfg(any(test, feature = "test-fixtures"))]
        after_successful_mutation_for_test(self.operation).await?;
        Ok(response)
    }
}
