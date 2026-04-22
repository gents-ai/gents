use std::fs;
use std::path::Path;

use serde::Deserialize;

use super::normalize::normalize_manifest;
use super::validate::validate_manifest;
use super::{
    DesiredAgentBehavior, DesiredAgentPrincipal, DesiredInferenceBackend, DesiredInferenceProfile,
    DesiredSchedule, DesiredStateCounts, DesiredStateManifest, DesiredStateValidationReport,
    DesiredTask, DesiredToolSelection, DesiredToolServiceRegistry,
};
use defra_agent::Collection;

pub(crate) fn validate_manifest_root(root: &Path) -> DesiredStateValidationReport {
    load_manifest_root(root).1
}

pub(crate) fn load_manifest_root(
    root: &Path,
) -> (Option<DesiredStateManifest>, DesiredStateValidationReport) {
    let root_display = root.display().to_string();
    let mut errors = Vec::new();

    if !root.exists() {
        errors.push(format!("manifest root does not exist: {root_display}"));
        return (
            None,
            DesiredStateValidationReport {
                status: "invalid",
                ok: false,
                root: root_display,
                agent_did: None,
                counts: DesiredStateCounts {
                    agent_principal: 0,
                    agent_behaviors: 0,
                    tool_selections: 0,
                    inference_backends: 0,
                    inference_profiles: 0,
                    tool_service_registries: 0,
                    tasks: 0,
                    schedules: 0,
                    event_triggers: 0,
                },
                errors,
            },
        );
    }
    if !root.is_dir() {
        errors.push(format!("manifest root is not a directory: {root_display}"));
        return (
            None,
            DesiredStateValidationReport {
                status: "invalid",
                ok: false,
                root: root_display,
                agent_did: None,
                counts: DesiredStateCounts {
                    agent_principal: 0,
                    agent_behaviors: 0,
                    tool_selections: 0,
                    inference_backends: 0,
                    inference_profiles: 0,
                    tool_service_registries: 0,
                    tasks: 0,
                    schedules: 0,
                    event_triggers: 0,
                },
                errors,
            },
        );
    }

    let principal = load_required_json::<DesiredAgentPrincipal>(
        root,
        Collection::AgentPrincipal.file_name(),
        &mut errors,
    );
    let behaviors = load_required_json::<Vec<DesiredAgentBehavior>>(
        root,
        Collection::AgentBehavior.file_name(),
        &mut errors,
    );
    let tool_selections = load_required_json::<Vec<DesiredToolSelection>>(
        root,
        Collection::ToolSelection.file_name(),
        &mut errors,
    );
    let backends = load_required_json::<Vec<DesiredInferenceBackend>>(
        root,
        Collection::InferenceBackend.file_name(),
        &mut errors,
    );
    let inference_profiles = load_optional_json::<Vec<DesiredInferenceProfile>>(
        root,
        Collection::InferenceProfile.file_name(),
        &mut errors,
    )
    .unwrap_or_default();
    let tool_service_registries = load_optional_json_collection::<DesiredToolServiceRegistry>(
        root,
        Collection::ToolServiceRegistry.file_name(),
        Collection::ToolServiceRegistry
            .dir_name()
            .expect("tool-services has a dir form"),
        &mut errors,
    )
    .unwrap_or_default();
    let tasks = load_optional_json_collection::<DesiredTask>(
        root,
        Collection::Task.file_name(),
        Collection::Task.dir_name().expect("tasks has a dir form"),
        &mut errors,
    )
    .unwrap_or_default();
    let schedules = load_optional_json_collection::<DesiredSchedule>(
        root,
        Collection::Schedule.file_name(),
        Collection::Schedule
            .dir_name()
            .expect("schedules has a dir form"),
        &mut errors,
    )
    .unwrap_or_default();

    let counts = DesiredStateCounts {
        agent_principal: usize::from(principal.is_some()),
        agent_behaviors: behaviors.as_ref().map_or(0, Vec::len),
        tool_selections: tool_selections.as_ref().map_or(0, Vec::len),
        inference_backends: backends.as_ref().map_or(0, Vec::len),
        inference_profiles: inference_profiles.len(),
        tool_service_registries: tool_service_registries.len(),
        tasks: tasks.len(),
        schedules: schedules.len(),
        event_triggers: 0,
    };

    let agent_did = principal.as_ref().map(|value| value.agent_did.clone());

    let manifest =
        if let (Some(principal), Some(behaviors), Some(tool_selections), Some(backends)) =
            (principal, behaviors, tool_selections, backends)
        {
            let mut manifest = DesiredStateManifest {
                agent_principal: principal,
                agent_behaviors: behaviors,
                tool_selections,
                inference_backends: backends,
                inference_profiles,
                tool_service_registries,
                tasks,
                schedules,
                event_triggers: Vec::new(),
            };
            normalize_manifest(&mut manifest);
            validate_manifest(&manifest, &mut errors);
            Some(manifest)
        } else {
            None
        };

    (
        manifest,
        DesiredStateValidationReport {
            status: if errors.is_empty() {
                "validated"
            } else {
                "invalid"
            },
            ok: errors.is_empty(),
            root: root_display,
            agent_did,
            counts,
            errors,
        },
    )
}

pub(super) fn load_required_json<T>(
    root: &Path,
    file_name: &str,
    errors: &mut Vec<String>,
) -> Option<T>
where
    T: for<'de> Deserialize<'de>,
{
    match load_json_file(root, file_name) {
        Ok(Some(value)) => Some(value),
        Ok(None) => {
            errors.push(format!(
                "required manifest file is missing: {}",
                root.join(file_name).display()
            ));
            None
        }
        Err(error) => {
            errors.push(error);
            None
        }
    }
}

pub(super) fn load_optional_json<T>(
    root: &Path,
    file_name: &str,
    errors: &mut Vec<String>,
) -> Option<T>
where
    T: for<'de> Deserialize<'de>,
{
    match load_json_file(root, file_name) {
        Ok(value) => value,
        Err(error) => {
            errors.push(error);
            None
        }
    }
}

pub(super) fn load_optional_json_collection<T>(
    root: &Path,
    file_name: &str,
    dir_name: &str,
    errors: &mut Vec<String>,
) -> Option<Vec<T>>
where
    T: for<'de> Deserialize<'de>,
{
    match load_json_collection(root, file_name, dir_name) {
        Ok(value) => value,
        Err(error) => {
            errors.push(error);
            None
        }
    }
}

pub(super) fn load_json_file<T>(root: &Path, file_name: &str) -> Result<Option<T>, String>
where
    T: for<'de> Deserialize<'de>,
{
    let path = root.join(file_name);
    if !path.exists() {
        return Ok(None);
    }

    let bytes =
        fs::read(&path).map_err(|error| format!("reading {} failed: {error}", path.display()))?;
    serde_json::from_slice::<T>(&bytes)
        .map(Some)
        .map_err(|error| format!("invalid {}: {error}", path.display()))
}

pub(super) fn load_json_collection<T>(
    root: &Path,
    file_name: &str,
    dir_name: &str,
) -> Result<Option<Vec<T>>, String>
where
    T: for<'de> Deserialize<'de>,
{
    let file_path = root.join(file_name);
    let dir_path = root.join(dir_name);

    if file_path.exists() && dir_path.exists() {
        return Err(format!(
            "manifest root must not contain both {} and {}",
            file_path.display(),
            dir_path.display()
        ));
    }

    if file_path.exists() {
        return load_json_file(root, file_name);
    }

    if !dir_path.exists() {
        return Ok(None);
    }
    if !dir_path.is_dir() {
        return Err(format!(
            "manifest collection path is not a directory: {}",
            dir_path.display()
        ));
    }

    let mut entry_paths = fs::read_dir(&dir_path)
        .map_err(|error| format!("reading {} failed: {error}", dir_path.display()))?
        .map(|entry| {
            entry
                .map(|entry| entry.path())
                .map_err(|error| format!("reading {} failed: {error}", dir_path.display()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    entry_paths.sort();

    let mut values = Vec::new();
    for path in entry_paths {
        if !path.is_file() {
            continue;
        }
        if path.extension().and_then(|value| value.to_str()) != Some("json") {
            continue;
        }
        let bytes = fs::read(&path)
            .map_err(|error| format!("reading {} failed: {error}", path.display()))?;
        let value = serde_json::from_slice::<T>(&bytes)
            .map_err(|error| format!("invalid {}: {error}", path.display()))?;
        values.push(value);
    }

    Ok(Some(values))
}
