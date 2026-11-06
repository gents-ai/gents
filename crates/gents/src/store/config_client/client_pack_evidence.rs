use std::sync::Arc;

use db::block::builder::{
    ClientCreateEvidence, ClientCreateSignatures, ClientDeleteSignatures, ClientMutationEvidence,
    ClientUpdateSignatures, ClientUpdateSnapshot, VerifiedClientCreate, VerifiedClientMutation,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::client_signed_agent::{signing_schema, trusted_delete_schema};
use super::{ConfigAccess, ConfigApplyTxn, ConfigClientError};
use crate::store::identity::NodeIdentity;

/// An exact mutation independently selected by the pack installation plan.
#[derive(Clone, Serialize, Deserialize)]
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
pub enum PackEvidence {
    Create(ClientCreateEvidence),
    Update(ClientMutationEvidence),
    Delete(ClientMutationEvidence),
}

pub enum VerifiedPackEvidence {
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
}

impl From<ConfigClientError> for PackEvidenceError {
    fn from(error: ConfigClientError) -> Self {
        Self::Config(Box::new(error))
    }
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
    mutation: &PackMutation,
) -> Result<PackEvidence, PackEvidenceError> {
    let capability = actor.client_signing_capability()?;
    if capability.did() != actor.did() || access.actor_did().as_deref() != Some(actor.did()) {
        return Err(PackEvidenceError::Mismatch);
    }
    let schema = trusted_schema(mutation)?;
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
    let mutation = mutation.clone();
    let signing = (*capability.config).clone();
    tokio::task::spawn_blocking(move || {
        let crdts = storage::Crdts::default();
        match mutation {
            PackMutation::Create { input, .. } => Ok(PackEvidence::Create(
                ClientCreateSignatures::from_input(&crdts, &schema, &input, &signing)?.evidence(),
            )),
            PackMutation::Update { doc_id, input, .. } => Ok(PackEvidence::Update(
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
            PackMutation::Delete { doc_id, .. } => Ok(PackEvidence::Delete(
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
    .await?
}

/// Verify the plan's exact mutation against the transaction's native snapshot.
pub async fn verify_pack_evidence(
    txn: &ConfigApplyTxn<'_>,
    actor: &str,
    mutation: &PackMutation,
    evidence: PackEvidence,
) -> Result<VerifiedPackEvidence, PackEvidenceError> {
    if txn.actor_did() != Some(actor) {
        return Err(PackEvidenceError::Mismatch);
    }
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
        match (mutation, evidence) {
            (PackMutation::Create { input, .. }, PackEvidence::Create(evidence)) => {
                let graph = VerifiedClientCreate::verify(&crdts, &schema, &input, evidence)?;
                if graph.signer_did() != actor {
                    return Err(PackEvidenceError::Mismatch);
                }
                Ok(VerifiedPackEvidence::Create(Arc::new(graph)))
            }
            (PackMutation::Update { doc_id, input, .. }, PackEvidence::Update(evidence)) => {
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
            (PackMutation::Delete { doc_id, .. }, PackEvidence::Delete(evidence)) => {
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
pub async fn apply_pack_evidence(
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
        let node = Arc::new(
            defra_node::Node::builder()
                .with_node_identity_did(runtime.did())
                .with_node_acp_owner(actor.did().to_owned())
                .build()
                .await?,
        );
        crate::ensure_runtime_schemas(&node).await?;
        let access = ConfigAccess::local_as(node.clone(), actor.did().parse()?);
        let create = PackMutation::Create {
            collection: "PackInstallation".into(),
            input: json!({"node_did": actor.did(), "coordinate": "acme/fixture", "version": "1", "digest": "one"}),
        };
        let evidence = prepare_pack_evidence(&access, &actor, &create).await?;
        let other_access = ConfigAccess::local_as(node.clone(), other.did().parse()?);
        let other_evidence = prepare_pack_evidence(&other_access, &other, &create).await?;
        let cases = tokio::task::spawn_blocking(|| {
            crate::lean_vocab_test::lean_contract_snapshot()
                .hosted_pack_evidence_cases
                .clone()
        })
        .await?;
        let tampered = PackMutation::Create {
            collection: "PackInstallation".into(),
            input: json!({"node_did": actor.did(), "coordinate": "acme/fixture", "version": "1", "digest": "other"}),
        };
        let created = access
            .transact_verified_pack("test.pack_evidence.create", |txn| {
                let evidence = evidence.clone();
                let other_evidence = other_evidence.clone();
                let create = &create;
                let tampered = &tampered;
                let cases = &cases;
                Box::pin(async move {
                    let mut response = None;
                    for case in cases {
                        let actor_matches = case["actor_matches"].as_bool().unwrap();
                        let graph_verified = case["graph_verified"].as_bool().unwrap();
                        let expected = case["allowed"].as_bool().unwrap();
                        let candidate = if graph_verified { create } else { tampered };
                        let signed = if actor_matches {
                            evidence.clone()
                        } else {
                            other_evidence.clone()
                        };
                        let actual = txn.apply(candidate, signed).await;
                        assert_eq!(actual.is_ok(), expected, "{case}");
                        if let Ok(value) = actual {
                            response = Some(value);
                        }
                    }
                    let response = response.expect("modeled successful create");
                    Ok::<_, anyhow::Error>(crate::store::graphql::created_doc_id(
                        &response,
                        "PackInstallation",
                    )?)
                })
            })
            .await?;
        let update = PackMutation::Update {
            collection: "PackInstallation".into(),
            doc_id: created,
            input: json!({"version": "2"}),
        };
        let evidence = prepare_pack_evidence(&access, &actor, &update).await?;
        access
            .transact_verified_pack("test.pack_evidence.update", |txn| {
                let evidence = evidence.clone();
                let update = &update;
                Box::pin(async move {
                    txn.apply(update, evidence).await?;
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await?;
        let observed = access
            .execute("{ PackInstallation { coordinate version } }")
            .await?;
        assert_eq!(observed["data"]["PackInstallation"][0]["version"], "2");
        node.shutdown().await?;
        Ok(())
    }
}
