//! Lean `Identity.OwnerStaged`: an owner-managed relation that a
//! configuration change implies is staged by the document's registered owner.
//! Native refuses a node administrator that does not manage the document on
//! every public route, so Gents never relies on that admission: the actor
//! stages the change when it owns the document; otherwise the embedding
//! runtime stages it as the owner, in the same transaction, for a requester
//! that the modeled authority admits.
//!
//! Over HTTP the runtime cannot join the client's transaction (native accepts
//! a relationship change in a transaction only in one the node itself began),
//! so each change is its own signed command to the runtime's owner-staged
//! route, decided there by the same owner as the signer
//! (`Identity.OwnerStaged.applyOverHttp`). A revocation is sent before the
//! client's transaction commits: a refusal aborts the apply, and revocations
//! already decided stay, so access only narrows. A grant is queued and sent
//! after the commit by the post-commit grant owner; a refusal is the apply's
//! `CommittedRelationshipsIncomplete` error with the documents committed.

use std::sync::Arc;

use futures::future::BoxFuture;
use serde_json::Value;

use super::apply_txn::{EmbeddedTransactionPurpose, TxnBackend};
use super::execute::native_relationship_error;
use super::native::execute_owned_native;
use super::transact::{expect_committed, transact_owned, TransactionMode};
use super::{ConfigAccess, ConfigApplyTxn, ConfigClientError, Result};
use crate::store::config_client::pairing_reader::reader_relation;
use crate::store::config_client::write_telemetry::WriteBackend;
use crate::store::config_client::ConfigRelationshipGrant;
use crate::store::graphql::escape_graphql_string;
use gents_protocol::owner_staged::OwnerStagedChange;

/// What the requester must natively hold before the owner stages a change for
/// it (`Identity.OwnerStaged.Authority`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Authority {
    /// `update` on the changed document itself.
    DocumentUpdate,
    /// `update` on the runtime's own Node row, only for a pairing
    /// observation; the runtime stages only one that it owns.
    PairingNode,
}

/// The native relation a modeled change names: its collection and relation
/// are fixed per path, so no request reaches another relation.
pub(in crate::store::config_client) struct StagedRelation {
    pub(in crate::store::config_client) collection: &'static str,
    pub(in crate::store::config_client) doc_id: String,
    relation: &'static str,
    target: String,
    authority: Authority,
}

impl StagedRelation {
    pub(in crate::store::config_client) fn of(change: &OwnerStagedChange) -> Result<Self> {
        let actor = |did: &str| {
            identity::Did::new(did)
                .map(|did| did.to_string())
                .map_err(ConfigClientError::RelationshipTargetIdentity)
        };
        Ok(match change {
            OwnerStagedChange::EventCursorWriter {
                cursor_doc_id,
                agent_did,
            } => Self {
                collection: "EventSourceCursor",
                doc_id: cursor_doc_id.clone(),
                relation: "writer",
                target: actor(agent_did)?,
                authority: Authority::DocumentUpdate,
            },
            OwnerStagedChange::PlacementReader {
                placement_doc_id,
                reader_did,
            } => Self {
                collection: crate::Collection::RepositoryPlacement.graphql_type(),
                doc_id: placement_doc_id.clone(),
                relation: "reader",
                target: actor(reader_did)?,
                authority: Authority::DocumentUpdate,
            },
            OwnerStagedChange::WorkspaceSelection {
                agent_doc_id,
                callback_doc_id,
            } => Self {
                collection: "Agent",
                doc_id: agent_doc_id.clone(),
                relation: "workspace_selected_by",
                target: super::super::relationships::entity_set_target(
                    "configuration",
                    callback_doc_id,
                    "reader",
                )?,
                authority: Authority::DocumentUpdate,
            },
            OwnerStagedChange::PairingReader {
                observation,
                doc_id,
                reason,
                reader_did,
            } => Self {
                collection: observation.collection(),
                doc_id: doc_id.clone(),
                relation: reader_relation(*reason),
                target: actor(reader_did)?,
                authority: Authority::PairingNode,
            },
        })
    }
}

impl ConfigApplyTxn<'_> {
    /// Request one owner-staged change (`Identity.OwnerStaged.request`) as
    /// this transaction's actor. An embedded transaction decides it in this
    /// transaction. Over HTTP a revocation is decided by the runtime's route
    /// now, before this transaction commits, and a grant after it commits.
    pub async fn request_owner_staged(
        &self,
        change: &OwnerStagedChange,
        grant: bool,
    ) -> Result<()> {
        let staged = StagedRelation::of(change)?;
        match &self.backend {
            TxnBackend::Embedded { .. } if !self.has_pack_interpreter() => {
                self.change_owner_staged(
                    staged.collection,
                    &staged.doc_id,
                    staged.relation,
                    &staged.target,
                    !grant,
                    staged.authority,
                )
                .await
            }
            TxnBackend::Http { endpoint, .. } if !endpoint.replays_pack() => {
                if grant {
                    self.queue_relationship_grant(ConfigRelationshipGrant::OwnerStaged {
                        change: change.clone(),
                    })
                } else {
                    super::super::owner_staged_route::request(endpoint, change, false).await
                }
            }
            // A hosted pack's relationship changes are replayed through its
            // interpreter on the runtime, which native ACP decides as the
            // requester; pairing readers are never pack relationships.
            _ if staged.authority == Authority::PairingNode => {
                Err(ConfigClientError::LocalAccessRequired)
            }
            _ => {
                self.change_document_relationship(
                    staged.collection,
                    &staged.doc_id,
                    staged.relation,
                    &staged.target,
                    !grant,
                )
                .await
            }
        }
    }

    pub(super) async fn change_owner_staged(
        &self,
        collection: &str,
        doc_id: &str,
        relation: &str,
        target: &str,
        delete: bool,
        authority: Authority,
    ) -> Result<()> {
        crate::store::graphql::validate_collection_identifier(collection)?;
        let refused = |kind: fn(String, String) -> ConfigClientError| {
            Err(kind(collection.to_owned(), doc_id.to_owned()))
        };
        let unmanaged = |collection, doc_id| ConfigClientError::OwnerStagedRelationshipUnmanaged {
            collection,
            doc_id,
        };
        let TxnBackend::Embedded {
            node,
            handle,
            identity,
            native_operations,
            ..
        } = &self.backend
        else {
            return Err(ConfigClientError::LocalAccessRequired);
        };
        let runtime = identity::Did::new(
            node.node_identity_did()
                .ok_or(ConfigClientError::LocalAccessRequired)?,
        )
        .map_err(ConfigClientError::RelationshipTargetIdentity)?;
        let requester = identity
            .as_ref()
            .ok_or(ConfigClientError::RelationshipActorRequired)?;
        // Authority first: a requester that may not request the change learns
        // nothing about whether the document exists or who owns it
        // (`unauthorized_refusal_independent_of_document`).
        if !self
            .owner_staged_request_allowed(requester, &runtime, collection, doc_id, authority)
            .await?
        {
            return refused(|collection, doc_id| {
                ConfigClientError::OwnerStagedRelationshipDenied { collection, doc_id }
            });
        }
        // `foundOwner`: as the requester observes it, else as the runtime does,
        // so an authorized requester may change a runtime-owned document it
        // cannot read yet.
        let mut owner = None;
        for reader in [requester, &runtime] {
            owner = self
                .document_owners_as(reader, collection, &[doc_id.to_owned()])
                .await?
                .into_iter()
                .find(|observation| observation.doc_id == doc_id)
                .and_then(|observation| observation.owner);
            if owner.is_some() {
                break;
            }
        }
        let Some(owner) = owner else {
            return refused(unmanaged);
        };
        if &owner == requester {
            return self
                .change_document_relationship(collection, doc_id, relation, target, delete)
                .await;
        }
        // `stagesAs`: an embedded transaction signs only as the runtime or one
        // of its Agents, and the pairing authority reaches only observations
        // the runtime itself owns.
        let managed = match authority {
            Authority::DocumentUpdate => {
                owner == runtime || self.configures_agent(&runtime, &owner).await?
            }
            Authority::PairingNode => owner == runtime,
        };
        if !managed {
            return refused(unmanaged);
        }
        let context = defra_core::RequestContext::default().with_identity(Some(owner.to_string()));
        let node = Arc::clone(node);
        let handle = handle.clone();
        let (collection, doc_id, relation, target) = (
            collection.to_owned(),
            doc_id.to_owned(),
            relation.to_owned(),
            target.to_owned(),
        );
        execute_owned_native(
            async move {
                if delete {
                    node.delete_dac_actor_relationship_for_in_txn(
                        &handle,
                        &context,
                        &collection,
                        &doc_id,
                        &relation,
                        &target,
                    )
                    .await
                } else {
                    node.add_dac_actor_relationship_for_in_txn(
                        &handle,
                        &context,
                        &collection,
                        &doc_id,
                        &relation,
                        &target,
                    )
                    .await
                }
            },
            Arc::clone(native_operations),
            self.operation,
        )
        .await?
        .map_err(native_relationship_error)?;
        Ok(())
    }

    async fn owner_staged_request_allowed(
        &self,
        requester: &identity::Did,
        runtime: &identity::Did,
        collection: &str,
        doc_id: &str,
        authority: Authority,
    ) -> Result<bool> {
        let (collection, doc_id) = match authority {
            Authority::DocumentUpdate => (collection.to_owned(), doc_id.to_owned()),
            Authority::PairingNode => match self.runtime_node_doc_id(runtime).await? {
                Some(node) => ("Node".to_owned(), node),
                None => return Ok(false),
            },
        };
        let decisions = self
            .observe_document_access(
                requester,
                &collection,
                &[doc_id],
                defra_node::DocumentAccessMode::Named {
                    permission: "update".into(),
                    node_permission: acp::nac::NodePermission::DocumentUpdate,
                },
            )
            .await?;
        Ok(matches!(
            decisions.as_slice(),
            [defra_node::DocumentAccessDecision::Allowed]
        ))
    }

    /// The single Node row the runtime reads for its own `node_did` in this
    /// transaction; the requester's view never chooses it. A local write keeps
    /// `node_did` unique against every live row whatever its writer may read,
    /// but a replicated merge can keep a second, unindexed row, so more than
    /// one row resolves to none rather than to either.
    async fn runtime_node_doc_id(&self, runtime: &identity::Did) -> Result<Option<String>> {
        let response = self
            .execute_local_response_with_actor(
                &format!(
                    r#"{{ Node(filter: {{node_did: {{_eq: "{}"}}}}, limit: 2) {{_docID}} }}"#,
                    escape_graphql_string(runtime.as_str()),
                ),
                Some(runtime.clone()),
                None,
            )
            .await?;
        let rows = response
            .data
            .as_ref()
            .and_then(|data| data.get("Node"))
            .and_then(Value::as_array);
        Ok(match rows.map(Vec::as_slice) {
            Some([row]) => row["_docID"]
                .as_str()
                .filter(|id| !id.is_empty())
                .map(str::to_owned),
            _ => None,
        })
    }

    /// Whether `agent` is an Agent configured on the runtime's Node, as the
    /// runtime observes it in this transaction (`stagesAs`): the requester's
    /// read access never decides which owners the runtime manages.
    async fn configures_agent(
        &self,
        runtime: &identity::Did,
        agent: &identity::Did,
    ) -> Result<bool> {
        let response = self
            .execute_local_response_with_actor(
                &format!(
                    r#"{{ Agent(filter: {{node_did: {{_eq: "{}"}}, agent_did: {{_eq: "{}"}}}}, limit: 1) {{_docID}} }}"#,
                    escape_graphql_string(runtime.as_str()),
                    escape_graphql_string(agent.as_str()),
                ),
                Some(runtime.clone()),
                None,
            )
            .await?;
        Ok(response
            .data
            .as_ref()
            .and_then(|data| data.get("Agent"))
            .and_then(Value::as_array)
            .is_some_and(|rows| !rows.is_empty()))
    }
}

impl ConfigAccess {
    /// Begin a transaction as the non-runtime client this access names, for
    /// an operation the runtime authenticated from that client's signed
    /// command. Only the owner-staged decision runs in it.
    pub(in crate::store::config_client) async fn transact_verified_client_owner_staged<'a, F>(
        &'a self,
        operation: &'static str,
        callback: F,
    ) -> Result<()>
    where
        F: for<'txn> Fn(&'txn ConfigApplyTxn<'a>) -> BoxFuture<'txn, Result<()>> + Send + 'a,
    {
        let Self::Local(local) = self else {
            return Err(ConfigClientError::EmbeddedAccessRequired);
        };
        let actor = local
            .identity()
            .cloned()
            .ok_or(ConfigClientError::RelationshipActorRequired)?;
        if local.node().node_identity_did() == Some(actor.as_str()) {
            return Err(ConfigClientError::TransactionActorMismatch);
        }
        transact_owned(
            operation,
            WriteBackend::Embedded,
            TransactionMode::ConflictRetry,
            move |operation, rollback| {
                let actor = actor.clone();
                Box::pin(async move {
                    ConfigApplyTxn::begin_local_owned(
                        local.node(),
                        Some(actor),
                        operation,
                        rollback,
                        EmbeddedTransactionPurpose::VerifiedClient,
                    )
                    .await
                })
            },
            callback,
        )
        .await
        .map(expect_committed)
    }

    /// [`ConfigAccess::request_owner_staged`] for the client this embedded
    /// access names, whose signed command the runtime has authenticated. The
    /// runtime holds no key for that client, so the transaction carries the
    /// client's DID without a signer; the change is still decided by the same
    /// `authorized` and owner checks, as that client.
    pub(crate) async fn request_verified_client_owner_staged(
        &self,
        change: &OwnerStagedChange,
        grant: bool,
    ) -> Result<()> {
        self.transact_verified_client_owner_staged(
            "config.request_verified_client_owner_staged",
            |txn| Box::pin(async move { txn.request_owner_staged(change, grant).await }),
        )
        .await
    }
}

#[cfg(test)]
mod tests;
