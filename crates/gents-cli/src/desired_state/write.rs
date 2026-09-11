use super::DesiredStateManifest;
use gents::Collection;
use serde_json::Value;
use std::{collections::BTreeSet, fs, path::Path};

pub(crate) fn check_filesystem_safe_id(id: &str) -> Result<(), String> {
    if id.is_empty() || id.starts_with('.') || id.chars().any(|ch| matches!(ch, '/' | '\\' | '\0'))
    {
        return Err(format!("unique id '{id}' is not filesystem-safe"));
    }
    Ok(())
}

pub(crate) fn write_manifest_root(
    root: &Path,
    manifest: &DesiredStateManifest,
    force: bool,
) -> Result<(), String> {
    write_root(root, manifest, force).map_err(|error| format!("{error:#}"))
}

fn write_root(root: &Path, manifest: &DesiredStateManifest, force: bool) -> anyhow::Result<()> {
    let plan = gents::config_client::DesiredStateApplyPlan::from_pack_config(manifest)?;
    gents::document_config::ConfigReferences::from_documents(
        &manifest.agent_principal.agent_did,
        plan.documents()
            .iter()
            .map(|doc| (doc.collection, doc.add.clone())),
    )?;
    let mut handles = BTreeSet::new();
    for doc in plan.documents() {
        if doc.collection == Collection::AgentPrincipal {
            continue;
        }
        let id = doc.add[doc.collection.unique_field()]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("document identity missing"))?;
        check_filesystem_safe_id(id).map_err(anyhow::Error::msg)?;
        anyhow::ensure!(
            handles.insert((doc.collection, super::document_handle(id))),
            "IDs collide at a filesystem handle in {}",
            doc.collection
        );
    }
    // Finish all structural checks before a forced replacement removes old files.
    let mut value = serde_json::to_value(manifest)?;
    prepare_root(root, force).map_err(anyhow::Error::msg)?;
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
    // Canonical root order makes exports stable without mutating authored values,
    // permission arrays, prompt whitespace, or any nested execution ordering.
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
    let mut bytes = serde_json::to_vec_pretty(&value)?;
    bytes.push(b'\n');
    fs::write(root.join("pack_config.json"), bytes)?;
    Ok(())
}

fn prepare_root(root: &Path, force: bool) -> Result<(), String> {
    if !root.exists() {
        return fs::create_dir_all(root).map_err(|error| error.to_string());
    }
    if fs::symlink_metadata(root)
        .map_err(|error| error.to_string())?
        .file_type()
        .is_symlink()
    {
        return Err("refusing to overwrite a symlink manifest root".into());
    }
    let empty = fs::read_dir(root)
        .map_err(|error| error.to_string())?
        .next()
        .is_none();
    if !empty {
        if !force {
            return Err(format!(
                "manifest root is non-empty; pass --force to overwrite: {}",
                root.display()
            ));
        }
        if !root.join("pack_config.json").is_file() {
            return Err(format!(
                "refusing to overwrite {}: not a manifest root",
                root.display()
            ));
        }
        fs::remove_dir_all(root).map_err(|error| error.to_string())?;
    }
    fs::create_dir_all(root).map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn config() -> DesiredStateManifest {
        serde_json::from_value(serde_json::json!({"agent_principal":{"agent_did":"owner"}}))
            .unwrap()
    }
    #[test]
    fn filesystem_id_boundary_preserves_human_keys() {
        for id in [
            "default",
            "workstation-1",
            "seed_fleet_health",
            "profile:default",
        ] {
            assert!(check_filesystem_safe_id(id).is_ok(), "{id}");
        }
        for id in ["", ".", "..", ".hidden", "a/b", "a\\b", "a\0b"] {
            assert!(check_filesystem_safe_id(id).is_err(), "{id}");
        }
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
    fn handle_collisions_and_unsafe_ids_fail_before_overwrite() {
        let dir = tempfile::tempdir().unwrap();
        write_manifest_root(dir.path(), &config(), false).unwrap();
        let file = dir.path().join("pack_config.json");
        let before = fs::read(&file).unwrap();
        for ids in [vec!["a-b", "a_b"], vec!["unsafe/id"]] {
            let mut config = config();
            config.contexts = ids
                .into_iter()
                .map(|id| {
                    serde_json::from_value(serde_json::json!({"agent_did":"owner","context_id":id}))
                        .unwrap()
                })
                .collect();
            assert!(write_manifest_root(dir.path(), &config, true).is_err());
            assert_eq!(fs::read(&file).unwrap(), before);
        }
    }
    #[test]
    fn absent_prompt_has_no_sidecar_or_authored_field() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = config();
        config.contexts.push(
            serde_json::from_value(serde_json::json!({"agent_did":"owner","context_id":"context"}))
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
