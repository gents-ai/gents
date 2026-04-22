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
                },
                errors,
            },
        );
    }

    let principal = load_required_json::<DesiredAgentPrincipal>(
        root,
        Collection::AgentPrincipal
            .file_name()
            .expect("shimmed in Task 1, rewritten in Task 4"),
        &mut errors,
    );
    let behaviors = load_required_json::<Vec<DesiredAgentBehavior>>(
        root,
        Collection::AgentBehavior
            .file_name()
            .expect("shimmed in Task 1, rewritten in Task 4"),
        &mut errors,
    );
    let tool_selections = load_required_json::<Vec<DesiredToolSelection>>(
        root,
        Collection::ToolSelection
            .file_name()
            .expect("shimmed in Task 1, rewritten in Task 4"),
        &mut errors,
    );
    let backends = load_required_json::<Vec<DesiredInferenceBackend>>(
        root,
        Collection::InferenceBackend
            .file_name()
            .expect("shimmed in Task 1, rewritten in Task 4"),
        &mut errors,
    );
    let inference_profiles = load_optional_json::<Vec<DesiredInferenceProfile>>(
        root,
        Collection::InferenceProfile
            .file_name()
            .expect("shimmed in Task 1, rewritten in Task 4"),
        &mut errors,
    )
    .unwrap_or_default();
    let tool_service_registries = load_optional_json_collection::<DesiredToolServiceRegistry>(
        root,
        Collection::ToolServiceRegistry
            .file_name()
            .expect("shimmed in Task 1, rewritten in Task 4"),
        Collection::ToolServiceRegistry
            .dir_name()
            .expect("tool-services has a dir form"),
        &mut errors,
    )
    .unwrap_or_default();
    let tasks = load_optional_json_collection::<DesiredTask>(
        root,
        Collection::Task
            .file_name()
            .expect("shimmed in Task 1, rewritten in Task 4"),
        Collection::Task.dir_name().expect("tasks has a dir form"),
        &mut errors,
    )
    .unwrap_or_default();
    let schedules = load_optional_json_collection::<DesiredSchedule>(
        root,
        Collection::Schedule
            .file_name()
            .expect("shimmed in Task 1, rewritten in Task 4"),
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

use super::HasUniqueId;

/// Scan `<root>/<collection.dir_name()>/` for per-document subdirectories
/// of the form `<handle>/object.json` and parse each into `T`.
///
/// Errors are accumulated into `errors`; the function always returns a
/// `Vec<T>` containing every document it could successfully parse.
// Wired into load_manifest_root by Task 4 (per-agent manifest roots, #67);
// #[allow(dead_code)] suppresses the unused-function warning until then.
#[allow(dead_code)]
pub(crate) fn load_per_doc_collection<T>(
    root: &Path,
    collection: Collection,
    errors: &mut Vec<String>,
) -> Vec<T>
where
    T: for<'de> Deserialize<'de> + HasUniqueId,
{
    let dir_name = collection
        .dir_name()
        .expect("load_per_doc_collection called with a non-directory collection");
    let collection_path = root.join(dir_name);
    if !collection_path.exists() {
        return Vec::new();
    }
    if !collection_path.is_dir() {
        errors.push(format!(
            "manifest collection path is not a directory: {}",
            collection_path.display()
        ));
        return Vec::new();
    }

    let entries = match fs::read_dir(&collection_path) {
        Ok(iter) => iter,
        Err(error) => {
            errors.push(format!(
                "reading {} failed: {error}",
                collection_path.display()
            ));
            return Vec::new();
        }
    };

    let mut subdirs: Vec<(String, std::path::PathBuf)> = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                errors.push(format!(
                    "reading {} failed: {error}",
                    collection_path.display()
                ));
                continue;
            }
        };
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|s| s.to_str()) else {
            continue;
        };
        if name.starts_with('.') {
            continue;
        }
        subdirs.push((name.to_string(), path));
    }
    subdirs.sort_by(|a, b| a.0.cmp(&b.0));

    let mut docs: Vec<T> = Vec::with_capacity(subdirs.len());
    // Maps unique_id (from JSON body) -> first handle that produced it, for
    // duplicate detection. Populated regardless of whether the handle matched,
    // so that two mismatched dirs with the same inner id still produce a
    // duplicate error.
    let mut id_to_handle: std::collections::BTreeMap<String, String> =
        std::collections::BTreeMap::new();

    for (handle, subdir_path) in subdirs {
        let object_path = subdir_path.join("object.json");
        if !object_path.exists() {
            errors.push(format!(
                "per-doc dir is missing object.json: {}",
                subdir_path.display()
            ));
            continue;
        }
        let bytes = match fs::read(&object_path) {
            Ok(bytes) => bytes,
            Err(error) => {
                errors.push(format!("reading {} failed: {error}", object_path.display()));
                continue;
            }
        };
        let parsed: T = match serde_json::from_slice(&bytes) {
            Ok(parsed) => parsed,
            Err(error) => {
                errors.push(format!("invalid {}: {error}", object_path.display()));
                continue;
            }
        };

        // Check for duplicate unique IDs across all successfully-parsed docs,
        // regardless of whether the handle matches. This ensures two different
        // handle dirs that both embed the same unique_id both produce errors.
        if let Some(prior) = id_to_handle.get(parsed.unique_id()) {
            errors.push(format!(
                "duplicate {} '{}' across {}/ and {}/",
                collection.unique_field(),
                parsed.unique_id(),
                prior,
                handle
            ));
            continue;
        }
        id_to_handle.insert(parsed.unique_id().to_string(), handle.clone());

        if parsed.unique_id() != handle {
            errors.push(format!(
                "directory name '{handle}' does not match {} '{}' in {}",
                collection.unique_field(),
                parsed.unique_id(),
                object_path.display()
            ));
            continue;
        }

        docs.push(parsed);
    }
    docs
}

/// Hydrate a sidecar-eligible string field. If `value` is `Some(s)` where
/// `s` starts with `./`, treat the rest as a path relative to `json_dir`,
/// read the file as UTF-8, and replace `*value` with the file contents.
/// Any other case (None, absolute path, `../` prefix, literal string) is
/// a no-op.
// Wired into load_manifest_root by Task 4 (per-agent manifest roots, #67);
// #[allow(dead_code)] suppresses the unused-function warning until then.
#[allow(dead_code)]
pub(crate) fn hydrate_sidecar(
    value: &mut Option<String>,
    json_dir: &Path,
) -> Result<(), String> {
    let Some(current) = value.as_deref() else { return Ok(()) };
    if !current.starts_with("./") {
        return Ok(());
    }
    let rel = &current[2..];
    let sidecar_path = json_dir.join(rel);
    let bytes = fs::read(&sidecar_path).map_err(|error| {
        if error.kind() == std::io::ErrorKind::NotFound {
            format!(
                "sidecar path does not resolve: {} (referenced from {})",
                sidecar_path.display(),
                json_dir.display()
            )
        } else {
            format!("reading {} failed: {error}", sidecar_path.display())
        }
    })?;
    let body = String::from_utf8(bytes).map_err(|_| {
        format!("sidecar is not valid UTF-8: {}", sidecar_path.display())
    })?;
    *value = Some(body);
    Ok(())
}
