use super::DesiredStateManifest;
use gents::Collection;
use serde_json::Value;
use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
};

#[derive(Debug, thiserror::Error)]
pub(crate) enum WriteError {
    #[error(transparent)]
    Plan(Box<gents::config_client::DesiredStateError>),
    #[error(transparent)]
    References(Box<gents::document_config::DocumentConfigError>),
    #[error("document identity missing")]
    DocumentIdentityMissing,
    #[error("unique id '{id}' is not filesystem-safe")]
    UnsafeId { id: String },
    #[error("IDs collide at a filesystem handle in {collection}")]
    HandleCollision { collection: Collection },
    #[error("refusing to overwrite a symlink manifest root")]
    SymlinkRoot,
    #[error("manifest root is non-empty; pass --force to overwrite: {}", root.display())]
    NonEmptyRoot { root: PathBuf },
    #[error("refusing to overwrite {}: not a manifest root", root.display())]
    NotManifestRoot { root: PathBuf },
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

type Result<T, E = WriteError> = std::result::Result<T, E>;

fn check_filesystem_safe_id(id: &str) -> Result<()> {
    if id.is_empty() || id.starts_with('.') || id.chars().any(|ch| matches!(ch, '/' | '\\' | '\0'))
    {
        return Err(WriteError::UnsafeId { id: id.to_owned() });
    }
    Ok(())
}

pub(crate) fn write_manifest_root(
    root: &Path,
    manifest: &DesiredStateManifest,
    force: bool,
) -> Result<()> {
    let plan = gents::config_client::DesiredStateApplyPlan::from_pack_config(manifest)
        .map_err(|error| WriteError::Plan(Box::new(error)))?;
    gents::document_config::ConfigReferences::from_documents(
        &manifest.node.node_did,
        plan.documents()
            .iter()
            .map(|doc| (doc.collection, doc.add.clone())),
    )
    .map_err(|error| WriteError::References(Box::new(error)))?;
    let mut handles = BTreeSet::new();
    for doc in plan.documents() {
        if doc.collection == Collection::Node {
            continue;
        }
        let id = doc.add[doc.collection.unique_field()]
            .as_str()
            .ok_or(WriteError::DocumentIdentityMissing)?;
        check_filesystem_safe_id(id)?;
        if !handles.insert((doc.collection, super::document_handle(id))) {
            return Err(WriteError::HandleCollision {
                collection: doc.collection,
            });
        }
    }
    let mut value = serde_json::to_value(manifest)?;
    prepare_root(root, force)?;
    for (collection, field, file) in [
        (
            Collection::AgentContext,
            "system_prompt",
            "system_prompt.md",
        ),
        (Collection::Task, "prompt_template", "prompt_template.md"),
    ] {
        if let Some(rows) = value
            .get_mut(collection.dir_name().expect("sidecar collection"))
            .and_then(Value::as_array_mut)
        {
            for row in rows {
                let Some(prompt) = row.get(field).and_then(Value::as_str).map(str::to_owned) else {
                    continue;
                };
                let id = row[collection.unique_field()]
                    .as_str()
                    .expect("validated identity");
                let relative = format!(
                    "{}/{}/{}",
                    collection.dir_name().unwrap(),
                    super::document_handle(id),
                    file
                );
                let path = root.join(&relative);
                fs::create_dir_all(path.parent().expect("sidecar parent"))?;
                fs::write(path, prompt)?;
                row[field] = format!("./{relative}").into();
            }
        }
    }
    for collection in Collection::ALL {
        if let Some(rows) = collection
            .dir_name()
            .and_then(|name| value.get_mut(name))
            .and_then(Value::as_array_mut)
        {
            rows.sort_by(|a, b| {
                a[collection.unique_field()]
                    .as_str()
                    .cmp(&b[collection.unique_field()].as_str())
            });
        }
    }
    // The loader interpolates every pack_config.json string but never sidecar
    // contents, so only the JSON is escaped to read back as exactly itself.
    gents::pack::interpolate::escape_values(&mut value);
    let mut bytes = serde_json::to_vec_pretty(&value)?;
    bytes.push(b'\n');
    fs::write(root.join("pack_config.json"), bytes)?;
    Ok(())
}

fn prepare_root(root: &Path, force: bool) -> Result<()> {
    if !root.exists() {
        return Ok(fs::create_dir_all(root)?);
    }
    if fs::symlink_metadata(root)?.file_type().is_symlink() {
        return Err(WriteError::SymlinkRoot);
    }
    let empty = fs::read_dir(root)?.next().is_none();
    if !empty {
        if !force {
            return Err(WriteError::NonEmptyRoot {
                root: root.to_owned(),
            });
        }
        if !root.join("pack_config.json").is_file() {
            return Err(WriteError::NotManifestRoot {
                root: root.to_owned(),
            });
        }
        fs::remove_dir_all(root)?;
    }
    Ok(fs::create_dir_all(root)?)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config() -> DesiredStateManifest {
        serde_json::from_value(serde_json::json!({"node":{"node_did":"owner"}})).unwrap()
    }
    #[test]
    fn overwrite_is_explicit_confined_and_removes_stale_files() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("unrelated"), "keep").unwrap();
        assert!(write_manifest_root(dir.path(), &config(), true).is_err());
        assert!(dir.path().join("unrelated").exists());
        fs::remove_file(dir.path().join("unrelated")).unwrap();
        write_manifest_root(dir.path(), &config(), false).unwrap();
        fs::write(dir.path().join("stale"), "old").unwrap();
        assert!(write_manifest_root(dir.path(), &config(), false).is_err());
        write_manifest_root(dir.path(), &config(), true).unwrap();
        assert!(!dir.path().join("stale").exists());
    }
    #[test]
    fn dollar_text_round_trips_through_export_and_load() {
        let script = "echo ${HOME} $${X} $$ $";
        let prompt = "context ${HOME} $${X} $$";
        let config: DesiredStateManifest = serde_json::from_value(serde_json::json!({
            "node":{"node_did":"owner"},
            "contexts":[{"node_did":"owner","context_id":"context","system_prompt":prompt}],
            "agents":[{"node_did":"owner","agent_id":"agent","context_id":"context",
                "inference_profile_id":"profile"}],
            "tasks":[{"node_did":"owner","task_id":"task","agent_id":"agent",
                "prompt_template":"work","hooks":[
                    {"hook_id":"gate","phase":"before","command":["sh","-c",script]}
            ]}]
        }))
        .unwrap();
        let dir = tempfile::tempdir().unwrap();
        write_manifest_root(dir.path(), &config, false).unwrap();
        let (loaded, report) = super::super::load_manifest_root(dir.path());
        assert!(report.ok, "{:?}", report.errors);
        let loaded = loaded.unwrap();
        assert_eq!(loaded.tasks[0].hooks[0].command[2], script);
        assert_eq!(loaded.contexts[0].system_prompt.as_deref(), Some(prompt));
        assert_eq!(
            serde_json::to_value(&loaded).unwrap(),
            serde_json::to_value(&config).unwrap()
        );
    }

    #[test]
    fn absent_prompt_has_no_sidecar_or_authored_field() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = config();
        config.contexts.push(
            serde_json::from_value(serde_json::json!({"node_did":"owner","context_id":"context"}))
                .unwrap(),
        );
        write_manifest_root(dir.path(), &config, false).unwrap();
        let value: Value =
            serde_json::from_slice(&fs::read(dir.path().join("pack_config.json")).unwrap())
                .unwrap();
        assert!(value["contexts"][0].get("system_prompt").is_none());
        assert!(!dir.path().join("contexts").exists());
    }
}
