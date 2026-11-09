use serde_json::Value;
use std::sync::atomic::Ordering;

use super::apply_txn::TxnBackend;
use super::{ConfigApplyTxn, ConfigClientError, Result};
use crate::store::config_client::client_pack_evidence::{
    apply_pack_evidence, verify_pack_evidence,
};
use crate::store::config_client::{PackMutation, PackRelationship, PackRelationshipTarget};

pub(super) fn relationship(
    collection: &str,
    doc_id: &str,
    relation: &str,
    target: &str,
    delete: bool,
) -> Result<PackRelationship> {
    let target = if target.starts_with("did:") {
        PackRelationshipTarget::Actor {
            did: target.to_owned(),
        }
    } else {
        match acp::parse_target_subject(target) {
            Ok(acp::Subject::EntitySet {
                resource,
                object_id,
                relation,
            }) if relation.is_empty() => PackRelationshipTarget::Resource {
                resource,
                doc_id: object_id,
            },
            Ok(acp::Subject::EntitySet {
                resource,
                object_id,
                relation,
            }) => PackRelationshipTarget::EntitySet {
                resource,
                doc_id: object_id,
                relation,
            },
            _ => return Err(ConfigClientError::InvalidResourceRelationshipTarget),
        }
    };
    Ok(PackRelationship {
        collection: collection.to_owned(),
        doc_id: doc_id.to_owned(),
        relation: relation.to_owned(),
        target,
        delete,
    })
}

impl ConfigApplyTxn<'_> {
    pub(crate) fn has_pack_interpreter(&self) -> bool {
        self.pack_approval.is_some()
            || matches!(&self.backend, TxnBackend::Http { endpoint, .. } if endpoint.pack_replay.is_some())
    }

    pub(super) async fn apply_pack_interpreter_effect(
        &self,
        mutation: PackMutation,
    ) -> Result<Value> {
        self.client_signed_write_dispatched
            .store(true, Ordering::Release);
        if let Some(approval) = &self.pack_approval {
            let history = match &mutation {
                PackMutation::Create { .. } => None,
                PackMutation::Update { doc_id, .. } | PackMutation::Delete { doc_id, .. } => {
                    let request = defra_http::client_signed_update::history_request(doc_id);
                    Some(
                        self.execute_with_variables(
                            &request.query,
                            &request.variables.unwrap_or_else(|| serde_json::json!({})),
                        )
                        .await?,
                    )
                }
            };
            let evidence = approval
                .approver
                .approve_graph(self, &mutation, history)
                .await?;
            let actor = self
                .actor_did()
                .ok_or(ConfigClientError::RelationshipActorRequired)?;
            let graph = verify_pack_evidence(self, actor, &approval.scope, &mutation, evidence)
                .await
                .map_err(|error| ConfigClientError::PackEvidence(Box::new(error)))?;
            let value = apply_pack_evidence(self, &mutation, graph)
                .await
                .map_err(|error| ConfigClientError::PackEvidence(Box::new(error)))?;
            approval.approver.graph_applied(&value).await?;
            return Ok(value);
        }
        if let TxnBackend::Http { endpoint, .. } = &self.backend {
            if let Some(replay) = &endpoint.pack_replay {
                return Ok(replay.0.apply_graph(&mutation).await?);
            }
        }
        Err(ConfigClientError::EmbeddedAccessRequired)
    }

    pub(super) async fn apply_pack_interpreter_relationship(
        &self,
        relationship: PackRelationship,
    ) -> Result<()> {
        self.client_signed_write_dispatched
            .store(true, Ordering::Release);
        if let Some(approval) = &self.pack_approval {
            let evidence = approval
                .approver
                .approve_relationship(self, &relationship)
                .await?;
            let verified = super::verified_pack::VerifiedPackTxn { inner: self };
            verified
                .apply_relationship(&approval.scope, &relationship, evidence)
                .await
                .map_err(|error| ConfigClientError::PackEvidence(Box::new(error)))?;
            approval.approver.relationship_applied().await?;
            return Ok(());
        }
        if let TxnBackend::Http { endpoint, .. } = &self.backend {
            if let Some(replay) = &endpoint.pack_replay {
                return Ok(replay.0.apply_relationship(&relationship).await?);
            }
        }
        Err(ConfigClientError::EmbeddedAccessRequired)
    }
}
