use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use super::{
    inference_slot_reference, PackInferenceBindings, PackInferenceSlot, PackManifest,
    INFERENCE_SLOT_REFERENCE_PREFIX,
};
use gents_documents::{InferenceBackend, InferenceProfile, PackConfig};
use gents_store::collection::Collection;
use gents_store::config_client::ConfigAccess;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct PackInferenceProfileOption {
    pub profile_id: String,
    pub display_name: Option<String>,
    pub model_name: String,
    pub reasoning_effort: Option<gents_documents::inference_defaults::ReasoningEffort>,
    pub backend_id: String,
    pub backend_name: Option<String>,
    pub provider_kind: Option<gents_loop::backend_provider::BackendProviderKind>,
    pub usable: bool,
    pub unavailable_reason: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum SlotBindingError {
    /// More than one profile could serve these required slots, so the person installing picks:
    /// `profiles` are the usable ones.
    #[error(
        "pack {pack} requires explicit inference slot bindings for: {}; inspect the preview and bind each slot to an existing profile",
        .slots.join(", ")
    )]
    Unbound {
        pack: String,
        slots: Vec<String>,
        profiles: Vec<PackInferenceProfileOption>,
    },
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl SlotBindingError {
    /// `Other` erases to its own error, as `PackChangeError::into_anyhow` says why.
    pub fn into_anyhow(self) -> anyhow::Error {
        match self {
            Self::Other(error) => error,
            unbound => unbound.into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct PackInferenceBindingPreview {
    pub slots: Vec<PackInferenceSlot>,
    pub profiles: Vec<PackInferenceProfileOption>,
    pub bindings: PackInferenceBindings,
    pub automatic: bool,
}

/// The pack's slots and the owner's profiles, checking only the bindings
/// requested: unlike [`preview_pack_inference_bindings`], it does not refuse
/// required slots left unbound, so an install preview can list them.
pub async fn inspect_pack_inference_bindings(
    access: &ConfigAccess,
    manifest: &PackManifest,
    node_did: &str,
    requested: &PackInferenceBindings,
) -> Result<PackInferenceBindingPreview> {
    inspect_pack_inference_bindings_with_node_binding(access, manifest, node_did, requested, None)
        .await
}

pub(crate) async fn inspect_pack_inference_bindings_with_node_binding(
    access: &ConfigAccess,
    manifest: &PackManifest,
    node_did: &str,
    requested: &PackInferenceBindings,
    node_binding: Option<&gents_documents::ConfigurationNodeBinding>,
) -> Result<PackInferenceBindingPreview> {
    super::validate_pack_manifest(manifest)?;
    anyhow::ensure!(
        !node_did.trim().is_empty(),
        "pack owner DID must not be blank"
    );

    let options =
        inference_profile_options_with_node_binding(access, node_did, node_binding).await?;

    let slots = manifest.metadata.inference_slots.clone();
    let slot_names = slots
        .iter()
        .map(|slot| slot.name.as_str())
        .collect::<BTreeSet<_>>();
    for slot in requested.keys() {
        anyhow::ensure!(
            slot_names.contains(slot.as_str()),
            "pack {} has no inference slot {slot:?}",
            manifest.name
        );
    }
    for (slot, profile_id) in requested {
        let profile = options
            .iter()
            .find(|profile| profile.profile_id == *profile_id)
            .with_context(|| {
                format!(
                    "inference slot {slot:?} references unknown profile {profile_id:?} for node {node_did}"
                )
            })?;
        anyhow::ensure!(
            crate::pack::core_packs::slot_binding(crate::pack::core_packs::ProfileState::of(
                profile
            ))
            .installs(),
            "inference slot {slot:?} profile {profile_id:?} is unavailable: {}",
            profile
                .unavailable_reason
                .as_deref()
                .unwrap_or("unknown reason")
        );
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
    inference_profile_options_with_node_binding(access, node_did, None).await
}

async fn inference_profile_options_with_node_binding(
    access: &ConfigAccess,
    node_did: &str,
    node_binding: Option<&gents_documents::ConfigurationNodeBinding>,
) -> Result<Vec<PackInferenceProfileOption>> {
    let (node_enabled, profiles, backends) = access
        .transact_readonly("pack.inference_binding_preview", |txn| {
            Box::pin(async move {
                let refs = gents_documents::ConfigReferences::load_in_txn(txn, node_did).await?;
                let node_enabled = if let Some(binding) = node_binding {
                    gents_documents::configuration_node_doc(txn, node_did, Some(binding)).await?;
                    binding.enabled
                } else {
                    refs.documents()
                        .find(|((collection, _), _)| *collection == Collection::Node)
                        .and_then(|(_, value)| value.get("enabled"))
                        .and_then(serde_json::Value::as_bool)
                        .unwrap_or(false)
                };
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
                Ok::<_, anyhow::Error>((node_enabled, profiles, backends))
            })
        })
        .await?;
    anyhow::ensure!(node_enabled, "pack owner node is missing or disabled");

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
) -> Result<PackInferenceBindingPreview, SlotBindingError> {
    preview_pack_inference_bindings_with_node_binding(access, manifest, node_did, requested, None)
        .await
}

pub(crate) async fn preview_pack_inference_bindings_with_node_binding(
    access: &ConfigAccess,
    manifest: &PackManifest,
    node_did: &str,
    requested: &PackInferenceBindings,
    node_binding: Option<&gents_documents::ConfigurationNodeBinding>,
) -> Result<PackInferenceBindingPreview, SlotBindingError> {
    let mut preview = inspect_pack_inference_bindings_with_node_binding(
        access,
        manifest,
        node_did,
        requested,
        node_binding,
    )
    .await?;
    use crate::plugin::model_binding::{AccessModels, ModelBinding, ModelResolver};
    let models = AccessModels(access);
    for slot in manifest
        .metadata
        .plugins
        .iter()
        .filter_map(|plugin| plugin.model_slot.as_ref())
    {
        if let Some(profile_id) = requested.get(slot) {
            models
                .resolve(
                    &ModelBinding {
                        node_did: node_did.to_owned(),
                        profile_id: profile_id.clone(),
                    },
                    None,
                )
                .await
                .with_context(|| {
                    format!("profile {profile_id:?} cannot serve plugin slot {slot:?}")
                })?;
        }
    }
    let required = preview
        .slots
        .iter()
        .filter(|slot| !slot.optional)
        .collect::<Vec<_>>();
    if required
        .iter()
        .all(|slot| requested.contains_key(&slot.name))
    {
        return Ok(preview);
    }
    let usable = preview
        .profiles
        .iter()
        .filter(|profile| profile.usable)
        .collect::<Vec<_>>();
    if usable.is_empty() {
        return Err(anyhow::anyhow!(
            "pack {} requires configured inference, but node {node_did} has no usable profile; finish Setup or use the existing inference configuration tools first",
            manifest.name
        )
        .into());
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
        return Err(SlotBindingError::Unbound {
            pack: manifest.name.clone(),
            slots: missing,
            profiles: usable.into_iter().cloned().collect(),
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
            .context("pack agent does not reference an inference slot")?;
        agent.inference_profile_id = bindings
            .get(slot)
            .with_context(|| format!("inference slot {slot:?} is unbound"))?
            .clone();
    }
    anyhow::ensure!(
        manifest
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
            }),
        "inference slot binding map does not exactly match the pack declaration"
    );
    super::provenance::stamp_pack_origin(manifest, &bound)
}

pub async fn install_pack_documents(
    access: &ConfigAccess,
    owner: &str,
    pack: &super::PackIdentity,
    config: &PackConfig,
    policy: super::DriftPolicy,
    profile_policy: &'static dyn gents_documents::inference_backend::AdvertisedProfilePolicy,
) -> Result<super::InstallReport> {
    install_pack_documents_with_expectations(
        access,
        owner,
        pack,
        config,
        policy,
        Vec::new(),
        profile_policy,
    )
    .await
}

/// Keep preconditions in the installation transaction; a separate preflight
/// can become stale before write admission.
pub async fn install_pack_documents_with_expectations(
    access: &ConfigAccess,
    owner: &str,
    pack: &super::PackIdentity,
    config: &PackConfig,
    policy: super::DriftPolicy,
    expected: Vec<gents_documents::DesiredStateExpectation>,
    profile_policy: &'static dyn gents_documents::inference_backend::AdvertisedProfilePolicy,
) -> Result<super::InstallReport> {
    super::provenance::apply_pack_documents(
        access,
        owner,
        pack,
        config,
        policy,
        expected,
        profile_policy,
    )
    .await
}

pub(super) fn validate_pack_inference_authoring(
    manifest: &PackManifest,
    config: &PackConfig,
) -> Result<()> {
    anyhow::ensure!(
        config.inference_backends.is_empty()
            && config.inference_profiles.is_empty()
            && config.inference_sampling.is_empty()
            && config.inference_execution.is_empty()
            && config.inference_retry_policies.is_empty(),
        "pack {} must bind existing inference profiles and cannot author inference configuration",
        manifest.name
    );
    let declared = manifest
        .metadata
        .inference_slots
        .iter()
        .flat_map(|slot| {
            slot.agents
                .iter()
                .map(move |agent_did| (agent_did.as_str(), slot.name.as_str()))
        })
        .collect::<BTreeMap<_, _>>();
    anyhow::ensure!(
        declared.len() == config.agents.len(),
        "pack {} must assign every agent to exactly one inference slot",
        manifest.name
    );
    for agent in &config.agents {
        let slot = declared.get(agent.agent_did.as_str()).with_context(|| {
            format!(
                "agent {:?} is missing from inference slots",
                agent.agent_did
            )
        })?;
        anyhow::ensure!(
            agent.inference_profile_id == inference_slot_reference(slot),
            "agent {:?} must reference inference slot {:?}",
            agent.agent_did,
            slot
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
        let (_directory, runtime, node, access) =
            super::super::agent_bindings::tests::fixture().await;
        let owner = gents_store::identity::NodeIdentity::did(&runtime);
        let one = manifest(json!([
            {"name":"worker","description":"works","agents":["work"]}
        ]));
        let preview = preview_pack_inference_bindings(&access, &one, owner, &BTreeMap::new())
            .await
            .unwrap();
        assert!(preview.automatic);
        assert_eq!(preview.bindings["worker"], "profile");
        let error = preview_pack_inference_bindings(&access, &two_slots(), owner, &BTreeMap::new())
            .await
            .unwrap_err();
        assert!(error.to_string().contains("coordinator, worker"));
        let SlotBindingError::Unbound {
            pack,
            slots,
            profiles,
        } = error
        else {
            panic!("an ambiguous pick is its own refusal: {error:#}");
        };
        assert_eq!(pack, "test_pack");
        assert_eq!(slots, ["coordinator", "worker"]);
        let profiles = profiles.iter().map(|profile| profile.profile_id.as_str());
        assert_eq!(profiles.collect::<Vec<_>>(), ["profile"]);
        node.shutdown().await.expect("node shutdown");
    }

    #[tokio::test]
    async fn preview_accepts_shared_or_distinct_existing_profiles_and_rejects_foreign_ids() {
        let (directory, runtime, node, access) =
            super::super::agent_bindings::tests::fixture().await;
        let owner = gents_store::identity::NodeIdentity::did(&runtime);
        let foreign = gents_store::identity::KeyIdentity::load_or_create(
            directory.path().join("foreign"),
            None,
        )
        .unwrap();
        let foreign_owner = gents_store::identity::NodeIdentity::did(&foreign);
        let backend = |owner: &str, id: &str, enabled: bool| {
            json!({
                "node_did":owner,"backend_id":id,"name":"Local","provider_kind":"OpenAiCompatible",
                "endpoint":"http://127.0.0.1:8000/v1","auth":{"kind":"unauthenticated"},"enabled":enabled
            })
        };
        let docs = [
            (
                gents_store::collection::Collection::InferenceBackend,
                backend(owner, "glm:backend", true),
            ),
            (
                gents_store::collection::Collection::InferenceProfile,
                json!({"node_did":owner,"profile_id":"glm:inference","backend_id":"glm:backend","model_name":"glm"}),
            ),
            (
                gents_store::collection::Collection::Node,
                json!({"node_did":foreign_owner}),
            ),
            (
                gents_store::collection::Collection::InferenceBackend,
                backend(foreign_owner, "foreign:backend", true),
            ),
            (
                gents_store::collection::Collection::InferenceProfile,
                json!({"node_did":foreign_owner,"profile_id":"foreign:inference","backend_id":"foreign:backend","model_name":"foreign"}),
            ),
        ];
        let plan = gents_documents::DesiredStateApplyPlan::new(
            docs.into_iter()
                .map(
                    |(collection, value)| gents_documents::DesiredStateApplyDocument {
                        collection,
                        add: value.clone(),
                        update: value,
                    },
                )
                .collect(),
        )
        .unwrap();
        access
            .transact_write("test.inference.preview", |txn| {
                let plan = &plan;
                Box::pin(async move {
                    gents_documents::apply_desired_state_plan(
                        txn,
                        plan,
                        &gents_inference::resolved::ProviderProfilePolicy,
                    )
                    .await
                    .map(|_| ())
                })
            })
            .await
            .unwrap();
        for requested in [
            BTreeMap::from([
                ("coordinator".into(), "profile".into()),
                ("worker".into(), "profile".into()),
            ]),
            BTreeMap::from([
                ("coordinator".into(), "profile".into()),
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
            ("coordinator".into(), "profile".into()),
            ("worker".into(), "foreign:inference".into()),
        ]);
        assert!(
            preview_pack_inference_bindings(&access, &two_slots(), owner, &foreign)
                .await
                .unwrap_err()
                .to_string()
                .contains("unknown profile")
        );
        let disabled = backend(owner, "glm:backend", false);
        let plan = gents_documents::DesiredStateApplyPlan::new(vec![
            gents_documents::DesiredStateApplyDocument {
                collection: gents_store::collection::Collection::InferenceBackend,
                add: disabled.clone(),
                update: disabled,
            },
        ])
        .unwrap();
        access
            .transact_write("test.inference.disable", |txn| {
                let plan = &plan;
                Box::pin(async move {
                    gents_documents::apply_desired_state_plan(
                        txn,
                        plan,
                        &gents_inference::resolved::ProviderProfilePolicy,
                    )
                    .await
                    .map(|_| ())
                })
            })
            .await
            .unwrap();
        let disabled_binding = BTreeMap::from([
            ("coordinator".into(), "profile".into()),
            ("worker".into(), "glm:inference".into()),
        ]);
        assert!(
            preview_pack_inference_bindings(&access, &two_slots(), owner, &disabled_binding)
                .await
                .unwrap_err()
                .to_string()
                .contains("disabled")
        );
        node.shutdown().await.expect("node shutdown");
    }
}
