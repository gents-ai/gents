use std::collections::{BTreeSet, HashMap};

use gents::template::{
    catalog::default_catalog, reads::validate_system_template, validate_request_context_template,
};
use gents::{DEFAULT_DEADLINE_DURATION_SECS, DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS};

use super::super::DesiredStateManifest;
use super::storage::non_empty;

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
        let backend_id = backend.backend_id.trim();
        if backend_id.is_empty() {
            errors.push(
                "inference-backends.json contains a backend with an empty backend_id".to_string(),
            );
        } else if !backend_ids.insert(backend_id.to_string()) {
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

        if backend.endpoint.trim().is_empty() {
            errors.push(format!(
                "backend {} in inference-backends.json must contain a non-empty endpoint",
                backend.backend_id
            ));
        }

        if backend
            .api_key
            .as_deref()
            .map(str::trim)
            .is_some_and(|value| value.is_empty())
        {
            errors.push(format!(
                "backend {} in inference-backends.json contains an empty api_key",
                backend.backend_id
            ));
        }

        if backend
            .api_key
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_some()
            && backend
                .api_key_env_var
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .is_some()
        {
            errors.push(format!(
                "backend {} in inference-backends.json must not set both api_key and api_key_env_var",
                backend.backend_id
            ));
        }
    }
    (backend_ids, backend_models)
}

pub(super) fn validate_profiles(
    manifest: &DesiredStateManifest,
    errors: &mut Vec<String>,
) -> BTreeSet<String> {
    let mut profile_ids = BTreeSet::new();
    for profile in &manifest.inference_profiles {
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
        let stream_liveness_timeout_secs = match profile.stream_liveness_timeout_secs {
            Some(value) if value <= 0 => {
                errors.push(format!(
                    "InferenceProfile {profile_id} stream_liveness_timeout_secs must be positive"
                ));
                None
            }
            Some(value) => Some(value),
            None => Some(DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS as i64),
        };
        let deadline_duration_secs = match profile.deadline_duration_secs {
            Some(value) if value <= 0 => {
                errors.push(format!(
                    "InferenceProfile {profile_id} deadline_duration_secs must be positive"
                ));
                None
            }
            Some(value) => Some(value),
            None => Some(DEFAULT_DEADLINE_DURATION_SECS as i64),
        };
        if let (Some(stream_liveness_timeout_secs), Some(deadline_duration_secs)) =
            (stream_liveness_timeout_secs, deadline_duration_secs)
        {
            if stream_liveness_timeout_secs >= deadline_duration_secs {
                errors.push(format!(
                    "InferenceProfile {profile_id} stream_liveness_timeout_secs ({stream_liveness_timeout_secs}) must be less than deadline_duration_secs ({deadline_duration_secs})"
                ));
            }
        }
        if profile.seed.is_some_and(|value| value < 0) {
            errors.push(format!(
                "InferenceProfile {profile_id} seed must be non-negative"
            ));
        }
        // Empty reasoning effort is unset: DefraDB may materialize nullable
        // strings as empty values, and exported manifests must round-trip.
        if profile
            .reasoning_effort
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .is_some_and(|value| {
                !matches!(
                    value,
                    "none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max" | "ultra"
                )
            })
        {
            errors.push(format!(
                "InferenceProfile {profile_id} reasoning_effort must be one of: none, minimal, low, medium, high, xhigh, max, ultra"
            ));
        }
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
    let mut behavior_ids = BTreeSet::new();
    for behavior in &manifest.agent_behaviors {
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

        if let Some(backend_id) = non_empty(&behavior.backend_id) {
            if !backend_ids.contains(backend_id) {
                errors.push(format!(
                    "behavior {} references missing backend_id {}",
                    behavior.behavior_id, backend_id
                ));
            } else if let Some(model_name) = non_empty(&behavior.model_name) {
                let advertised = backend_models
                    .get(backend_id)
                    .expect("known backend has a model entry");
                if !advertised.is_empty() && !advertised.contains(model_name) {
                    errors.push(format!(
                        "behavior {} selects model {} which backend {} does not advertise",
                        behavior.behavior_id, model_name, backend_id
                    ));
                }
            }
        }

        if let Some(selection_id) = non_empty(&behavior.tool_selection_id) {
            if !tool_selection_ids.contains(selection_id) {
                errors.push(format!(
                    "behavior {} references missing tool_selection_id {}",
                    behavior.behavior_id, selection_id
                ));
            }
        }

        if let Some(profile_id) = non_empty(&behavior.inference_profile_id) {
            if !profile_ids.contains(profile_id) {
                errors.push(format!(
                    "behavior {} references missing inference_profile_id {}",
                    behavior.behavior_id, profile_id
                ));
            }
        }

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

        for skill_ref in &behavior.skill_refs {
            let skill_ref = skill_ref.trim();
            if !skill_ref.is_empty() && !skill_ids.contains(skill_ref) {
                errors.push(format!(
                    "behavior {} references missing skill_ref {} (import the skill first)",
                    behavior.behavior_id, skill_ref
                ));
            }
        }
        for skill_exclude in &behavior.skill_excludes {
            let skill_exclude = skill_exclude.trim();
            if !skill_exclude.is_empty() && !skill_ids.contains(skill_exclude) {
                errors.push(format!(
                    "behavior {} references missing skill_exclude {}",
                    behavior.behavior_id, skill_exclude
                ));
            }
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
