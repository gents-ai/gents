use anyhow::Context;
use gents_documents::{
    load_inference_backend_in_txn, AdvertisedModel, InferenceBackend, InferenceBackendObservation,
};
use gents_protocol::operations::inference::{
    DiscoveryAnswer, InferenceActor, InferenceDocument, InferenceOperationError,
};
use gents_store::config_client::ConfigAccess;

use super::decision::{acting_for_authenticated_request, acting_on, discovery_answer};
use super::OperationFailure;
use crate::backends::registry::{
    discover_backend_for_actor, discover_shared_backend, lookup_backend_observation_in_txn,
};

/// Model discovery's answer for one stored backend.
#[derive(Debug, Clone)]
pub struct Discovery {
    pub backend: InferenceBackend,
    pub answer: DiscoveryAnswer,
    /// What the provider advertised, when it was asked.
    pub models: Option<Vec<AdvertisedModel>>,
    pub observation: Option<InferenceBackendObservation>,
}

/// A stored backend and what discovery will do for the acting actor.
#[derive(Debug, Clone)]
pub struct DiscoveryTarget {
    actor: InferenceActor,
    node_did: String,
    backend: InferenceBackend,
    stored: Option<InferenceBackendObservation>,
}

/// Lean `InferenceOperations.discoveryAnswer`: a person's discovery asks the
/// provider with the backend's stored authentication and records the catalog;
/// an Agent's asks only an unauthenticated local server and otherwise answers
/// with the stored observation.
pub async fn discovery_target(
    access: &ConfigAccess,
    node_did: &str,
    backend_id: &str,
) -> Result<DiscoveryTarget, InferenceOperationError> {
    let actor = acting_on(access, node_did)
        .await
        .map_err(OperationFailure::from)?;
    target_for(access, node_did, backend_id, actor).await
}

async fn target_for(
    access: &ConfigAccess,
    node_did: &str,
    backend_id: &str,
    actor: InferenceActor,
) -> Result<DiscoveryTarget, InferenceOperationError> {
    let (backend, stored) = access
        .transact_readonly("inference.backend.discovery_target", |txn| {
            Box::pin(async move {
                let backend = load_inference_backend_in_txn(txn, node_did, backend_id)
                    .await?
                    .ok_or_else(|| InferenceOperationError::Missing {
                        document: InferenceDocument::Backend,
                        id: backend_id.to_owned(),
                    })?;
                let observation =
                    lookup_backend_observation_in_txn(txn, node_did, backend_id).await?;
                Ok::<_, OperationFailure>((backend, observation))
            })
        })
        .await?;
    Ok(DiscoveryTarget {
        actor,
        node_did: node_did.to_owned(),
        backend,
        stored,
    })
}

/// Discovery for the caller of a runtime route, on the embedded access the
/// route opened as that caller; `acting_for_authenticated_request` states
/// what the route must have verified.
pub async fn discover_for_authenticated_request(
    access: &ConfigAccess,
    node_did: &str,
    backend_id: &str,
) -> Result<Discovery, InferenceOperationError> {
    let actor = acting_for_authenticated_request(access, node_did)
        .await
        .map_err(OperationFailure::from)?;
    target_for(access, node_did, backend_id, actor)
        .await?
        .resolve(access)
        .await
}

impl DiscoveryTarget {
    pub fn answer(&self) -> DiscoveryAnswer {
        discovery_answer(self.actor, &self.backend)
    }

    /// `access` is the runtime's own node: a remote client reaches this
    /// through the runtime's discovery route.
    pub async fn resolve(
        self,
        access: &ConfigAccess,
    ) -> Result<Discovery, InferenceOperationError> {
        let answer = self.answer();
        let Self {
            actor,
            node_did,
            backend,
            stored,
        } = self;
        if answer == DiscoveryAnswer::StoredObservation {
            return Ok(Discovery {
                backend,
                answer,
                models: None,
                observation: stored,
            });
        }
        let node = access
            .local_node()
            .context("model discovery runs on the runtime that hosts the backend")
            .map_err(OperationFailure::from)?;
        let (models, observation) = match actor {
            InferenceActor::Agent => {
                let observation = discover_shared_backend(node, &backend)
                    .await
                    .map_err(OperationFailure::from)?;
                (None, Some(observation))
            }
            InferenceActor::Person => {
                let actor = access
                    .actor_did()
                    .context("model discovery requires an authenticated actor")
                    .and_then(|did| Ok(identity::Did::new(&did)?))
                    .map_err(OperationFailure::from)?;
                let models =
                    discover_backend_for_actor(node, &actor, &node_did, &backend.backend_id)
                        .await
                        .map_err(|error| OperationFailure::Other(error.into()))?;
                (Some(models), stored)
            }
        };
        Ok(Discovery {
            backend,
            answer,
            models,
            observation,
        })
    }
}

pub async fn discover(
    access: &ConfigAccess,
    node_did: &str,
    backend_id: &str,
) -> Result<Discovery, InferenceOperationError> {
    discovery_target(access, node_did, backend_id)
        .await?
        .resolve(access)
        .await
}
