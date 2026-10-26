use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::provenance::ProvenanceError;
use super::{
    inference_slot_reference, PackInferenceBindings, PackInferenceSlot, PackManifest,
    INFERENCE_SLOT_REFERENCE_PREFIX,
};
use crate::config_client::{ConfigAccess, ConfigClientError, TransactionFailure};
use crate::document_config::{DocumentConfigError, InferenceBackend, InferenceProfile, PackConfig};
use crate::Collection;

type Result<T, E = InferenceError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum InferenceError {
    #[error(transparent)]
    ConfigClient(#[from] ConfigClientError),
    #[error(transparent)]
    DocumentConfig(Box<DocumentConfigError>),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Provenance(#[from] ProvenanceError),
    #[error(transparent)]
    Pack(#[from] crate::pack::PackError),
    #[error("profile {profile_id:?} cannot serve plugin slot {slot:?}")]
    PluginSlot {
        profile_id: String,
        slot: String,
        #[source]
        source: Box<crate::plugin::model_calls::ModelCallError>,
    },
    #[error("pack owner DID must not be blank")]
    NodeDidBlank,
    #[error("pack {pack} has no inference slot {slot:?}")]
    UnknownSlot { pack: String, slot: String },
    #[error(
        "inference slot {slot:?} references unknown profile {profile_id:?} for node {node_did}"
    )]
    UnknownProfile {
        slot: String,
        profile_id: String,
        node_did: String,
    },
    #[error("inference slot {slot:?} profile {profile_id:?} is unavailable: {reason}")]
    ProfileUnavailable {
        slot: String,
        profile_id: String,
        reason: String,
    },
    #[error("pack owner node is missing or disabled")]
    NodeDisabled,
    #[error(
        "pack {pack} requires configured inference, but node {node_did} has no usable profile; finish Setup or use the existing inference configuration tools first"
    )]
    NoUsableProfile { pack: String, node_did: String },
    #[error(
        "pack {pack} requires explicit inference slot bindings for: {missing}; inspect the preview and bind each slot to an existing profile"
    )]
    MissingBindings { pack: String, missing: String },
    #[error("pack agent does not reference an inference slot")]
    AgentNotSlotReference,
    #[error("inference slot {slot:?} is unbound")]
    UnboundSlot { slot: String },
    #[error("inference slot binding map does not exactly match the pack declaration")]
    BindingMapMismatch,
    #[error(
        "pack {pack} must bind existing inference profiles and cannot author inference configuration"
    )]
    AuthoredInference { pack: String },
    #[error("pack {pack} must assign every agent to exactly one inference slot")]
    AgentSlotCount { pack: String },
    #[error("agent {agent_id:?} is missing from inference slots")]
    AgentMissingSlot { agent_id: String },
    #[error("agent {agent_id:?} must reference inference slot {slot:?}")]
    AgentSlotReference { agent_id: String, slot: String },
}

impl From<DocumentConfigError> for InferenceError {
    fn from(error: DocumentConfigError) -> Self {
        Self::DocumentConfig(Box::new(error))
    }
}

impl TransactionFailure for InferenceError {
    fn config_client_error(&self) -> Option<&ConfigClientError> {
        match self {
            Self::ConfigClient(error) => Some(error),
            Self::DocumentConfig(error) => error.config_client_error(),
            Self::Provenance(error) => error.config_client_error(),
            Self::PluginSlot { source, .. } => source.config_client_error(),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct PackInferenceProfileOption {
    pub profile_id: String,
    pub display_name: Option<String>,
    pub model_name: String,
    pub reasoning_effort: Option<crate::config::ReasoningEffort>,
    pub backend_id: String,
    pub backend_name: Option<String>,
    pub provider_kind: Option<crate::backend_provider::BackendProviderKind>,
    pub usable: bool,
    pub unavailable_reason: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PackInferenceBindingPreview {
    pub slots: Vec<PackInferenceSlot>,
    pub profiles: Vec<PackInferenceProfileOption>,
    pub bindings: PackInferenceBindings,
    pub automatic: bool,
}

pub async fn inspect_pack_inference_bindings(
    access: &ConfigAccess,
    manifest: &PackManifest,
    node_did: &str,
    requested: &PackInferenceBindings,
) -> Result<PackInferenceBindingPreview> {
    super::validate_pack_manifest(manifest)?;
    if node_did.trim().is_empty() {
        return Err(InferenceError::NodeDidBlank);
    }

    let options = inference_profile_options(access, node_did).await?;

    let slots = manifest.metadata.inference_slots.clone();
    let slot_names = slots
        .iter()
        .map(|slot| slot.name.as_str())
        .collect::<BTreeSet<_>>();
    for slot in requested.keys() {
        if !slot_names.contains(slot.as_str()) {
            return Err(InferenceError::UnknownSlot {
                pack: manifest.name.clone(),
                slot: slot.clone(),
            });
        }
    }
    for (slot, profile_id) in requested {
        let profile = options
            .iter()
            .find(|profile| profile.profile_id == *profile_id)
            .ok_or_else(|| InferenceError::UnknownProfile {
                slot: slot.clone(),
                profile_id: profile_id.clone(),
                node_did: node_did.to_owned(),
            })?;
        if !profile.usable {
            return Err(InferenceError::ProfileUnavailable {
                slot: slot.clone(),
                profile_id: profile_id.clone(),
                reason: profile
                    .unavailable_reason
                    .as_deref()
                    .unwrap_or("unknown reason")
                    .to_owned(),
            });
        }
    }

    Ok(PackInferenceBindingPreview {
        slots,
        profiles: options,
        bindings: requested.clone(),
        automatic: false,
    })
}

pub async fn inference_profile_options(
    access: &ConfigAccess,
    node_did: &str,
) -> Result<Vec<PackInferenceProfileOption>> {
    let (node_enabled, profiles, backends) = access
        .transact_readonly("pack.inference_binding_preview", |txn| {
            Box::pin(async move {
                let refs = crate::ConfigReferences::load_in_txn(txn, node_did).await?;
                let node_enabled = refs
                    .documents()
                    .find(|((collection, _), _)| *collection == Collection::Node)
                    .and_then(|(_, value)| value.get("enabled"))
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                let profiles = refs
                    .documents()
                    .filter(|((collection, _), _)| *collection == Collection::InferenceProfile)
                    .map(|(_, value)| {
                        Ok(serde_json::from_value::<InferenceProfile>(value.clone())?)
                    })
                    .collect::<Result<Vec<_>>>()?;
                let backends = refs
                    .documents()
                    .filter(|((collection, _), _)| *collection == Collection::InferenceBackend)
                    .map(|(_, value)| {
                        Ok(serde_json::from_value::<InferenceBackend>(value.clone())?)
                    })
                    .collect::<Result<Vec<_>>>()?;
                Ok::<_, InferenceError>((node_enabled, profiles, backends))
            })
        })
        .await?;
    if !node_enabled {
        return Err(InferenceError::NodeDisabled);
    }

    let backend_by_id: BTreeMap<_, _> = backends
        .iter()
        .map(|backend| (backend.backend_id.as_str(), backend))
        .collect();
    let mut options = profiles
        .iter()
        .map(|profile| {
            let backend = backend_by_id.get(profile.backend_id.as_str()).copied();
            let unavailable_reason = match backend {
                None => Some("referenced backend is missing".to_owned()),
                Some(backend) if !backend.enabled => {
                    Some("referenced backend is disabled".to_owned())
                }
                Some(_) => None,
            };
            PackInferenceProfileOption {
                profile_id: profile.profile_id.clone(),
                display_name: profile.display_name.clone(),
                model_name: profile.model_name.clone(),
                reasoning_effort: profile.reasoning_effort,
                backend_id: profile.backend_id.clone(),
                backend_name: backend.map(|backend| backend.name.clone()),
                provider_kind: backend.map(|backend| backend.provider_kind),
                usable: unavailable_reason.is_none(),
                unavailable_reason,
            }
        })
        .collect::<Vec<_>>();
    options.sort_by(|left, right| left.profile_id.cmp(&right.profile_id));
    Ok(options)
}

pub async fn preview_pack_inference_bindings(
    access: &ConfigAccess,
    manifest: &PackManifest,
    node_did: &str,
    requested: &PackInferenceBindings,
) -> Result<PackInferenceBindingPreview> {
    let mut preview =
        inspect_pack_inference_bindings(access, manifest, node_did, requested).await?;
    use crate::plugin::model_calls::{AccessModels, ModelBinding, ModelResolver};
    let models = AccessModels(access);
    for slot in manifest
        .metadata
        .plugins
        .iter()
        .filter_map(|plugin| plugin.model_slot.as_ref())
    {
        if let Some(profile_id) = requested.get(slot) {
            models
                .resolve(&ModelBinding {
                    node_did: node_did.to_owned(),
                    profile_id: profile_id.clone(),
                })
                .await
                .map_err(|source| InferenceError::PluginSlot {
                    profile_id: profile_id.clone(),
                    slot: slot.clone(),
                    source: Box::new(source),
                })?;
        }
    }
    let required = preview
        .slots
        .iter()
        .filter(|slot| !slot.optional)
        .collect::<Vec<_>>();
    if required.is_empty() {
        return Ok(preview);
    }
    let usable = preview
        .profiles
        .iter()
        .filter(|profile| profile.usable)
        .collect::<Vec<_>>();
    if usable.is_empty() {
        return Err(InferenceError::NoUsableProfile {
            pack: manifest.name.clone(),
            node_did: node_did.to_owned(),
        });
    }
    if requested.is_empty() && required.len() == 1 && usable.len() == 1 {
        let slot = required[0].name.clone();
        preview.bindings.insert(slot, usable[0].profile_id.clone());
        preview.automatic = true;
        return Ok(preview);
    }
    let missing = required
        .iter()
        .filter(|slot| !requested.contains_key(&slot.name))
        .map(|slot| slot.name.clone())
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(InferenceError::MissingBindings {
            pack: manifest.name.clone(),
            missing: missing.join(", "),
        });
    }
    Ok(preview)
}

pub fn bind_pack_install_config(
    manifest: &PackManifest,
    config: &PackConfig,
    bindings: &PackInferenceBindings,
) -> Result<PackConfig> {
    validate_pack_inference_authoring(manifest, config)?;
    let mut bound = config.clone();
    for agent in &mut bound.agents {
        let slot = agent
            .inference_profile_id
            .strip_prefix(INFERENCE_SLOT_REFERENCE_PREFIX)
            .ok_or(InferenceError::AgentNotSlotReference)?;
        agent.inference_profile_id = bindings
            .get(slot)
            .ok_or_else(|| InferenceError::UnboundSlot {
                slot: slot.to_owned(),
            })?
            .clone();
    }
    let bindings_match = manifest
        .metadata
        .inference_slots
        .iter()
        .all(|slot| slot.optional || bindings.contains_key(&slot.name))
        && bindings.keys().all(|name| {
            manifest
                .metadata
                .inference_slots
                .iter()
                .any(|slot| slot.name == *name)
        });
    if !bindings_match {
        return Err(InferenceError::BindingMapMismatch);
    }
    Ok(super::provenance::stamp_pack_origin(manifest, &bound)?)
}

pub async fn install_pack_documents(
    access: &ConfigAccess,
    owner: &str,
    pack: &super::PackIdentity,
    config: &PackConfig,
    policy: super::DriftPolicy,
) -> Result<super::InstallReport, super::InstallationError> {
    install_pack_documents_with_expectations(access, owner, pack, config, policy, Vec::new()).await
}

/// Keep preconditions in the installation transaction; a separate preflight
/// can become stale before write admission.
pub async fn install_pack_documents_with_expectations(
    access: &ConfigAccess,
    owner: &str,
    pack: &super::PackIdentity,
    config: &PackConfig,
    policy: super::DriftPolicy,
    expected: Vec<crate::config_client::DesiredStateExpectation>,
) -> Result<super::InstallReport, super::InstallationError> {
    super::provenance::apply_pack_documents(access, owner, pack, config, policy, expected).await
}

pub(super) fn validate_pack_inference_authoring(
    manifest: &PackManifest,
    config: &PackConfig,
) -> Result<()> {
    if !(config.inference_backends.is_empty()
        && config.inference_profiles.is_empty()
        && config.inference_sampling.is_empty()
        && config.inference_execution.is_empty()
        && config.inference_retry_policies.is_empty())
    {
        return Err(InferenceError::AuthoredInference {
            pack: manifest.name.clone(),
        });
    }
    let declared = manifest
        .metadata
        .inference_slots
        .iter()
        .flat_map(|slot| {
            slot.agents
                .iter()
                .map(move |agent_id| (agent_id.as_str(), slot.name.as_str()))
        })
        .collect::<BTreeMap<_, _>>();
    if declared.len() != config.agents.len() {
        return Err(InferenceError::AgentSlotCount {
            pack: manifest.name.clone(),
        });
    }
    for agent in &config.agents {
        let slot = declared.get(agent.agent_id.as_str()).ok_or_else(|| {
            InferenceError::AgentMissingSlot {
                agent_id: agent.agent_id.clone(),
            }
        })?;
        if agent.inference_profile_id != inference_slot_reference(slot) {
            return Err(InferenceError::AgentSlotReference {
                agent_id: agent.agent_id.clone(),
                slot: slot.to_string(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::sync::Arc;

    fn manifest(slots: serde_json::Value) -> PackManifest {
        serde_json::from_value(json!({
            "manifest_version": 1,
            "name": "test_pack",
            "version": "1.0.0",
            "description": "test pack",
            "authors": ["tests"],
            "kind": "documents",
            "assets": ["README.md", "pack_config.json"],
            "dependencies": [],
            "config": "pack_config.json",
            "inference_slots": slots
        }))
        .unwrap()
    }

    fn two_slots() -> PackManifest {
        manifest(json!([
            {"name":"coordinator","description":"plans","agents":["plan"]},
            {"name":"worker","description":"scans","agents":["scan"]}
        ]))
    }

    #[tokio::test]
    async fn preview_auto_binds_only_the_unambiguous_one_to_one_case() {
        let owner = "did:key:preview-owner";
        let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
        crate::ensure_runtime_schemas(&node).await.unwrap();
        crate::document_config::ensure_node(&node, owner)
            .await
            .unwrap();
        crate::test_support::install_test_agent(&node, owner, "only").await;
        let access = ConfigAccess::local(node);
        let one = manifest(json!([
            {"name":"worker","description":"works","agents":["work"]}
        ]));
        let preview = preview_pack_inference_bindings(&access, &one, owner, &BTreeMap::new())
            .await
            .unwrap();
        assert!(preview.automatic);
        assert_eq!(preview.bindings["worker"], "only:inference");
        let error = preview_pack_inference_bindings(&access, &two_slots(), owner, &BTreeMap::new())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("coordinator, worker"));
    }

    #[tokio::test]
    async fn preview_accepts_shared_or_distinct_existing_profiles_and_rejects_foreign_ids() {
        let owner = "did:key:binding-owner";
        let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
        crate::ensure_runtime_schemas(&node).await.unwrap();
        crate::document_config::ensure_node(&node, owner)
            .await
            .unwrap();
        crate::test_support::install_test_agent(&node, owner, "claude").await;
        crate::test_support::install_test_agent(&node, owner, "glm").await;
        let foreign_owner = "did:key:foreign-owner";
        crate::document_config::ensure_node(&node, foreign_owner)
            .await
            .unwrap();
        crate::test_support::install_test_agent(&node, foreign_owner, "foreign").await;
        let access = ConfigAccess::local(node.clone());
        for requested in [
            BTreeMap::from([
                ("coordinator".into(), "claude:inference".into()),
                ("worker".into(), "claude:inference".into()),
            ]),
            BTreeMap::from([
                ("coordinator".into(), "claude:inference".into()),
                ("worker".into(), "glm:inference".into()),
            ]),
        ] {
            let preview = preview_pack_inference_bindings(&access, &two_slots(), owner, &requested)
                .await
                .unwrap();
            assert!(!preview.automatic);
            assert_eq!(preview.bindings, requested);
        }
        let foreign = BTreeMap::from([
            ("coordinator".into(), "claude:inference".into()),
            ("worker".into(), "foreign:inference".into()),
        ]);
        assert!(
            preview_pack_inference_bindings(&access, &two_slots(), owner, &foreign)
                .await
                .unwrap_err()
                .to_string()
                .contains("unknown profile")
        );
        let disabled = node
            .execute(
                r#"mutation { update_InferenceBackend(filter: {
                    node_did: {_eq: "did:key:binding-owner"},
                    backend_id: {_eq: "glm:backend"}
                }, input: {enabled: false}) {_docID} }"#,
            )
            .await;
        assert!(!disabled.has_errors(), "{:?}", disabled.errors);
        let disabled_binding = BTreeMap::from([
            ("coordinator".into(), "claude:inference".into()),
            ("worker".into(), "glm:inference".into()),
        ]);
        assert!(
            preview_pack_inference_bindings(&access, &two_slots(), owner, &disabled_binding)
                .await
                .unwrap_err()
                .to_string()
                .contains("disabled")
        );
    }
}
