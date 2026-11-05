use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use defra_node::Node as EmbeddedNode;
use identity::Did;
use query::TransactionHandle;

use super::gate::mutation_write_gate;
use super::native::NativeOperationTracker;
#[cfg(test)]
use super::rollback::RollbackDropCleanupPause;
use super::rollback::{begin_embedded_owned, begin_http_owned, RollbackOnDrop};
use super::{ConfigClientError, GraphqlEndpoint, Result};
use crate::store::config_client::write_telemetry::WriteOperation;
use crate::store::config_client::AgentPublicationObserver;
use crate::store::config_client::ConfigRelationshipGrant;

pub(super) enum TxnBackend<'a> {
    Http {
        endpoint: &'a GraphqlEndpoint,
        id: String,
        client: reqwest::Client,
    },
    Embedded {
        node: Arc<EmbeddedNode>,
        handle: TransactionHandle,
        identity: Option<Did>,
        agent_signer: Option<crate::store::identity::ManagedAgentSigner>,
        native_operations: Arc<NativeOperationTracker>,
    },
}

/// An open explicit transaction, handed to [`ConfigAccess::transact_write`] callbacks.
/// Raw begin/commit/discard transport lives in the crate-private `graphql`
/// submodule; external callers must use the owned entry points.
pub struct ConfigApplyTxn<'a> {
    pub(super) backend: TxnBackend<'a>,
    pub(super) rollback_on_drop: Option<RollbackOnDrop>,
    pub(super) affected_documents: AtomicU64,
    pub(super) operation: Option<&'static str>,
    pub(super) contends_with_request_siblings: AtomicBool,
    pub(super) client_signed_write_dispatched: AtomicBool,
    pub(super) relationship_grants: Mutex<Vec<ConfigRelationshipGrant>>,
    pub(super) publication_observer: Option<Arc<dyn AgentPublicationObserver>>,
}

pub(super) enum EmbeddedTransactionPurpose {
    Mutation,
    ExecutionRenewal(Arc<NativeOperationTracker>),
    ReadOnly,
}

impl<'a> ConfigApplyTxn<'a> {
    /// An unbound local collection cannot hide individual rows through DAC.
    /// HTTP collection metadata and metadata failures do not prove completeness.
    pub(crate) fn collection_has_no_document_policy(&self, name: &str) -> bool {
        match &self.backend {
            TxnBackend::Embedded { node, .. } => node
                .get_collection(name)
                .ok()
                .flatten()
                .is_some_and(|collection| collection.policy.is_none()),
            TxnBackend::Http { .. } => false,
        }
    }

    pub(crate) fn managed_agent_signer(
        &self,
    ) -> Option<crate::store::identity::ManagedAgentSigner> {
        match &self.backend {
            TxnBackend::Embedded { agent_signer, .. } => agent_signer.clone(),
            TxnBackend::Http { .. } => None,
        }
    }
    /// The explicitly selected transaction actor, never an implicit runtime DID.
    pub fn actor_did(&self) -> Option<&str> {
        match &self.backend {
            TxnBackend::Embedded { identity, .. } => identity.as_ref().map(identity::Did::as_str),
            TxnBackend::Http { endpoint, .. } => endpoint.principal_did.as_deref(),
        }
    }

    pub(crate) fn is_http(&self) -> bool {
        matches!(&self.backend, TxnBackend::Http { .. })
    }

    pub(crate) fn client_signed_configuration(&self) -> bool {
        matches!(&self.backend, TxnBackend::Http { endpoint, .. } if endpoint.client_signed_agent_creates)
    }

    /// Verify a local operation uses this transaction's configured runtime actor.
    pub fn validate_runtime_actor(&self, actor: &identity::Did) -> Result<()> {
        match &self.backend {
            TxnBackend::Embedded { node, identity, .. }
                if identity.as_ref() == Some(actor)
                    && node.node_identity_did() == Some(actor.as_str()) =>
            {
                Ok(())
            }
            _ => Err(ConfigClientError::TransactionActorMismatch),
        }
    }

    pub(super) fn embedded_poisoned(&self) -> bool {
        matches!(
            &self.backend,
            TxnBackend::Embedded { native_operations, .. } if native_operations.is_abandoned()
        )
    }

    pub(super) async fn begin_local_owned<'txn>(
        node: &Arc<EmbeddedNode>,
        identity: Option<Did>,
        operation: WriteOperation,
        cancellation_rollback_scheduled: Arc<AtomicBool>,
        purpose: EmbeddedTransactionPurpose,
    ) -> Result<ConfigApplyTxn<'txn>> {
        Self::begin_local_owned_with_signer(
            node,
            identity,
            None,
            operation,
            cancellation_rollback_scheduled,
            purpose,
        )
        .await
    }

    pub(super) async fn begin_local_owned_with_signer<'txn>(
        node: &Arc<EmbeddedNode>,
        identity: Option<Did>,
        agent_signer: Option<crate::store::identity::ManagedAgentSigner>,
        operation: WriteOperation,
        cancellation_rollback_scheduled: Arc<AtomicBool>,
        purpose: EmbeddedTransactionPurpose,
    ) -> Result<ConfigApplyTxn<'txn>> {
        let read_only = matches!(&purpose, EmbeddedTransactionPurpose::ReadOnly);
        super::gate::validate_managed_signer(node, identity.as_ref(), agent_signer.as_ref())?;
        if !read_only {
            super::gate::require_mutation_signer(node, identity.as_ref(), agent_signer.as_ref())?;
        }
        let native_gate = mutation_write_gate(node);
        let write_guard = match &purpose {
            EmbeddedTransactionPurpose::Mutation => {
                Some(Arc::clone(&native_gate).acquire(operation).await?)
            }
            EmbeddedTransactionPurpose::ExecutionRenewal(_)
            | EmbeddedTransactionPurpose::ReadOnly => None,
        };
        let native_operations = match purpose {
            EmbeddedTransactionPurpose::ExecutionRenewal(native_operations) => native_operations,
            EmbeddedTransactionPurpose::Mutation | EmbeddedTransactionPurpose::ReadOnly => {
                Arc::new(NativeOperationTracker::default())
            }
        };
        let (rollback_on_drop, handle) = begin_embedded_owned(
            Arc::clone(node),
            native_gate,
            read_only,
            operation,
            write_guard,
            Arc::clone(&native_operations),
            cancellation_rollback_scheduled,
            |_| std::future::ready(()),
        )
        .await?;
        Ok(ConfigApplyTxn {
            backend: TxnBackend::Embedded {
                node: Arc::clone(node),
                handle: handle.clone(),
                identity,
                agent_signer,
                native_operations,
            },
            rollback_on_drop: Some(rollback_on_drop),
            affected_documents: AtomicU64::new(0),
            contends_with_request_siblings: AtomicBool::new(false),
            client_signed_write_dispatched: AtomicBool::new(false),
            relationship_grants: Mutex::new(Vec::new()),
            publication_observer: None,
            operation: Some(operation.as_str()),
        })
    }

    #[cfg(test)]
    pub(crate) async fn begin_execution_renewal_for_test(
        node: &Arc<EmbeddedNode>,
    ) -> Result<ConfigApplyTxn<'static>> {
        Self::begin_local_owned(
            node,
            None,
            WriteOperation::new("test.execution_renewal")?,
            Arc::new(AtomicBool::new(false)),
            EmbeddedTransactionPurpose::ExecutionRenewal(Arc::new(
                NativeOperationTracker::default(),
            )),
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn begin_local(
        node: &Arc<EmbeddedNode>,
        identity: Option<Did>,
    ) -> Result<ConfigApplyTxn<'static>> {
        Self::begin_local_owned(
            node,
            identity,
            WriteOperation::new("test.begin_local")?,
            Arc::new(AtomicBool::new(false)),
            EmbeddedTransactionPurpose::Mutation,
        )
        .await
    }

    #[cfg(test)]
    pub(crate) async fn begin_local_with_signer(
        node: &Arc<EmbeddedNode>,
        actor: Did,
        signer: crate::store::identity::ManagedAgentSigner,
    ) -> Result<ConfigApplyTxn<'static>> {
        Self::begin_local_owned_with_signer(
            node,
            Some(actor),
            Some(signer),
            WriteOperation::new("test.begin_local_with_signer")?,
            Arc::new(AtomicBool::new(false)),
            EmbeddedTransactionPurpose::Mutation,
        )
        .await
    }

    #[cfg(test)]
    pub(crate) fn pause_drop_rollback_for_test(&mut self, pause: Arc<RollbackDropCleanupPause>) {
        let rollback = self
            .rollback_on_drop
            .as_mut()
            .expect("new transaction owns its rollback guard");
        let RollbackOnDrop::Embedded {
            drop_cleanup_pause, ..
        } = rollback
        else {
            panic!("test cleanup pause requires an embedded transaction");
        };
        *drop_cleanup_pause = Some(pause);
    }

    pub(super) async fn begin_http(
        endpoint: &'a GraphqlEndpoint,
        cancellation_rollback_scheduled: Arc<AtomicBool>,
        read_only: bool,
    ) -> Result<Self> {
        let (client, id, rollback_on_drop) = tokio::spawn(begin_http_owned(
            endpoint.clone(),
            cancellation_rollback_scheduled,
            read_only,
        ))
        .await
        .map_err(ConfigClientError::BeginTask)??;
        let txn = Self {
            backend: TxnBackend::Http {
                endpoint,
                id: id.clone(),
                client: client.clone(),
            },
            rollback_on_drop: Some(rollback_on_drop),
            affected_documents: AtomicU64::new(0),
            contends_with_request_siblings: AtomicBool::new(false),
            client_signed_write_dispatched: AtomicBool::new(false),
            relationship_grants: Mutex::new(Vec::new()),
            publication_observer: None,
            operation: None,
        };
        Ok(txn)
    }

    pub(super) fn affected_documents(&self) -> u64 {
        self.affected_documents.load(Ordering::Relaxed)
    }

    pub(crate) fn queue_relationship_grant(&self, grant: ConfigRelationshipGrant) -> Result<()> {
        if self.actor_did().is_none() {
            return Err(ConfigClientError::RelationshipActorRequired);
        }
        self.relationship_grants
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(grant);
        Ok(())
    }

    pub(super) async fn complete_relationship_grants(&self) -> Result<()> {
        let grants = std::mem::take(
            &mut *self
                .relationship_grants
                .lock()
                .unwrap_or_else(|e| e.into_inner()),
        );
        if grants.is_empty() {
            return Ok(());
        }
        let creator = self
            .actor_did()
            .ok_or(ConfigClientError::RelationshipActorRequired)?;
        let access = match &self.backend {
            TxnBackend::Http { endpoint, .. } => super::ConfigAccess::Graphql((*endpoint).clone()),
            TxnBackend::Embedded {
                node,
                identity: Some(actor),
                agent_signer,
                ..
            } => super::ConfigAccess::local_with_optional_agent_signer(
                Arc::clone(node),
                actor.clone(),
                agent_signer.clone(),
            )?,
            TxnBackend::Embedded { identity: None, .. } => {
                return Err(ConfigClientError::RelationshipActorRequired)
            }
        };
        if let Some(observer) = &self.publication_observer {
            for grant in &grants {
                observer.before_grant(grant).await.map_err(|error| {
                    ConfigClientError::ClientAgentPublicationUnconfirmed(Box::new(error))
                })?;
                if let Err(error) = access
                    .complete_relationship_grants(creator, std::slice::from_ref(grant))
                    .await
                {
                    return Err(ConfigClientError::ClientAgentPublicationUnconfirmed(
                        Box::new(error),
                    ));
                }
                if let Err(error) = observer.granted(grant).await {
                    return Err(ConfigClientError::ClientAgentPublicationUnconfirmed(
                        Box::new(error),
                    ));
                }
            }
            Ok(())
        } else {
            access.complete_relationship_grants(creator, &grants).await
        }
    }

    pub(super) async fn observe_before_commit(&self) -> Result<()> {
        if let Some(observer) = &self.publication_observer {
            let grants = self
                .relationship_grants
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone();
            if !grants.is_empty() {
                let creator = self
                    .actor_did()
                    .ok_or(ConfigClientError::RelationshipActorRequired)?;
                observer.before_commit(creator, &grants).await?;
            }
        }
        Ok(())
    }

    pub(super) async fn observe_committed(&self) -> Result<()> {
        if self.has_relationship_grants() {
            if let Some(observer) = &self.publication_observer {
                observer.committed().await?;
            }
        }
        Ok(())
    }

    pub(super) async fn observe_commit_failed(&self, definitely_not_committed: bool) -> Result<()> {
        if self.has_relationship_grants() {
            if let Some(observer) = &self.publication_observer {
                observer.commit_failed(definitely_not_committed).await?;
            }
        }
        Ok(())
    }

    pub(super) fn unconfirmed_relationships(&self, source: ConfigClientError) -> ConfigClientError {
        ConfigClientError::UnconfirmedRelationships {
            creator: self
                .actor_did()
                .expect("queued grants require an actor")
                .to_owned(),
            pending: self
                .relationship_grants
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone(),
            source: Box::new(source),
        }
    }

    pub(super) fn has_relationship_grants(&self) -> bool {
        !self
            .relationship_grants
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_empty()
    }

    /// Marks this attempt as writing a key that every concurrent sibling of
    /// its request also writes: the background admission sequence, or the
    /// session activity each published tool terminal refreshes. A conflict
    /// then retries within the sibling budget, since such siblings conflict
    /// until one commits.
    pub(crate) fn contend_with_request_siblings(&self) {
        self.contends_with_request_siblings
            .store(true, Ordering::Relaxed);
    }
}
