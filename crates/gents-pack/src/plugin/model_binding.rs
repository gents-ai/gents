use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use futures::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use gents_documents::{InferenceBackend, InferenceProfile};
use gents_loop::openai_wire::OpenAiWireApi;
use gents_store::collection::Collection;
use gents_store::config_client::ConfigAccess;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct ModelBinding {
    pub node_did: String,
    pub profile_id: String,
}

const DEFAULT_CONNECT_TIMEOUT_SECS: i64 = 10;
/// Bound one host-side model exchange independently of the guest's remaining
/// wall-clock budget, so a stalled endpoint cannot occupy its permit for the
/// entire guest lifetime.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

pub struct ModelEndpoint {
    pub url: String,
    pub api_key: Option<String>,
    pub model: String,
    pub max_concurrent: usize,
    pub backend_key: String,
    pub connect_timeout: Duration,
    pub request_timeout: Duration,
    pub max_output_tokens: Option<u64>,
    pub profile_visibility: Option<ModelProfileVisibility>,
}

/// Retains the exact selected profile and caller for native checks before
/// each plugin model request; it contains no credential material.
pub struct ModelProfileVisibility {
    pub access: ConfigAccess,
    pub owner: String,
    pub profile_id: String,
    pub profile_doc: String,
    pub profile_value: Value,
    pub backend_id: String,
    pub backend_doc: String,
    pub backend_value: Value,
}

impl std::fmt::Debug for ModelEndpoint {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ModelEndpoint")
            .field("model", &self.model)
            .field("max_concurrent", &self.max_concurrent)
            .finish_non_exhaustive()
    }
}

pub trait ModelResolver: Send + Sync {
    fn resolve<'a>(
        &'a self,
        binding: &'a ModelBinding,
        actor: Option<&'a identity::Did>,
    ) -> BoxFuture<'a, Result<ModelEndpoint>>;
}

pub struct AccessModels<A>(pub A);

impl<A> ModelResolver for AccessModels<A>
where
    A: std::borrow::Borrow<ConfigAccess> + Send + Sync,
{
    fn resolve<'a>(
        &'a self,
        binding: &'a ModelBinding,
        actor: Option<&'a identity::Did>,
    ) -> BoxFuture<'a, Result<ModelEndpoint>> {
        Box::pin(async move {
            let owner = binding.node_did.as_str();
            let profile_id = binding.profile_id.as_str();
            let access = self.0.borrow();
            if let (Some(configured), Some(actor)) = (access.actor_did(), actor) {
                anyhow::ensure!(
                    configured == actor.as_str(),
                    "plugin model caller does not match access"
                );
            }
            let profile_access = match (access.local_node(), actor) {
                (Some(node), Some(actor)) => {
                    ConfigAccess::local_as(Arc::clone(node), actor.clone())
                }
                (_, None) if access.actor_did().is_some() => access.clone(),
                (None, Some(_)) if access.actor_did().is_some() => access.clone(),
                _ => anyhow::bail!("plugin model requires an authenticated caller"),
            };
            let (profile_doc, profile_value, profile, backend_doc, backend_value, backend) =
                profile_access
                    .transact_readonly("plugin.model_profile", |txn| {
                        Box::pin(async move {
                            let (profile_doc, profile_value) =
                                gents_documents::read_desired_state_record_in_txn(
                                    txn,
                                    Collection::InferenceProfile,
                                    owner,
                                    profile_id,
                                )
                                .await?
                                .with_context(|| "the selected inference profile is unavailable")?;
                            let profile: InferenceProfile =
                                serde_json::from_value(profile_value.clone())?;
                            let (backend_doc, backend_value) =
                                gents_documents::read_desired_state_record_in_txn(
                                    txn,
                                    Collection::InferenceBackend,
                                    owner,
                                    &profile.backend_id,
                                )
                                .await?
                                .with_context(|| {
                                    format!(
                                        "the backend of profile {profile_id:?} no longer exists"
                                    )
                                })?;
                            let backend: InferenceBackend =
                                serde_json::from_value(backend_value.clone())?;
                            Ok::<_, anyhow::Error>((
                                profile_doc,
                                profile_value,
                                profile,
                                backend_doc,
                                backend_value,
                                backend,
                            ))
                        })
                    })
                    .await?;
            let mut endpoint = endpoint_for_selected(&profile, &backend, true)?;
            if matches!(
                &backend.auth,
                gents_documents::BackendAuth::Credential { .. }
            ) {
                anyhow::ensure!(
                    profile_access.local_node().is_some(),
                    "runtime credential resolver unavailable"
                );
            }
            endpoint.profile_visibility = Some(ModelProfileVisibility {
                access: profile_access,
                owner: owner.to_owned(),
                profile_id: profile_id.to_owned(),
                profile_doc,
                profile_value,
                backend_id: profile.backend_id.clone(),
                backend_doc,
                backend_value,
            });
            Ok(endpoint)
        })
    }
}

pub fn endpoint_for(
    profile: &InferenceProfile,
    backend: &InferenceBackend,
) -> Result<ModelEndpoint> {
    endpoint_for_selected(profile, backend, false)
}

fn endpoint_for_selected(
    profile: &InferenceProfile,
    backend: &InferenceBackend,
    allow_runtime_credential: bool,
) -> Result<ModelEndpoint> {
    use gents_loop::backend_provider::BackendProviderKind::{OpenAiCompatible, OpenRouter};
    anyhow::ensure!(
        backend.enabled,
        "the backend of profile {:?} is disabled",
        profile.profile_id
    );
    anyhow::ensure!(
        matches!(backend.provider_kind, OpenAiCompatible | OpenRouter)
            && OpenAiWireApi::effective_for_provider(
                backend.provider_kind,
                backend.openai_wire_api
            ) == OpenAiWireApi::ChatCompletions,
        "profile {:?} needs an OpenAI-compatible backend on the chat completions wire",
        profile.profile_id
    );
    // Cloud credentials resolve to no key because they sign or mint per
    // request; on this chat-completions path they would send unauthenticated.
    anyhow::ensure!(
        backend.auth.suits_provider(backend.provider_kind),
        "the backend of profile {:?} has credentials its provider cannot use",
        profile.profile_id
    );
    let api_key = if matches!(
        &backend.auth,
        gents_documents::BackendAuth::Credential { .. }
    ) {
        anyhow::ensure!(
            allow_runtime_credential,
            "runtime credential resolver required"
        );
        None
    } else {
        backend.auth.resolve_api_key()?
    };
    let max_concurrent = usize::try_from(backend.effective_max_concurrent())
        .ok()
        .filter(|limit| *limit > 0)
        .with_context(|| {
            format!(
                "the backend of profile {:?} allows no requests",
                profile.profile_id
            )
        })?;
    let connect = backend
        .connect_timeout_secs
        .filter(|secs| *secs > 0)
        .unwrap_or(DEFAULT_CONNECT_TIMEOUT_SECS);
    Ok(ModelEndpoint {
        url: format!(
            "{}/chat/completions",
            backend.endpoint.trim_end_matches('/')
        ),
        api_key,
        model: profile.model_name.clone(),
        max_concurrent,
        backend_key: format!("{}/{}", backend.node_did, backend.backend_id),
        connect_timeout: Duration::from_secs(connect.unsigned_abs()),
        request_timeout: REQUEST_TIMEOUT,
        max_output_tokens: profile
            .max_output_tokens
            .and_then(|tokens| u64::try_from(tokens).ok()),
        profile_visibility: None,
    })
}
