use anyhow::{Context, Result};

use super::refused;
use crate::config_client::{
    desired_state_document_digest, read_desired_state_record_in_txn, ConfigAccess,
};
use crate::document_config::{
    BackendAuth, EvalDefinition, InferenceBackend, InferenceExecution, InferenceProfile,
    InferenceRetryPolicy, InferenceSampling,
};
use crate::eval::runner::executor::InferenceBinding;
use crate::eval::DefinitionRef;
use crate::Collection;

pub(super) async fn read_document<T>(
    access: &ConfigAccess,
    collection: Collection,
    owner: &str,
    id: &str,
) -> Result<Option<T>>
where
    T: serde::de::DeserializeOwned,
{
    let found = access
        .transact_readonly("eval.freeze.read_configuration", |txn| {
            Box::pin(
                async move { read_desired_state_record_in_txn(txn, collection, owner, id).await },
            )
        })
        .await?;
    found
        .map(|(_, value)| {
            serde_json::from_value(value)
                .with_context(|| format!("decoding {} {id:?}", collection.graphql_type()))
        })
        .transpose()
}

pub(crate) async fn load_definition(
    access: &ConfigAccess,
    owner: &str,
    definition_id: &str,
) -> Result<EvalDefinition> {
    let definition: EvalDefinition =
        read_document(access, Collection::EvalDefinition, owner, definition_id)
            .await?
            .ok_or_else(|| refused(format!("no eval definition {definition_id:?} for {owner}")))?;
    definition.validate().map_err(|error| {
        refused(format!(
            "eval definition {:?}: {error:#}",
            definition.definition_id
        ))
    })?;
    Ok(definition)
}

pub(crate) fn definition_ref(definition: &EvalDefinition) -> Result<DefinitionRef> {
    Ok(DefinitionRef {
        definition_id: definition.definition_id.clone(),
        comparability_version: definition.comparability_version,
        digest: desired_state_document_digest(&serde_json::to_value(definition)?)?,
    })
}

pub(super) async fn inference_binding(
    access: &ConfigAccess,
    owner: &str,
    cell_id: &str,
    inference_profile_id: &str,
) -> Result<InferenceBinding> {
    let profile: InferenceProfile = read_document(
        access,
        Collection::InferenceProfile,
        owner,
        inference_profile_id,
    )
    .await?
    .ok_or_else(|| {
        refused(format!(
            "cell {cell_id:?} names no inference profile {inference_profile_id:?}"
        ))
    })?;
    let backend: InferenceBackend = read_document(
        access,
        Collection::InferenceBackend,
        owner,
        &profile.backend_id,
    )
    .await?
    .ok_or_else(|| {
        refused(format!(
            "inference profile {:?} names no backend {:?}",
            profile.profile_id, profile.backend_id
        ))
    })?;
    let sampling: Option<InferenceSampling> = match &profile.sampling_id {
        Some(sampling_id) => Some(
            read_document(access, Collection::InferenceSampling, owner, sampling_id)
                .await?
                .ok_or_else(|| {
                    refused(format!(
                        "inference profile {:?} names no sampling document {sampling_id:?}",
                        profile.profile_id
                    ))
                })?,
        ),
        None => None,
    };
    let execution: Option<InferenceExecution> = match &profile.execution_id {
        Some(execution_id) => Some(
            read_document(access, Collection::InferenceExecution, owner, execution_id)
                .await?
                .ok_or_else(|| {
                    refused(format!(
                        "inference profile {:?} names no execution {execution_id:?}",
                        profile.profile_id
                    ))
                })?,
        ),
        None => None,
    };
    let retry_policy: Option<InferenceRetryPolicy> =
        match execution.as_ref().and_then(|e| e.retry_policy_id.as_ref()) {
            Some(retry_policy_id) => Some(
                read_document(
                    access,
                    Collection::InferenceRetryPolicy,
                    owner,
                    retry_policy_id,
                )
                .await?
                .ok_or_else(|| {
                    refused(format!(
                        "inference profile {:?} names no retry policy {retry_policy_id:?}",
                        profile.profile_id
                    ))
                })?,
            ),
            None => None,
        };

    if matches!(backend.auth, BackendAuth::NodeOAuth { .. }) {
        return Err(refused(format!(
            "backend {:?} authenticates with node_oauth; an eval run must not \
             spend the launching principal's subscription credential",
            backend.backend_id
        )));
    }

    Ok(InferenceBinding {
        profile: serde_json::to_value(&profile)?,
        backend: serde_json::to_value(&backend)?,
        sampling: sampling.as_ref().map(serde_json::to_value).transpose()?,
        execution: execution.as_ref().map(serde_json::to_value).transpose()?,
        retry_policy: retry_policy
            .as_ref()
            .map(serde_json::to_value)
            .transpose()?,
        seed: 0,
    })
}
