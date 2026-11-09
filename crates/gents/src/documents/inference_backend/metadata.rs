use anyhow::{Context, Result};

use super::{InferenceBackend, InferenceBackendMetadata};
use crate::documents::backend_credential::{
    backend_credential_in_txn, put_backend_credential_in_txn, BackendCredential,
    BackendCredentialSecret,
};
use crate::store::config_client::{
    ConfigAccess, ConfigApplyTxn, DesiredStateApplyDocument, DesiredStateApplyPlan,
};
use crate::Collection;

pub(crate) fn backend_metadata_fields() -> Result<&'static [&'static str]> {
    crate::store::config_client::canonical_struct_fields::<InferenceBackendMetadata>()
}

/// Retire the two owned records in one transaction. Credentials can be shared
/// and remain private; incoming relationships retain the old physical ID.
pub(crate) async fn remove_backend_metadata_in_txn(
    txn: &ConfigApplyTxn<'_>,
    metadata_doc: &str,
    node_did: &str,
    backend_id: &str,
) -> Result<()> {
    let node = crate::store::graphql::escape_graphql_string(node_did);
    let backend = crate::store::graphql::escape_graphql_string(backend_id);
    let response = txn.execute(&format!(
        r#"{{ InferenceBackendObservation(filter: {{node_did: {{_eq: "{node}"}}, backend_id: {{_eq: "{backend}"}}}}, limit: 2) {{ _docID node_did backend_id metadata_doc_id }} }}"#
    )).await?;
    let observations = gents_protocol::graphql::graphql_rows_from_response(
        &response,
        "InferenceBackendObservation",
    );
    let observation =
        super::observation_binding::bind_rows(node_did, backend_id, metadata_doc, &observations)?
            .context("backend observation is unavailable")?;
    let observation_doc = observation["_docID"]
        .as_str()
        .filter(|doc| !doc.is_empty())
        .context("backend observation has no physical ID")?;
    let (node_doc, _) = crate::store::config_client::read_desired_state_record_in_txn(
        txn,
        Collection::Node,
        node_did,
        node_did,
    )
    .await?
    .context("backend removal requires its visible Node")?;
    txn.delete_entity_set_relationship(
        "InferenceBackendObservation",
        observation_doc,
        "selected_by",
        "configuration",
        metadata_doc,
        "selected_by",
    )
    .await?;
    txn.delete_actor_relationship(
        "InferenceBackendObservation",
        observation_doc,
        "writer",
        &identity::Did::new(node_did)?,
    )
    .await?;
    txn.delete_resource_relationship(
        "InferenceBackendObservation",
        observation_doc,
        "parent",
        "nodes",
        &node_doc,
    )
    .await?;
    txn.delete_resource_relationship(
        "InferenceBackendMetadata",
        metadata_doc,
        "parent",
        "nodes",
        &node_doc,
    )
    .await?;
    txn.delete_configuration_document("InferenceBackendObservation", observation_doc)
        .await?;
    txn.delete_configuration_document("InferenceBackendMetadata", metadata_doc)
        .await?;
    Ok(())
}

pub(crate) async fn prepare_backend_metadata_in_txn(
    txn: &ConfigApplyTxn<'_>,
    backend: InferenceBackend,
    previous: Option<&serde_json::Value>,
) -> Result<(InferenceBackendMetadata, Option<BackendCredential>)> {
    use super::{BackendAuth, BackendAuthSelection};
    let prior_auth = previous
        .map(|value| serde_json::from_value::<InferenceBackend>(value.clone()))
        .transpose()?
        .map(|prior| prior.auth);
    let mut replacement = None;
    let selection = match &backend.auth {
        BackendAuth::Unauthenticated => BackendAuthSelection::Unauthenticated,
        BackendAuth::Environment { variable } => BackendAuthSelection::Environment {
            variable: variable.clone(),
        },
        BackendAuth::NodeOAuth { account_ref } => BackendAuthSelection::NodeOAuth {
            account_ref: account_ref.clone(),
        },
        BackendAuth::NodeOAuthPool { allow_overage } => BackendAuthSelection::NodeOAuthPool {
            allow_overage: *allow_overage,
        },
        BackendAuth::Credential { id, version } => {
            let stored = backend_credential_in_txn(txn, id).await?;
            anyhow::ensure!(
                stored.version == *version && stored.node_did == backend.node_did,
                "existing backend credential does not match metadata"
            );
            anyhow::ensure!(
                stored.matches_binding(
                    &backend.node_did,
                    backend.provider_kind.as_str(),
                    &backend.endpoint,
                    version,
                ),
                "stored backend credential is bound to a different provider or endpoint; supply credentials for the new destination"
            );
            BackendAuthSelection::Credential {
                id: id.clone(),
                version: version.clone(),
            }
        }
        raw => {
            let desired = BackendCredentialSecret::from_backend_auth(raw.clone())
                .context("unsupported backend credential")?;
            let previous_id = match prior_auth {
                Some(BackendAuth::Credential { id, version }) => {
                    let stored = backend_credential_in_txn(txn, &id).await?;
                    anyhow::ensure!(
                        stored.version == version && stored.node_did == backend.node_did,
                        "existing backend credential does not match metadata"
                    );
                    if stored.matches_binding(
                        &backend.node_did,
                        backend.provider_kind.as_str(),
                        &backend.endpoint,
                        &version,
                    ) && stored.secret == desired
                    {
                        return Ok((
                            InferenceBackendMetadata::from_backend(
                                backend,
                                BackendAuthSelection::Credential { id, version },
                            )?,
                            None,
                        ));
                    }
                    Some(id)
                }
                _ => None,
            };
            let secret = BackendCredential::for_rotation(
                previous_id,
                backend.node_did.clone(),
                backend.provider_kind.as_str().to_owned(),
                backend.endpoint.clone(),
                desired,
            );
            let selection = BackendAuthSelection::Credential {
                id: secret.credential_id.clone(),
                version: secret.version.clone(),
            };
            replacement = Some(secret);
            selection
        }
    };
    Ok((
        InferenceBackendMetadata::from_backend(backend, selection)?,
        replacement,
    ))
}

/// Publish a secret and its public selection under the same actor and native
/// transaction. A supplied secret must match every part of that selection.
pub(crate) async fn write_backend_metadata_in_txn(
    txn: &ConfigApplyTxn<'_>,
    metadata: &InferenceBackendMetadata,
    credential: Option<&BackendCredential>,
) -> Result<String> {
    use super::BackendAuthSelection;
    match (metadata.auth_selection()?, credential) {
        (BackendAuthSelection::Credential { id, version }, Some(secret)) => {
            anyhow::ensure!(
                secret.credential_id == id
                    && secret.version == version
                    && secret.node_did == metadata.node_did
                    && secret.provider_kind == metadata.provider_kind.as_str()
                    && secret.endpoint == metadata.endpoint
                    && secret
                        .secret
                        .clone()
                        .into_backend_auth()
                        .suits_provider(metadata.provider_kind),
                "backend credential does not match metadata binding"
            );
            put_backend_credential_in_txn(txn, secret).await?;
        }
        (BackendAuthSelection::Credential { id, version }, None) => {
            let secret = backend_credential_in_txn(txn, &id).await?;
            anyhow::ensure!(
                secret.matches_binding(
                    &metadata.node_did,
                    metadata.provider_kind.as_str(),
                    &metadata.endpoint,
                    &version
                ) && secret
                    .secret
                    .clone()
                    .into_backend_auth()
                    .suits_provider(metadata.provider_kind),
                "backend credential does not match metadata binding"
            );
        }
        (_, Some(_)) => {
            anyhow::bail!("backend credential does not match metadata selection");
        }
        (_, None) => {}
    }

    let node = crate::store::graphql::escape_graphql_string(&metadata.node_did);
    let backend = crate::store::graphql::escape_graphql_string(&metadata.backend_id);
    let query = format!(
        r#"{{ InferenceBackendMetadata(filter: {{node_did: {{_eq: "{node}"}}, backend_id: {{_eq: "{backend}"}}}}, limit: 2) {{ _docID }} }}"#
    );
    let response = txn.execute(&query).await?;
    let rows =
        gents_protocol::graphql::graphql_rows_from_response(&response, "InferenceBackendMetadata");
    anyhow::ensure!(rows.len() <= 1, "ambiguous backend metadata identity");
    let mut input = serde_json::to_value(metadata)?;
    let fields = input
        .as_object_mut()
        .context("backend metadata must be an object")?;
    if fields
        .get("tags")
        .and_then(serde_json::Value::as_array)
        .is_some_and(Vec::is_empty)
    {
        fields.insert("tags".into(), serde_json::Value::Null);
    }
    if let Some(row) = rows.first() {
        let doc = row["_docID"]
            .as_str()
            .context("backend metadata has no document ID")?;
        fields.remove("node_did");
        fields.remove("backend_id");
        let escaped = crate::store::graphql::escape_graphql_string(doc);
        let mutation = format!(
            r#"mutation($input: InferenceBackendMetadataMutationInputArg!) {{ update_InferenceBackendMetadata(filter: {{_docID: {{_eq: "{escaped}"}}}}, input: $input) {{ _docID }} }}"#
        );
        let response = txn
            .execute_client_signed_update(
                &mutation,
                &serde_json::json!({"input": input}),
                gents_protocol::schemas::INFERENCE_BACKEND_METADATA,
            )
            .await?;
        let changed = gents_protocol::graphql::extract_mutation_doc_id(
            &response,
            "InferenceBackendMetadata",
        )?;
        anyhow::ensure!(
            changed == doc,
            "backend metadata update affected a different document"
        );
        Ok(changed)
    } else {
        input["creation_nonce"] = serde_json::Value::String(uuid::Uuid::new_v4().to_string());
        let response = txn.execute_client_signed_create(
            "mutation($input: InferenceBackendMetadataMutationInputArg!) { create_InferenceBackendMetadata(input: $input) { _docID } }",
            &serde_json::json!({"input": input}), gents_protocol::schemas::INFERENCE_BACKEND_METADATA,
        ).await?;
        let doc = gents_protocol::graphql::extract_mutation_doc_id(
            &response,
            "InferenceBackendMetadata",
        )?;
        crate::store::config_client::publish_node_child_in_txn(
            txn,
            "InferenceBackendMetadata",
            &doc,
            &metadata.node_did,
        )
        .await?;
        let observation_input = serde_json::json!({
            "node_did": metadata.node_did.as_str(), "backend_id": metadata.backend_id.as_str(),
            "metadata_doc_id": doc.as_str()
        });
        let observation = txn.execute_client_signed_create(
            "mutation($input: InferenceBackendObservationMutationInputArg!) { create_InferenceBackendObservation(input: $input) { _docID } }",
            &serde_json::json!({"input": observation_input}),
            gents_protocol::schemas::INFERENCE_BACKEND_OBSERVATION,
        ).await?;
        let observation_doc = gents_protocol::graphql::extract_mutation_doc_id(
            &observation,
            "InferenceBackendObservation",
        )?;
        crate::store::config_client::publish_node_child_in_txn(
            txn,
            "InferenceBackendObservation",
            &observation_doc,
            &metadata.node_did,
        )
        .await?;
        txn.add_actor_relationship(
            "InferenceBackendObservation",
            &observation_doc,
            "writer",
            &identity::Did::new(&metadata.node_did)?,
        )
        .await?;
        txn.add_entity_set_relationship(
            "InferenceBackendObservation",
            &observation_doc,
            "selected_by",
            "configuration",
            &doc,
            "selected_by",
        )
        .await?;
        Ok(doc)
    }
}

pub(crate) async fn write_inference_backend_in_txn(
    txn: &ConfigApplyTxn<'_>,
    backend: &InferenceBackend,
) -> Result<()> {
    backend.validate()?;
    let value = serde_json::to_value(backend)?;
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: Collection::InferenceBackend,
        add: value.clone(),
        update: value,
    }])?;
    crate::documents::apply_desired_state_plan(txn, &plan).await?;
    Ok(())
}

pub async fn write_inference_backend_document(
    access: &ConfigAccess,
    backend: &InferenceBackend,
) -> Result<String> {
    backend.validate()?;
    access
        .transact_write("config.inference_backend.upsert", |txn| {
            Box::pin(async move {
                write_inference_backend_in_txn(txn, backend).await?;
                crate::store::config_client::read_desired_state_record_in_txn(
                    txn,
                    Collection::InferenceBackend,
                    &backend.node_did,
                    &backend.backend_id,
                )
                .await?
                .map(|(id, _)| id)
                .context("replaced InferenceBackend missing")
            })
        })
        .await
}
