use std::collections::{BTreeMap, BTreeSet, HashMap};

use gents::template::{
    catalog::default_catalog, reads::validate_system_template, validate_request_context_template,
};
use gents::{
    AgentBehaviorDocument, ConfigReferences, InferenceBackend, InferenceProfile,
    UNKNOWN_PROBE_STATUS,
};

use super::super::DesiredStateManifest;
use super::super::{DesiredAgentBehavior, DesiredInferenceBackend, DesiredInferenceProfile};
use super::storage::non_empty;

/// Decode a manifest backend into the document type `InferenceBackend::validate`
/// owns. `probe_status` is runtime-owned (not part of desired state) and does
/// not affect validation.
fn to_document_backend(backend: &DesiredInferenceBackend) -> InferenceBackend {
    InferenceBackend {
        backend_id: backend.backend_id.clone(),
        name: backend.name.clone(),
        provider_kind: backend.provider_kind,
        openai_wire_api: backend.openai_wire_api,
        endpoint: backend.endpoint.clone(),
        api_key: backend.api_key.clone(),
        api_key_env_var: backend.api_key_env_var.clone(),
        max_concurrent: backend.max_concurrent,
        max_queue_depth: backend.max_queue_depth,
        enabled: backend.enabled,
        models: backend.models.clone(),
        probe_status: UNKNOWN_PROBE_STATUS.to_string(),
    }
}

/// Decode a manifest profile into the document type `InferenceProfile::validate`
/// owns. The two structs are field-for-field identical; this is a plain copy.
fn to_document_profile(profile: &DesiredInferenceProfile) -> InferenceProfile {
    InferenceProfile {
        profile_id: profile.profile_id.clone(),
        display_name: profile.display_name.clone(),
        context_window: profile.context_window,
        max_output_tokens: profile.max_output_tokens,
        max_turns: profile.max_turns,
        temperature: profile.temperature,
        top_p: profile.top_p,
        top_k: profile.top_k,
        seed: profile.seed,
        min_p: profile.min_p,
        frequency_penalty: profile.frequency_penalty,
        presence_penalty: profile.presence_penalty,
        repetition_penalty: profile.repetition_penalty,
        reasoning_effort: profile.reasoning_effort.clone(),
        stream_batch_ms: profile.stream_batch_ms,
        stream_liveness_timeout_secs: profile.stream_liveness_timeout_secs,
        deadline_duration_secs: profile.deadline_duration_secs,
        retry_max_transport: profile.retry_max_transport,
        retry_backoff_ms: profile.retry_backoff_ms.clone(),
        retry_max_resample: profile.retry_max_resample,
        retry_allow_repair: profile.retry_allow_repair,
        retry_interactive_max: profile.retry_interactive_max,
    }
}

/// Decode a manifest behavior into the document type
/// `AgentBehavior::validate_references` owns.
fn to_document_behavior(behavior: &DesiredAgentBehavior) -> AgentBehaviorDocument {
    AgentBehaviorDocument {
        behavior_id: behavior.behavior_id.clone(),
        agent_did: behavior.agent_did.clone(),
        display_name: behavior.display_name.clone(),
        description: behavior.description.clone(),
        summary: behavior.summary.clone(),
        system_prompt: behavior.system_prompt.clone(),
        request_context_template: behavior.request_context_template.clone(),
        backend_id: behavior.backend_id.clone(),
        model_name: behavior.model_name.clone(),
        tool_selection_id: behavior.tool_selection_id.clone(),
        inference_profile_id: behavior.inference_profile_id.clone(),
        compaction_strategy: behavior.compaction_strategy.clone(),
        compaction_threshold: behavior.compaction_threshold,
        enabled: behavior.enabled,
        skill_refs: behavior.skill_refs.clone(),
        skill_excludes: behavior.skill_excludes.clone(),
        created_at: None,
    }
}

pub(super) fn validate_principal<'a>(
    manifest: &'a DesiredStateManifest,
    errors: &mut Vec<String>,
) -> &'a str {
    let principal_agent_did = manifest.agent_principal.agent_did.trim();
    if principal_agent_did.is_empty() {
        errors.push("agent-principal.json must contain a non-empty agent_did".to_string());
    }
    principal_agent_did
}

pub(super) fn validate_backends(
    manifest: &DesiredStateManifest,
    errors: &mut Vec<String>,
) -> (BTreeSet<String>, HashMap<String, BTreeSet<String>>) {
    let mut backend_ids = BTreeSet::new();
    let mut backend_models = HashMap::<String, BTreeSet<String>>::new();
    for backend in &manifest.inference_backends {
        // Manifest-shape: duplicate backend_id detection has no document
        // equivalent (a document validator sees one document at a time).
        let backend_id = backend.backend_id.trim();
        if !backend_id.is_empty() && !backend_ids.insert(backend_id.to_string()) {
            errors.push(format!(
                "duplicate backend_id in inference-backends.json: {backend_id}"
            ));
        }
        if !backend_id.is_empty() {
            backend_models.insert(
                backend_id.to_string(),
                backend
                    .models
                    .iter()
                    .map(|model| model.trim())
                    .filter(|model| !model.is_empty())
                    .map(str::to_string)
                    .collect(),
            );
        }

        // Document rules (empty backend_id/endpoint, api_key shape,
        // max_concurrent/max_queue_depth) are owned by
        // `InferenceBackend::validate`. `current_model=None`: desired state
        // validates backends independently of behaviors and separately
        // checks each behavior's model against its backend's advertised list
        // in `validate_behaviors`, so the no-lockout conjunct never fires
        // here.
        errors.extend(to_document_backend(backend).validation_violations(None));
    }
    (backend_ids, backend_models)
}

pub(super) fn validate_profiles(
    manifest: &DesiredStateManifest,
    errors: &mut Vec<String>,
) -> BTreeSet<String> {
    let mut profile_ids = BTreeSet::new();
    for profile in &manifest.inference_profiles {
        // Manifest-shape: empty/duplicate profile_id detection has no
        // document equivalent (`InferenceProfile::validate` does not check
        // profile_id — a document validator sees one document at a time and
        // cannot compare it against siblings; emptiness here is a manifest
        // authoring mistake naming the file it came from).
        let profile_id = profile.profile_id.trim();
        if profile_id.is_empty() {
            errors.push(
                "inference-profiles.json contains a profile with an empty profile_id".to_string(),
            );
        } else if !profile_ids.insert(profile_id.to_string()) {
            errors.push(format!(
                "duplicate profile_id in inference-profiles.json: {profile_id}"
            ));
        }

        // Document rules (timeout/deadline bounds and relationship, seed,
        // reasoning_effort vocabulary) are owned by
        // `InferenceProfile::validate`.
        errors.extend(to_document_profile(profile).validation_violations());
    }
    profile_ids
}

pub(super) fn validate_skills(
    manifest: &DesiredStateManifest,
    principal_agent_did: &str,
    errors: &mut Vec<String>,
) -> BTreeSet<String> {
    let mut skill_ids = BTreeSet::new();
    for skill in &manifest.skills {
        let skill_id = skill.skill_id.trim();
        if skill_id.is_empty() {
            errors.push("skills manifest contains a skill with an empty skill_id".to_string());
        } else if !skill_ids.insert(skill_id.to_string()) {
            errors.push(format!("duplicate skill_id in skills manifest: {skill_id}"));
        }

        if !principal_agent_did.is_empty() && skill.agent_did.trim() != principal_agent_did {
            errors.push(format!(
                "skill {} belongs to {} not {}",
                skill.skill_id, skill.agent_did, manifest.agent_principal.agent_did
            ));
        }

        if !matches!(skill.scope.trim(), "principal" | "behavior") {
            errors.push(format!(
                "skill {} has invalid scope {:?}; expected \"principal\" or \"behavior\"",
                skill.skill_id, skill.scope
            ));
        }

        if skill.name.trim().is_empty() {
            errors.push(format!(
                "skill {} in skills manifest must contain a non-empty name",
                skill.skill_id
            ));
        }
    }
    skill_ids
}

pub(super) fn validate_behaviors(
    manifest: &DesiredStateManifest,
    principal_agent_did: &str,
    backend_ids: &BTreeSet<String>,
    backend_models: &HashMap<String, BTreeSet<String>>,
    tool_selection_ids: &BTreeSet<String>,
    profile_ids: &BTreeSet<String>,
    skill_ids: &BTreeSet<String>,
    errors: &mut Vec<String>,
) -> BTreeSet<String> {
    // Document rule: reference existence (backend, model advertised by the
    // backend, tool selection, profile, skill refs/excludes) is owned by
    // `AgentBehavior::validate_references`. `backend_ids` names every
    // non-empty manifest backend id; document-shape violations for those
    // backends are reported independently by `validate_backends`.
    let refs = ConfigReferences {
        backends: backend_ids
            .iter()
            .map(|id| {
                let models = backend_models
                    .get(id)
                    .map(|models| models.iter().cloned().collect())
                    .unwrap_or_default();
                (id.clone(), models)
            })
            .collect::<BTreeMap<_, _>>(),
        tool_selections: tool_selection_ids.clone(),
        profiles: profile_ids.clone(),
        skills: skill_ids.clone(),
    };

    let mut behavior_ids = BTreeSet::new();
    for behavior in &manifest.agent_behaviors {
        // Manifest-shape: empty/duplicate behavior_id and principal
        // ownership have no document equivalent.
        let behavior_id = behavior.behavior_id.trim();
        if behavior_id.is_empty() {
            errors.push(
                "agent-behaviors.json contains a behavior with an empty behavior_id".to_string(),
            );
        } else if !behavior_ids.insert(behavior_id.to_string()) {
            errors.push(format!(
                "duplicate behavior_id in agent-behaviors.json: {behavior_id}"
            ));
        }

        if !principal_agent_did.is_empty() && behavior.agent_did.trim() != principal_agent_did {
            errors.push(format!(
                "behavior {} belongs to {} not {}",
                behavior.behavior_id, behavior.agent_did, manifest.agent_principal.agent_did
            ));
        }

        // Each violation becomes its own error-list entry (not one joined
        // string) — `reference_violations` is the Vec-returning variant of
        // `AgentBehavior::validate_references` for exactly this reason.
        errors.extend(to_document_behavior(behavior).reference_violations(&refs));

        if let Some(system_prompt) = behavior.system_prompt.as_deref() {
            validate_behavior_system_template(&behavior.behavior_id, system_prompt, errors);
        }

        if let Some(request_context_template) = behavior.request_context_template.as_deref() {
            validate_behavior_request_context_template(
                &behavior.behavior_id,
                request_context_template,
                errors,
            );
        }
    }
    behavior_ids
}

pub(super) fn validate_default_behavior(
    manifest: &DesiredStateManifest,
    behavior_ids: &BTreeSet<String>,
    errors: &mut Vec<String>,
) {
    match non_empty(&manifest.agent_principal.default_behavior_id) {
        Some(default_behavior_id) => {
            if !behavior_ids.contains(default_behavior_id) {
                errors.push(format!(
                    "agent-principal.json default_behavior_id {} is not present in agent-behaviors.json",
                    default_behavior_id
                ));
            }
        }
        None => errors
            .push("agent-principal.json must contain a non-empty default_behavior_id".to_string()),
    }
}

fn validate_behavior_system_template(
    behavior_id: &str,
    system_prompt: &str,
    errors: &mut Vec<String>,
) {
    if !contains_template_marker(system_prompt) {
        return;
    }
    let catalog = default_catalog();
    if let Err(error) = validate_system_template(system_prompt, &catalog) {
        errors.push(format!(
            "behavior {behavior_id} system_prompt template is invalid: {error}"
        ));
    }
}

fn validate_behavior_request_context_template(
    behavior_id: &str,
    request_context_template: &str,
    errors: &mut Vec<String>,
) {
    if !contains_template_marker(request_context_template) {
        return;
    }
    let catalog = default_catalog();
    if let Err(error) = validate_request_context_template(request_context_template, &catalog) {
        errors.push(format!(
            "behavior {behavior_id} request_context_template is invalid: {error}"
        ));
    }
}

fn contains_template_marker(value: &str) -> bool {
    value.contains("{{") || value.contains("{%") || value.contains("{#")
}
