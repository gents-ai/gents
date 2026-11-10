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
//! route, decided there by the same owner as the signer. An open HTTP
//! transaction must not read documents such a command then changes (DefraDB
//! refuses its commit without an abort receipt), so an apply that needs
//! revocations runs PLAN, REVOKE, WRITE (`Identity.OwnerStaged.applyOverHttp`):
//! the transaction owner runs the apply once and discards it to learn its
//! revocations, sends them before any write transaction is open, then runs the
//! apply again and commits only if it derives exactly the same revocations. A
//! grant is queued and sent after the commit by the post-commit grant owner; a
//! refusal is the apply's `CommittedRelationshipsIncomplete` error with the
//! documents committed.

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

/// How an HTTP transaction handles an owner-staged revocation it requests
/// (`Identity.OwnerStaged.applyOverHttp`). Embedded transactions decide every
/// change in the transaction and never consult this.
pub(crate) enum OwnerStagedRevocations {
    /// Sent to the route when requested: a transaction no owner drives
    /// through PLAN and WRITE.
    Send,
    /// PLAN: record each revocation; the transaction owner discards this
    /// attempt, confirmed, and sends them before WRITE.
    Plan(Vec<OwnerStagedChange>),
    /// WRITE: the planned revocations were sent; record what this attempt
    /// derives so the owner can revalidate it before committing.
    Write {
        planned: Vec<OwnerStagedChange>,
        derived: Vec<OwnerStagedChange>,
    },
}

/// The transaction owner's next step after a successful HTTP callback.
pub(super) enum OwnerStagedCheckpoint {
    Commit,
    Revoke(Vec<OwnerStagedChange>),
}

/// Which subjects an operation may grant (`Identity.OwnerStaged.Target`);
/// revocations only narrow access and are not constrained.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum TargetKind {
    /// An Agent identity the runtime controls (`controls_agent`).
    ControlledAgent,
    /// A reader the RepositoryPlacement itself declares in `readers`.
    DeclaredReader,
    /// The `reader` set of this Callback, which must be bound to the Agent.
    BoundCallback { callback_doc_id: String },
    /// Any subject: pairing readers, named by an operator with Node authority.
    Any,
}

/// What the requester must natively hold before the owner stages a change for
/// it (`Identity.OwnerStaged.Authority`): `update` on the runtime's own Node
/// row in both cases, never `update` on the document alone, which `writer`
/// grants.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Authority {
    /// A configuration relation, granted only to targets of its kind.
    Configuration(TargetKind),
    /// A pairing observation's reader; the runtime stages only one it owns.
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
                authority: Authority::Configuration(TargetKind::ControlledAgent),
            },
            OwnerStagedChange::PlacementReader {
                placement_doc_id,
                reader_did,
            } => Self {
                collection: crate::Collection::RepositoryPlacement.graphql_type(),
                doc_id: placement_doc_id.clone(),
                relation: "reader",
                target: actor(reader_did)?,
                authority: Authority::Configuration(TargetKind::DeclaredReader),
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
                authority: Authority::Configuration(TargetKind::BoundCallback {
                    callback_doc_id: callback_doc_id.clone(),
                }),
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
    /// Drive this HTTP attempt as PLAN (`None`) or as WRITE of `planned`.
    pub(super) fn drive_owner_staged(&self, planned: Option<&[OwnerStagedChange]>) {
        if matches!(self.backend, TxnBackend::Http { .. }) {
            *self
                .owner_staged_revocations
                .lock()
                .unwrap_or_else(|e| e.into_inner()) = match planned {
                None => OwnerStagedRevocations::Plan(Vec::new()),
                Some(planned) => OwnerStagedRevocations::Write {
                    planned: planned.to_vec(),
                    derived: Vec::new(),
                },
            };
        }
    }

    /// The apply's owner states `SignedConflictRebuild`'s contract for its
    /// callback: it re-reads its inputs, has no effects outside its
    /// transaction, and signs afresh on every attempt. Over HTTP this lets
    /// the transaction owner rerun it after a confirmed PLAN discard.
    pub(crate) fn authorize_planned_replay(&self) {
        self.planned_replay_authorized
            .store(true, std::sync::atomic::Ordering::Release);
    }

    /// After a successful callback: commit, revoke the plan first, or refuse
    /// a WRITE that derived other revocations than were sent
    /// (`Identity.OwnerStaged.changed_plan_is_refused_and_no_wider`).
    pub(super) fn owner_staged_checkpoint(&self) -> Result<OwnerStagedCheckpoint> {
        match &*self
            .owner_staged_revocations
            .lock()
            .unwrap_or_else(|e| e.into_inner())
        {
            OwnerStagedRevocations::Send => Ok(OwnerStagedCheckpoint::Commit),
            OwnerStagedRevocations::Plan(planned) if planned.is_empty() => {
                Ok(OwnerStagedCheckpoint::Commit)
            }
            OwnerStagedRevocations::Plan(planned) => {
                Ok(OwnerStagedCheckpoint::Revoke(planned.clone()))
            }
            OwnerStagedRevocations::Write { planned, derived } if planned == derived => {
                Ok(OwnerStagedCheckpoint::Commit)
            }
            OwnerStagedRevocations::Write { planned, .. } => {
                Err(ConfigClientError::OwnerStagedPlanChanged {
                    revoked: planned.len(),
                })
            }
        }
    }

    /// REVOKE: send each planned revocation, in order, before any write
    /// transaction is open. The first refusal is the apply's error; the
    /// revocations already decided stay.
    pub(super) async fn send_planned_revocations(
        endpoint: &super::GraphqlEndpoint,
        planned: &[OwnerStagedChange],
    ) -> Result<()> {
        for change in planned {
            super::super::owner_staged_route::request(endpoint, change, false).await?;
        }
        Ok(())
    }

    pub(super) fn http_endpoint(&self) -> Option<&super::GraphqlEndpoint> {
        match &self.backend {
            TxnBackend::Http { endpoint, .. } => Some(endpoint),
            TxnBackend::Embedded { .. } => None,
        }
    }

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
                    &staged.authority,
                )
                .await
            }
            TxnBackend::Http { endpoint, .. } if !endpoint.replays_pack() => {
                if grant {
                    return self.queue_relationship_grant(ConfigRelationshipGrant::OwnerStaged {
                        change: change.clone(),
                    });
                }
                {
                    let mut revocations = self
                        .owner_staged_revocations
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    match &mut *revocations {
                        OwnerStagedRevocations::Send => {}
                        OwnerStagedRevocations::Plan(planned) => {
                            planned.push(change.clone());
                            return Ok(());
                        }
                        OwnerStagedRevocations::Write { derived, .. } => {
                            derived.push(change.clone());
                            return Ok(());
                        }
                    }
                }
                super::super::owner_staged_route::request(endpoint, change, false).await
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
        authority: &Authority,
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
            .owner_staged_request_allowed(requester, &runtime, collection, authority)
            .await?
        {
            return refused(|collection, doc_id| {
                ConfigClientError::OwnerStagedRelationshipDenied { collection, doc_id }
            });
        }
        // `out_of_kind_grant_is_refused`: after authority, before the owner.
        if !delete {
            let kind = match authority {
                Authority::Configuration(kind) => kind,
                Authority::PairingNode => &TargetKind::Any,
            };
            if !self.admits_target(&runtime, doc_id, target, kind).await? {
                return refused(
                    |collection, doc_id| ConfigClientError::OwnerStagedTargetRefused {
                        collection,
                        doc_id,
                    },
                );
            }
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
            Authority::Configuration(_) => {
                owner == runtime || self.controls_agent(&runtime, &owner).await?
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

    /// `authorized`: `update` on the runtime's own Node row, and a pairing
    /// relation only on a pairing observation collection.
    async fn owner_staged_request_allowed(
        &self,
        requester: &identity::Did,
        runtime: &identity::Did,
        collection: &str,
        authority: &Authority,
    ) -> Result<bool> {
        if matches!(authority, Authority::PairingNode)
            && !matches!(collection, "PeerPairingDesired" | "PeerPairingApplied")
        {
            return Ok(false);
        }
        let (collection, doc_id) = match self.runtime_node_doc_id(runtime).await? {
            Some(node) => ("Node".to_owned(), node),
            None => return Ok(false),
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

    /// `admitsTarget`, read as the runtime in this transaction.
    async fn admits_target(
        &self,
        runtime: &identity::Did,
        doc_id: &str,
        target: &str,
        kind: &TargetKind,
    ) -> Result<bool> {
        let read = |query: String| async move {
            let response = self
                .execute_local_response_with_actor(&query, Some(runtime.clone()), None)
                .await?;
            Ok::<_, ConfigClientError>(response.data.unwrap_or(Value::Null))
        };
        match kind {
            TargetKind::Any => Ok(true),
            TargetKind::ControlledAgent => match identity::Did::new(target) {
                Ok(agent) => self.controls_agent(runtime, &agent).await,
                Err(_) => Ok(false),
            },
            TargetKind::DeclaredReader => {
                let data = read(format!(
                    r#"{{ RepositoryPlacement(filter: {{_docID: {{_eq: "{}"}}}}, limit: 2) {{readers}} }}"#,
                    escape_graphql_string(doc_id),
                ))
                .await?;
                let Some([row]) = data["RepositoryPlacement"].as_array().map(Vec::as_slice) else {
                    return Ok(false);
                };
                let readers = match &row["readers"] {
                    Value::String(raw) => serde_json::from_str(raw).unwrap_or(Value::Null),
                    other => other.clone(),
                };
                Ok(readers.as_array().is_some_and(|readers| {
                    readers
                        .iter()
                        .any(|reader| reader["agent_did"].as_str() == Some(target))
                }))
            }
            TargetKind::BoundCallback { callback_doc_id } => {
                // The Callback's own configuration selects the Agent, read by
                // the workspace-selection owner (`selected_agents`).
                let data = read(format!(
                    r#"{{ Agent(filter: {{_docID: {{_eq: "{}"}}}}, limit: 2) {{agent_did node_did}} Callback(filter: {{_docID: {{_eq: "{}"}}}}, limit: 2) {{callback_id node_did enabled handler}} }}"#,
                    escape_graphql_string(doc_id),
                    escape_graphql_string(callback_doc_id),
                ))
                .await?;
                let (Some([agent]), Some([callback])) = (
                    data["Agent"].as_array().map(Vec::as_slice),
                    data["Callback"].as_array().map(Vec::as_slice),
                ) else {
                    return Ok(false);
                };
                let (Some(agent_did), Some(node_did)) =
                    (agent["agent_did"].as_str(), callback["node_did"].as_str())
                else {
                    return Ok(false);
                };
                if agent["node_did"].as_str() != Some(node_did) {
                    return Ok(false);
                }
                let mut value = callback.clone();
                if let Some(raw) = value["handler"].as_str() {
                    value["handler"] = serde_json::from_str(raw).unwrap_or(Value::Null);
                }
                if value["enabled"] == Value::Bool(false) {
                    return Ok(false);
                }
                Ok(
                    super::super::desired_state::workspace_selection::selected_agents(
                        node_did, &value,
                    )
                    .is_ok_and(|agents| agents.contains(agent_did)),
                )
            }
        }
    }

    /// Whether `agent` is an Agent identity the runtime controls (`stagesAs`,
    /// `derivedAgents`): exactly one Agent row on the runtime's Node names it,
    /// as the runtime observes it in this transaction, and its DID derives
    /// from the runtime key and that row's creation nonce. A row's
    /// `agent_did` is written unverified, so a row alone never suffices; the
    /// requester's read access never decides it.
    async fn controls_agent(&self, runtime: &identity::Did, agent: &identity::Did) -> Result<bool> {
        let response = self
            .execute_local_response_with_actor(
                &format!(
                    r#"{{ Agent(filter: {{node_did: {{_eq: "{}"}}, agent_did: {{_eq: "{}"}}}}, limit: 2) {{agent_did creation_nonce}} }}"#,
                    escape_graphql_string(runtime.as_str()),
                    escape_graphql_string(agent.as_str()),
                ),
                Some(runtime.clone()),
                None,
            )
            .await?;
        let rows = response
            .data
            .as_ref()
            .and_then(|data| data.get("Agent"))
            .and_then(Value::as_array);
        let Some([row]) = rows.map(Vec::as_slice) else {
            return Ok(false);
        };
        let (Some(agent_did), Some(creation_nonce)) =
            (row["agent_did"].as_str(), row["creation_nonce"].as_str())
        else {
            return Ok(false);
        };
        let Some(proof) = &self.agent_proof else {
            return Ok(false);
        };
        Ok(agent_did == agent.as_str()
            && proof.derives(&gents_protocol::agent_identity::AgentIdentityBinding {
                agent_did: agent_did.to_owned(),
                creation_nonce: creation_nonce.to_owned(),
            }))
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
                let proof = local.agent_proof.clone();
                Box::pin(async move {
                    let mut txn = ConfigApplyTxn::begin_local_owned(
                        local.node(),
                        Some(actor),
                        operation,
                        rollback,
                        EmbeddedTransactionPurpose::VerifiedClient,
                    )
                    .await?;
                    txn.agent_proof = proof;
                    Ok(txn)
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

    /// The decision's first step alone (`authorized`), read as the runtime:
    /// whether this access's verified client may request `change` at all. The
    /// route asks before it consumes a command's nonce, so an unauthorized
    /// signer makes the runtime write nothing; the decision checks it again.
    pub(crate) async fn verified_client_may_request_owner_staged(
        &self,
        change: &OwnerStagedChange,
    ) -> Result<bool> {
        let Self::Local(local) = self else {
            return Err(ConfigClientError::EmbeddedAccessRequired);
        };
        let requester = local
            .identity()
            .cloned()
            .ok_or(ConfigClientError::RelationshipActorRequired)?;
        let runtime = identity::Did::new(
            local
                .node()
                .node_identity_did()
                .ok_or(ConfigClientError::LocalAccessRequired)?,
        )
        .map_err(ConfigClientError::RelationshipTargetIdentity)?;
        let staged = StagedRelation::of(change)?;
        ConfigAccess::local_as(local.node().clone(), runtime.clone())
            .transact_readonly("config.owner_staged_authority", |txn| {
                let (requester, runtime, staged) = (&requester, &runtime, &staged);
                Box::pin(async move {
                    txn.owner_staged_request_allowed(
                        requester,
                        runtime,
                        staged.collection,
                        &staged.authority,
                    )
                    .await
                })
            })
            .await
    }
}

#[cfg(test)]
mod tests;
