use std::sync::Arc;

use db::block::builder::{
    ClientCreateEvidence, ClientCreateSignatures, ClientDeleteSignatures, ClientMutationEvidence,
    ClientUpdateSignatures, ClientUpdateSnapshot, VerifiedClientCreate, VerifiedClientMutation,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::client_signed_agent::{signing_schema, trusted_delete_schema};
use super::{ConfigAccess, ConfigApplyTxn, ConfigClientError};
use crate::store::identity::NodeIdentity;

/// An exact mutation independently selected by the pack installation plan.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PackMutation {
    Create {
        collection: String,
        input: Value,
    },
    Update {
        collection: String,
        doc_id: String,
        input: Value,
    },
    Delete {
        collection: String,
        doc_id: String,
    },
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", content = "evidence", rename_all = "snake_case")]
enum PackGraphEvidence {
    Create(ClientCreateEvidence),
    Update(ClientMutationEvidence),
    Delete(ClientMutationEvidence),
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackEvidence {
    scope: PackApprovalScope,
    actor_did: String,
    mutation: PackMutation,
    graph: PackGraphEvidence,
    signature: Vec<u8>,
}

/// Identifies the runtime-hosted stage that a client has reviewed.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackApprovalScope {
    pub node_did: String,
    pub pack_digests: Vec<String>,
    pub nonce: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum PackRelationshipTarget {
    Actor {
        did: String,
    },
    Resource {
        resource: String,
        doc_id: String,
    },
    EntitySet {
        resource: String,
        doc_id: String,
        relation: String,
    },
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackRelationship {
    pub collection: String,
    pub doc_id: String,
    pub relation: String,
    pub target: PackRelationshipTarget,
    pub delete: bool,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PackRelationshipEvidence {
    scope: PackApprovalScope,
    actor_did: String,
    relationship: PackRelationship,
    signature: Vec<u8>,
}

pub(super) enum VerifiedPackEvidence {
    Create(Arc<VerifiedClientCreate>),
    Mutation(Arc<VerifiedClientMutation>),
}

#[derive(Debug, thiserror::Error)]
pub enum PackEvidenceError {
    #[error(transparent)]
    Config(Box<ConfigClientError>),
    #[error("pack mutation does not use a trusted configuration schema")]
    UnsupportedCollection,
    #[error("pack mutation evidence does not match its operation or actor")]
    Mismatch,
    #[error("building or verifying client create evidence")]
    Create(#[from] db::block::builder::ClientCreateError),
    #[error("building client update evidence")]
    Update(#[from] db::block::builder::ClientUpdateError),
    #[error("building client deletion evidence")]
    Delete(#[from] db::block::builder::ClientDeleteError),
    #[error("verifying client mutation evidence")]
    Verify(#[from] db::block::builder::VerifiedMutationError),
    #[error("joining client evidence work")]
    Join(#[from] tokio::task::JoinError),
    #[error("encoding the approved pack relationship")]
    Encoding(#[from] serde_json::Error),
    #[error("signing or verifying the approved pack relationship")]
    Identity(#[source] Box<crate::store::identity::IdentityError>),
}

impl From<ConfigClientError> for PackEvidenceError {
    fn from(error: ConfigClientError) -> Self {
        Self::Config(Box::new(error))
    }
}

impl super::TransactionFailure for PackEvidenceError {
    fn config_client_error(&self) -> Option<&ConfigClientError> {
        match self {
            Self::Config(error) => Some(error),
            _ => None,
        }
    }
}

impl From<crate::store::identity::IdentityError> for PackEvidenceError {
    fn from(error: crate::store::identity::IdentityError) -> Self {
        Self::Identity(Box::new(error))
    }
}

fn relationship_payload(
    scope: &PackApprovalScope,
    actor_did: &str,
    relationship: &PackRelationship,
) -> Result<Vec<u8>, PackEvidenceError> {
    validate_scope(scope)?;
    Ok(serde_json::to_vec(&(
        "gents.pack.relationship.v1",
        scope,
        actor_did,
        relationship,
    ))?)
}

fn validate_scope(scope: &PackApprovalScope) -> Result<(), PackEvidenceError> {
    if scope.node_did.is_empty()
        || scope.pack_digests.is_empty()
        || scope.pack_digests.iter().any(String::is_empty)
        || scope.nonce.is_empty()
    {
        return Err(PackEvidenceError::Mismatch);
    }
    Ok(())
}

/// Binds the exact signed graph using SHA-256 collision resistance.
fn graph_payload(
    scope: &PackApprovalScope,
    actor_did: &str,
    mutation: &PackMutation,
    graph: &PackGraphEvidence,
) -> Result<Vec<u8>, PackEvidenceError> {
    validate_scope(scope)?;
    let bytes = serde_json::to_vec(&("gents.pack.graph.v1", scope, actor_did, mutation, graph))?;
    Ok(Sha256::digest(bytes).to_vec())
}

/// Approve one plan-selected relationship with the client's key.
pub async fn prepare_pack_relationship(
    actor: &dyn NodeIdentity,
    scope: &PackApprovalScope,
    relationship: &PackRelationship,
) -> Result<PackRelationshipEvidence, PackEvidenceError> {
    let payload = relationship_payload(scope, actor.did(), relationship)?;
    let signature = actor.sign(&payload).await?;
    Ok(PackRelationshipEvidence {
        scope: scope.clone(),
        actor_did: actor.did().to_owned(),
        relationship: relationship.clone(),
        signature,
    })
}

pub(super) fn verify_pack_relationship(
    actor: &str,
    scope: &PackApprovalScope,
    evidence: PackRelationshipEvidence,
) -> Result<PackRelationship, PackEvidenceError> {
    if evidence.actor_did != actor || evidence.scope != *scope {
        return Err(PackEvidenceError::Mismatch);
    }
    let payload = relationship_payload(scope, actor, &evidence.relationship)?;
    if !crate::store::identity::verify_did_signature(actor, &payload, &evidence.signature)? {
        return Err(PackEvidenceError::Mismatch);
    }
    Ok(evidence.relationship)
}

async fn snapshot(
    txn: &ConfigApplyTxn<'_>,
    doc_id: &str,
) -> Result<ClientUpdateSnapshot, ConfigClientError> {
    let request = defra_http::client_signed_update::history_request(doc_id);
    let value = txn
        .execute_with_variables(
            &request.query,
            &request.variables.unwrap_or_else(|| serde_json::json!({})),
        )
        .await?;
    let response: query::QueryResponse =
        serde_json::from_value(value).map_err(ConfigClientError::ClientSignedSnapshotDecode)?;
    defra_http::client_signed_update::snapshot_from_response(doc_id, &response)
        .map_err(ConfigClientError::ClientConfigurationGraph)
}

fn collection(mutation: &PackMutation) -> &str {
    match mutation {
        PackMutation::Create { collection, .. }
        | PackMutation::Update { collection, .. }
        | PackMutation::Delete { collection, .. } => collection,
    }
}

fn trusted_schema(mutation: &PackMutation) -> Result<schema::CollectionVersion, PackEvidenceError> {
    let name = collection(mutation);
    let sdl = trusted_delete_schema(name).ok_or(PackEvidenceError::UnsupportedCollection)?;
    Ok(signing_schema(sdl, name)?)
}

/// Sign the caller's already validated pack plan with the held client key.
pub async fn prepare_pack_evidence(
    access: &ConfigAccess,
    actor: &dyn NodeIdentity,
    scope: &PackApprovalScope,
    mutation: &PackMutation,
) -> Result<PackEvidence, PackEvidenceError> {
    if access.actor_did().as_deref() != Some(actor.did()) {
        return Err(PackEvidenceError::Mismatch);
    }
    let observed = match mutation {
        PackMutation::Create { .. } => None,
        PackMutation::Update { doc_id, .. } | PackMutation::Delete { doc_id, .. } => Some(
            access
                .transact_readonly("pack.prepare_client_evidence", |txn| {
                    Box::pin(async move { snapshot(txn, doc_id).await })
                })
                .await?,
        ),
    };
    prepare_pack_evidence_observed(actor, scope, mutation, observed).await
}

/// Use the caller's transaction snapshot, including its earlier staged writes.
pub async fn prepare_pack_evidence_in_txn(
    txn: &ConfigApplyTxn<'_>,
    actor: &dyn NodeIdentity,
    scope: &PackApprovalScope,
    mutation: &PackMutation,
) -> Result<PackEvidence, PackEvidenceError> {
    if txn.actor_did() != Some(actor.did()) {
        return Err(PackEvidenceError::Mismatch);
    }
    let observed = match mutation {
        PackMutation::Create { .. } => None,
        PackMutation::Update { doc_id, .. } | PackMutation::Delete { doc_id, .. } => {
            Some(snapshot(txn, doc_id).await?)
        }
    };
    prepare_pack_evidence_observed(actor, scope, mutation, observed).await
}

async fn prepare_pack_evidence_observed(
    actor: &dyn NodeIdentity,
    scope: &PackApprovalScope,
    mutation: &PackMutation,
    observed: Option<ClientUpdateSnapshot>,
) -> Result<PackEvidence, PackEvidenceError> {
    let capability = actor.client_signing_capability()?;
    if capability.did() != actor.did() {
        return Err(PackEvidenceError::Mismatch);
    }
    let schema = trusted_schema(mutation)?;
    let requested = mutation.clone();
    let signing = (*capability.config).clone();
    let graph =
        tokio::task::spawn_blocking(move || -> Result<PackGraphEvidence, PackEvidenceError> {
            let crdts = storage::Crdts::default();
            match requested {
                PackMutation::Create { input, .. } => Ok(PackGraphEvidence::Create(
                    ClientCreateSignatures::from_input(&crdts, &schema, &input, &signing)?
                        .evidence(),
                )),
                PackMutation::Update { doc_id, input, .. } => Ok(PackGraphEvidence::Update(
                    ClientUpdateSignatures::from_input(
                        &crdts,
                        &schema,
                        &doc_id,
                        &input,
                        observed.as_ref().ok_or(PackEvidenceError::Mismatch)?,
                        &signing,
                    )?
                    .evidence(),
                )),
                PackMutation::Delete { doc_id, .. } => Ok(PackGraphEvidence::Delete(
                    ClientDeleteSignatures::from_snapshot(
                        &schema,
                        &doc_id,
                        observed.as_ref().ok_or(PackEvidenceError::Mismatch)?,
                        &signing,
                    )?
                    .evidence(),
                )),
            }
        })
        .await??;
    let payload_scope = scope.clone();
    let payload_actor = actor.did().to_owned();
    let payload_mutation = mutation.clone();
    let (graph, payload) = tokio::task::spawn_blocking(move || {
        let payload = graph_payload(&payload_scope, &payload_actor, &payload_mutation, &graph)?;
        Ok::<_, PackEvidenceError>((graph, payload))
    })
    .await??;
    let signature = actor.sign(&payload).await?;
    Ok(PackEvidence {
        scope: scope.clone(),
        actor_did: actor.did().to_owned(),
        mutation: mutation.clone(),
        graph,
        signature,
    })
}

/// Verify the plan's exact mutation against the transaction's native snapshot.
pub(super) async fn verify_pack_evidence(
    txn: &ConfigApplyTxn<'_>,
    actor: &str,
    scope: &PackApprovalScope,
    mutation: &PackMutation,
    evidence: PackEvidence,
) -> Result<VerifiedPackEvidence, PackEvidenceError> {
    if txn.actor_did() != Some(actor)
        || evidence.actor_did != actor
        || evidence.scope != *scope
        || evidence.mutation != *mutation
    {
        return Err(PackEvidenceError::Mismatch);
    }
    let checked_scope = scope.clone();
    let checked_actor = actor.to_owned();
    let checked_mutation = mutation.clone();
    let evidence = tokio::task::spawn_blocking(move || {
        let payload = graph_payload(
            &checked_scope,
            &checked_actor,
            &checked_mutation,
            &evidence.graph,
        )?;
        if !crate::store::identity::verify_did_signature(
            &checked_actor,
            &payload,
            &evidence.signature,
        )? {
            return Err(PackEvidenceError::Mismatch);
        }
        Ok::<_, PackEvidenceError>(evidence)
    })
    .await??;
    let schema = trusted_schema(mutation)?;
    let observed = match mutation {
        PackMutation::Create { .. } => None,
        PackMutation::Update { doc_id, .. } | PackMutation::Delete { doc_id, .. } => {
            Some(snapshot(txn, doc_id).await?)
        }
    };
    let mutation = mutation.clone();
    let actor = actor.to_owned();
    tokio::task::spawn_blocking(move || {
        let crdts = storage::Crdts::default();
        match (mutation, evidence.graph) {
            (PackMutation::Create { input, .. }, PackGraphEvidence::Create(evidence)) => {
                let graph = VerifiedClientCreate::verify(&crdts, &schema, &input, evidence)?;
                if graph.signer_did() != actor {
                    return Err(PackEvidenceError::Mismatch);
                }
                Ok(VerifiedPackEvidence::Create(Arc::new(graph)))
            }
            (PackMutation::Update { doc_id, input, .. }, PackGraphEvidence::Update(evidence)) => {
                let graph = VerifiedClientMutation::verify_update(
                    &crdts,
                    &schema,
                    &doc_id,
                    &input,
                    observed.as_ref().ok_or(PackEvidenceError::Mismatch)?,
                    evidence,
                )?;
                if graph.signer_did() != actor {
                    return Err(PackEvidenceError::Mismatch);
                }
                Ok(VerifiedPackEvidence::Mutation(Arc::new(graph)))
            }
            (PackMutation::Delete { doc_id, .. }, PackGraphEvidence::Delete(evidence)) => {
                let graph = VerifiedClientMutation::verify_delete(
                    &schema,
                    &doc_id,
                    observed.as_ref().ok_or(PackEvidenceError::Mismatch)?,
                    evidence,
                )?;
                if graph.signer_did() != actor {
                    return Err(PackEvidenceError::Mismatch);
                }
                Ok(VerifiedPackEvidence::Mutation(Arc::new(graph)))
            }
            _ => Err(PackEvidenceError::Mismatch),
        }
    })
    .await?
}

/// Apply only the graph verified for this exact plan operation and actor.
pub(super) async fn apply_pack_evidence(
    txn: &ConfigApplyTxn<'_>,
    mutation: &PackMutation,
    graph: VerifiedPackEvidence,
) -> Result<Value, PackEvidenceError> {
    let _ = trusted_schema(mutation)?;
    let result = match (mutation, graph) {
        (PackMutation::Create { collection, input }, VerifiedPackEvidence::Create(graph)) => {
            let document = format!("mutation($input: {collection}MutationInputArg!) {{ create_{collection}(input: $input) {{ _docID }} }}");
            txn.execute_verified_client_create(
                &document,
                &serde_json::json!({"input": input}),
                graph,
            )
            .await?
        }
        (
            PackMutation::Update {
                collection,
                doc_id,
                input,
            },
            VerifiedPackEvidence::Mutation(graph),
        ) => {
            let doc_id = crate::store::graphql::escape_graphql_string(doc_id);
            let document = format!("mutation($input: {collection}MutationInputArg!) {{ update_{collection}(docID: \"{doc_id}\", input: $input) {{ _docID }} }}");
            txn.execute_verified_client_mutation(
                &document,
                &serde_json::json!({"input": input}),
                graph,
            )
            .await?
        }
        (PackMutation::Delete { collection, doc_id }, VerifiedPackEvidence::Mutation(graph)) => {
            txn.delete_verified_configuration_document(collection, doc_id, graph)
                .await?;
            Value::Null
        }
        _ => return Err(PackEvidenceError::Mismatch),
    };
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::identity::KeyIdentity;
    use serde_json::json;

    #[tokio::test]
    async fn verified_pack_evidence_binds_mutation_actor_and_snapshot() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let actor = KeyIdentity::load_or_create(directory.path().join("user.key"), None)?;
        let runtime = KeyIdentity::load_or_create(directory.path().join("node.key"), None)?;
        let other = KeyIdentity::load_or_create(directory.path().join("other.key"), None)?;
        let mut public_only = defra_core::signing::get_identity(actor.did()).unwrap();
        public_only.private_key_bytes.clear();
        defra_core::signing::store_identity(actor.did(), public_only);
        assert!(!defra_core::signing::get_identity(actor.did())
            .unwrap()
            .has_local_private_key());
        let node = Arc::new(
            defra_node::Node::builder()
                .with_node_identity_did(runtime.did())
                .with_node_acp_owner(actor.did().to_owned())
                .build()
                .await?,
        );
        crate::ensure_runtime_schemas(&node).await?;
        let access = ConfigAccess::local_as(node.clone(), actor.did().parse()?);
        let scope = PackApprovalScope {
            node_did: runtime.did().to_owned(),
            pack_digests: vec!["sha256:fixture".into()],
            nonce: "test-session".into(),
        };
        let node_create = PackMutation::Create {
            collection: "Node".into(),
            input: json!({"node_did": runtime.did(), "enabled": true}),
        };
        let node_evidence = prepare_pack_evidence(&access, &actor, &scope, &node_create).await?;
        let node_doc = access
            .transact_verified_pack("test.pack_evidence.node", |txn| {
                let node_create = &node_create;
                let evidence = node_evidence.clone();
                let scope = &scope;
                Box::pin(async move {
                    let response = txn.apply(scope, node_create, evidence).await?;
                    Ok::<_, anyhow::Error>(crate::store::graphql::created_doc_id(
                        &response, "Node",
                    )?)
                })
            })
            .await?;
        let create = PackMutation::Create {
            collection: "PackInstallation".into(),
            input: json!({"node_did": runtime.did(), "coordinate": "acme/fixture", "version": "1", "digest": "one"}),
        };
        let evidence = prepare_pack_evidence(&access, &actor, &scope, &create).await?;
        let other_access = ConfigAccess::local_as(node.clone(), other.did().parse()?);
        let other_evidence = prepare_pack_evidence(&other_access, &other, &scope, &create).await?;
        let cases = tokio::task::spawn_blocking(|| {
            crate::lean_vocab_test::lean_contract_snapshot()
                .hosted_pack_evidence_cases
                .clone()
        })
        .await?;
        let tampered = PackMutation::Create {
            collection: "PackInstallation".into(),
            input: json!({"node_did": runtime.did(), "coordinate": "acme/fixture", "version": "1", "digest": "other"}),
        };
        let created = access
            .transact_verified_pack("test.pack_evidence.create", |txn| {
                let evidence = evidence.clone();
                let other_evidence = other_evidence.clone();
                let create = &create;
                let tampered = &tampered;
                let cases = &cases;
                let actor = &actor;
                let other = &other;
                let node_doc = &node_doc;
                let scope = &scope;
                Box::pin(async move {
                    let mut response = None;
                    for case in cases {
                        let actor_matches = case["actor_matches"].as_bool().unwrap();
                        let graph_verified = case["graph_verified"].as_bool().unwrap();
                        let plan_approved = case["plan_approved"].as_bool().unwrap();
                        let expected = case["allowed"].as_bool().unwrap();
                        let candidate = if graph_verified { create } else { tampered };
                        let signed = if actor_matches {
                            evidence.clone()
                        } else {
                            other_evidence.clone()
                        };
                        let mut wrong_scope = (*scope).clone();
                        wrong_scope.nonce.push_str("-other");
                        let actual = txn
                            .apply(
                                if plan_approved { scope } else { &wrong_scope },
                                candidate,
                                signed,
                            )
                            .await;
                        assert_eq!(actual.is_ok(), expected, "{case}");
                        if let Ok(value) = actual {
                            response = Some(value);
                        }
                    }
                    let response = response.expect("modeled successful create");
                    let created =
                        crate::store::graphql::created_doc_id(&response, "PackInstallation")?;
                    let parent = PackRelationship {
                        collection: "PackInstallation".into(),
                        doc_id: created.clone(),
                        relation: "parent".into(),
                        target: PackRelationshipTarget::Resource {
                            resource: "nodes".into(),
                            doc_id: node_doc.to_owned(),
                        },
                        delete: false,
                    };
                    let signed = prepare_pack_relationship(actor, scope, &parent).await?;
                    let mut altered = signed.clone();
                    altered.relationship.relation = "reader".into();
                    let expected = cases
                        .iter()
                        .find(|case| {
                            case["actor_matches"] == true
                                && case["graph_verified"] == false
                                && case["plan_approved"] == true
                        })
                        .unwrap()["allowed"]
                        .as_bool()
                        .unwrap();
                    assert_eq!(
                        txn.apply_relationship(scope, altered).await.is_ok(),
                        expected
                    );
                    let other_signed = prepare_pack_relationship(other, scope, &parent).await?;
                    let expected = cases
                        .iter()
                        .find(|case| {
                            case["actor_matches"] == false
                                && case["graph_verified"] == true
                                && case["plan_approved"] == true
                        })
                        .unwrap()["allowed"]
                        .as_bool()
                        .unwrap();
                    assert_eq!(
                        txn.apply_relationship(scope, other_signed).await.is_ok(),
                        expected
                    );
                    let mut wrong_scope = (*scope).clone();
                    wrong_scope.nonce.push_str("-other");
                    let expected = cases
                        .iter()
                        .find(|case| {
                            case["actor_matches"] == true
                                && case["graph_verified"] == true
                                && case["plan_approved"] == false
                        })
                        .unwrap()["allowed"]
                        .as_bool()
                        .unwrap();
                    assert_eq!(
                        txn.apply_relationship(&wrong_scope, signed.clone())
                            .await
                            .is_ok(),
                        expected
                    );
                    txn.apply_relationship(scope, signed).await?;
                    Ok::<_, anyhow::Error>(created)
                })
            })
            .await?;
        let update = PackMutation::Update {
            collection: "PackInstallation".into(),
            doc_id: created,
            input: json!({"version": "2"}),
        };
        let evidence = access
            .transact_readonly("test.pack_evidence.prepare_in_txn", |txn| {
                let actor = &actor;
                let scope = &scope;
                let update = &update;
                Box::pin(
                    async move { prepare_pack_evidence_in_txn(txn, actor, scope, update).await },
                )
            })
            .await?;
        access
            .transact_verified_pack("test.pack_evidence.update", |txn| {
                let evidence = evidence.clone();
                let update = &update;
                let scope = &scope;
                Box::pin(async move {
                    txn.apply(scope, update, evidence).await?;
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await?;
        let observed = access
            .execute("{ PackInstallation { coordinate version } }")
            .await?;
        assert_eq!(observed["data"]["PackInstallation"][0]["version"], "2");
        let stale = PackMutation::Update {
            collection: "PackInstallation".into(),
            doc_id: match &update {
                PackMutation::Update { doc_id, .. } => doc_id.clone(),
                _ => unreachable!(),
            },
            input: json!({"version": "stale"}),
        };
        let stale_evidence = prepare_pack_evidence(&access, &actor, &scope, &stale).await?;
        let replacement = PackMutation::Update {
            collection: "PackInstallation".into(),
            doc_id: match &update {
                PackMutation::Update { doc_id, .. } => doc_id.clone(),
                _ => unreachable!(),
            },
            input: json!({"version": "winner"}),
        };
        let replacement_evidence =
            prepare_pack_evidence(&access, &actor, &scope, &replacement).await?;
        access
            .transact_verified_pack("test.pack_evidence.winner", |txn| {
                let replacement_evidence = replacement_evidence.clone();
                let replacement = &replacement;
                let scope = &scope;
                Box::pin(async move {
                    txn.apply(scope, replacement, replacement_evidence).await?;
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await?;
        let stale_result = access
            .transact_verified_pack("test.pack_evidence.stale", |txn| {
                let evidence = stale_evidence.clone();
                let stale = &stale;
                let scope = &scope;
                Box::pin(async move {
                    txn.apply(scope, stale, evidence).await?;
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await;
        let stale_expected = cases
            .iter()
            .find(|case| case["actor_matches"] == true && case["graph_verified"] == false)
            .expect("modeled stale graph")["allowed"]
            .as_bool()
            .unwrap();
        assert_eq!(stale_result.is_ok(), stale_expected);
        let observed = access
            .execute("{ PackInstallation { coordinate version } }")
            .await?;
        assert_eq!(observed["data"]["PackInstallation"][0]["version"], "winner");
        node.shutdown().await?;
        Ok(())
    }
}
