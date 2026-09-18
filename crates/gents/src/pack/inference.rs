use std::collections::{BTreeMap, BTreeSet};

use anyhow::{Context, Result};
use serde::Serialize;

use super::{
    inference_slot_reference, PackInferenceBindings, PackInferenceSlot, PackManifest,
    INFERENCE_SLOT_REFERENCE_PREFIX,
};
use crate::config_client::ConfigAccess;
use crate::document_config::{InferenceBackend, InferenceProfile, PackConfig};
use crate::Collection;

#[derive(Clone, Debug, PartialEq, Serialize)]
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

/// Inspect declared slots and the principal's retained inference documents.
/// Requested bindings are validated when present, while missing slots remain
/// visible so a model can discover the exact choices before constructing an
/// install preview.
pub async fn inspect_pack_inference_bindings(
    access: &ConfigAccess,
    manifest: &PackManifest,
    agent_did: &str,
    requested: &PackInferenceBindings,
) -> Result<PackInferenceBindingPreview> {
    super::validate_pack_manifest(manifest)?;
    anyhow::ensure!(
        !agent_did.trim().is_empty(),
        "pack owner DID must not be blank"
    );

    let (principal_enabled, profiles, backends) = access
        .transact("pack.inference_binding_preview", |txn| {
            Box::pin(async move {
                let refs = crate::ConfigReferences::load_in_txn(txn, agent_did).await?;
                let principal_enabled = refs
                    .documents()
                    .find(|((collection, _), _)| *collection == Collection::AgentPrincipal)
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
                Ok((principal_enabled, profiles, backends))
            })
        })
        .await?;
    anyhow::ensure!(
        principal_enabled,
        "pack owner principal is missing or disabled"
    );

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
                    "inference slot {slot:?} references unknown profile {profile_id:?} for principal {agent_did}"
                )
            })?;
        anyhow::ensure!(
            profile.usable,
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

/// Resolve every declared slot against the principal's retained inference
/// documents. This is read-only and is the strict preview used immediately
/// before an authorized canonical installer transaction.
pub async fn preview_pack_inference_bindings(
    access: &ConfigAccess,
    manifest: &PackManifest,
    agent_did: &str,
    requested: &PackInferenceBindings,
) -> Result<PackInferenceBindingPreview> {
    let mut preview =
        inspect_pack_inference_bindings(access, manifest, agent_did, requested).await?;
    if preview.slots.is_empty() {
        return Ok(preview);
    }
    let usable = preview
        .profiles
        .iter()
        .filter(|profile| profile.usable)
        .collect::<Vec<_>>();
    anyhow::ensure!(
        !usable.is_empty(),
        "pack {} requires configured inference, but principal {agent_did} has no usable profile; finish Setup or use the existing inference configuration tools first",
        manifest.name
    );
    if requested.is_empty() && preview.slots.len() == 1 && usable.len() == 1 {
        preview
            .bindings
            .insert(preview.slots[0].name.clone(), usable[0].profile_id.clone());
        preview.automatic = true;
        return Ok(preview);
    }
    let missing = preview
        .slots
        .iter()
        .filter(|slot| !requested.contains_key(&slot.name))
        .map(|slot| slot.name.clone())
        .collect::<Vec<_>>();
    anyhow::ensure!(
        missing.is_empty(),
        "pack {} requires explicit inference slot bindings for: {}; inspect the preview and bind each slot to an existing profile",
        manifest.name,
        missing.join(", ")
    );
    Ok(preview)
}

/// Bind authored slot markers and stamp provenance on every pack-authored
/// configuration type that owns tags. User inference documents are rejected,
/// so referenced profiles/backends can never acquire pack provenance here.
pub fn bind_pack_install_config(
    manifest: &PackManifest,
    config: &PackConfig,
    bindings: &PackInferenceBindings,
) -> Result<PackConfig> {
    validate_pack_inference_authoring(manifest, config)?;
    let mut bound = config.clone();
    for behavior in &mut bound.agent_behaviors {
        let slot = behavior
            .inference_profile_id
            .strip_prefix(INFERENCE_SLOT_REFERENCE_PREFIX)
            .context("pack behavior does not reference an inference slot")?;
        behavior.inference_profile_id = bindings
            .get(slot)
            .with_context(|| format!("inference slot {slot:?} is unbound"))?
            .clone();
    }
    anyhow::ensure!(
        bindings.len() == manifest.metadata.inference_slots.len(),
        "inference slot binding map does not exactly match the pack declaration"
    );
    super::provenance::stamp_pack_origin(manifest, &bound)
}

/// Publish bound document-pack configuration without replacing its principal
/// or the retained inference documents selected by the slot map.
pub async fn install_pack_documents(
    access: &ConfigAccess,
    config: &PackConfig,
) -> Result<crate::config_client::DesiredStateApplyCounts> {
    super::provenance::apply_pack_documents(access, config).await
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
            slot.behaviors
                .iter()
                .map(move |behavior| (behavior.as_str(), slot.name.as_str()))
        })
        .collect::<BTreeMap<_, _>>();
    anyhow::ensure!(
        declared.len() == config.agent_behaviors.len(),
        "pack {} must assign every behavior to exactly one inference slot",
        manifest.name
    );
    for behavior in &config.agent_behaviors {
        let slot = declared
            .get(behavior.behavior_id.as_str())
            .with_context(|| {
                format!(
                    "behavior {:?} is missing from inference slots",
                    behavior.behavior_id
                )
            })?;
        anyhow::ensure!(
            behavior.inference_profile_id == inference_slot_reference(slot),
            "behavior {:?} must reference inference slot {:?}",
            behavior.behavior_id,
            slot
        );
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

    fn config() -> PackConfig {
        serde_json::from_value(json!({
            "agent_principal": {"agent_did": "did:key:pack-owner"},
            "agent_behaviors": [
                {"agent_did":"did:key:pack-owner","behavior_id":"plan","inference_profile_id":"gents:inference-slot:coordinator","tags":["authored"]},
                {"agent_did":"did:key:pack-owner","behavior_id":"scan","inference_profile_id":"gents:inference-slot:worker"}
            ],
            "contexts": [{"agent_did":"did:key:pack-owner","context_id":"context"}],
            "tasks": [{"agent_did":"did:key:pack-owner","task_id":"task","behavior_id":"plan","prompt_template":"work"}]
        }))
        .unwrap()
    }

    fn two_slots() -> PackManifest {
        manifest(json!([
            {"name":"coordinator","description":"plans","behaviors":["plan"]},
            {"name":"worker","description":"scans","behaviors":["scan"]}
        ]))
    }

    #[test]
    fn binding_replaces_slot_markers_and_stamps_only_pack_owned_documents() {
        let bound = bind_pack_install_config(
            &two_slots(),
            &config(),
            &BTreeMap::from([
                ("coordinator".into(), "claude".into()),
                ("worker".into(), "glm".into()),
            ]),
        )
        .unwrap();
        assert_eq!(bound.agent_behaviors[0].inference_profile_id, "claude");
        assert_eq!(bound.agent_behaviors[1].inference_profile_id, "glm");
        for tags in [
            &bound.agent_behaviors[0].tags,
            &bound.agent_behaviors[1].tags,
            &bound.contexts[0].tags,
            &bound.tasks[0].tags,
        ] {
            assert!(tags.contains(&"gents:pack:test_pack".to_owned()));
        }
        assert_eq!(
            bound.agent_behaviors[0].tags,
            ["authored", "gents:pack:test_pack"]
        );
        assert!(bound.agent_principal.tags.is_empty());
        assert!(bound.inference_profiles.is_empty());
        assert!(bound.inference_backends.is_empty());
    }

    #[test]
    fn authoring_requires_exact_declared_slot_coverage_and_no_inference_docs() {
        let mut invalid = config();
        invalid.agent_behaviors[0].inference_profile_id = "user-profile".into();
        assert!(validate_pack_inference_authoring(&two_slots(), &invalid).is_err());
        let mut invalid = config();
        invalid.inference_profiles.push(InferenceProfile {
            agent_did: "did:key:pack-owner".into(),
            profile_id: "copy".into(),
            backend_id: "copy".into(),
            model_name: "copy".into(),
            ..Default::default()
        });
        assert!(validate_pack_inference_authoring(&two_slots(), &invalid).is_err());
        assert!(bind_pack_install_config(&two_slots(), &config(), &BTreeMap::new()).is_err());
    }

    #[tokio::test]
    async fn preview_auto_binds_only_the_unambiguous_one_to_one_case() {
        let owner = "did:key:preview-owner";
        let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
        crate::ensure_runtime_schemas(&node).await.unwrap();
        crate::document_config::ensure_agent_principal(&node, owner)
            .await
            .unwrap();
        crate::test_support::install_test_behavior(&node, owner, "only").await;
        let access = ConfigAccess::Local(node);
        let one = manifest(json!([
            {"name":"worker","description":"works","behaviors":["work"]}
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
        let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
        crate::ensure_runtime_schemas(&node).await.unwrap();
        crate::document_config::ensure_agent_principal(&node, owner)
            .await
            .unwrap();
        crate::test_support::install_test_behavior(&node, owner, "claude").await;
        crate::test_support::install_test_behavior(&node, owner, "glm").await;
        let foreign_owner = "did:key:foreign-owner";
        crate::document_config::ensure_agent_principal(&node, foreign_owner)
            .await
            .unwrap();
        crate::test_support::install_test_behavior(&node, foreign_owner, "foreign").await;
        let access = ConfigAccess::Local(node.clone());
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
                    agent_did: {_eq: "did:key:binding-owner"},
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
