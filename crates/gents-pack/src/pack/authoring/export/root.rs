use std::collections::BTreeSet;
use std::fs;
use std::path::Path;

use anyhow::Result;
use serde_json::Value;

use gents_documents::PackConfig;
use gents_store::collection::Collection;

/// The prose fields that leave `pack_config.json` as sidecar files.
const SIDECARS: [(Collection, &str, &str); 3] = [
    (
        Collection::AgentContext,
        "system_prompt",
        "system_prompt.md",
    ),
    (Collection::Task, "prompt_template", "prompt_template.md"),
    (Collection::Skill, "instructions", "SKILL.md"),
];

pub fn document_handle(id: &str) -> String {
    id.replace('-', "_")
}

fn check_filesystem_safe_id(id: &str) -> Result<(), String> {
    if id.is_empty() || id.starts_with('.') || id.chars().any(|ch| matches!(ch, '/' | '\\' | '\0'))
    {
        return Err(format!("unique id '{id}' is not filesystem-safe"));
    }
    Ok(())
}

/// Moves each sidecar field of `config` out to the file `emit` writes at the
/// relative path it is given, leaving a `./` reference in its place. `handle`
/// names a document's directory.
pub(super) fn extract_sidecars(
    config: &mut Value,
    handle: &dyn Fn(Collection, &str) -> String,
    emit: &mut dyn FnMut(&str, &str) -> Result<()>,
) -> Result<()> {
    for (collection, field, file) in SIDECARS {
        let directory = collection.dir_name().expect("sidecar collection");
        let Some(rows) = config.get_mut(directory).and_then(Value::as_array_mut) else {
            continue;
        };
        for row in rows {
            let Some(text) = row.get(field).and_then(Value::as_str).map(str::to_owned) else {
                continue;
            };
            let id = row[collection.unique_field()]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("document identity missing"))?;
            let relative = format!("{directory}/{}/{file}", handle(collection, id));
            emit(&relative, &text)?;
            row[field] = format!("./{relative}").into();
        }
    }
    Ok(())
}

pub fn write_manifest_root(root: &Path, manifest: &PackConfig, force: bool) -> Result<()> {
    let plan = gents_documents::DesiredStateApplyPlan::from_pack_config(manifest)?;
    gents_documents::ConfigReferences::from_documents(
        &manifest.node.node_did,
        plan.documents()
            .iter()
            .map(|doc| (doc.collection, doc.add.clone())),
    )?;
    let mut handles = BTreeSet::new();
    for doc in plan.documents() {
        if doc.collection.keyed_by_owner() {
            continue;
        }
        let id = doc.add[doc.collection.unique_field()]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("document identity missing"))?;
        check_filesystem_safe_id(id).map_err(anyhow::Error::msg)?;
        anyhow::ensure!(
            handles.insert((doc.collection, document_handle(id))),
            "IDs collide at a filesystem handle in {}",
            doc.collection
        );
    }
    let mut value = serde_json::to_value(manifest)?;
    prepare_root(root, force)?;
    extract_sidecars(
        &mut value,
        &|_, id| document_handle(id),
        &mut |relative, text| {
            let path = root.join(relative);
            fs::create_dir_all(path.parent().expect("sidecar parent"))?;
            fs::write(path, text)?;
            Ok(())
        },
    )?;
    for collection in Collection::ALL {
        if let Some(rows) = collection
            .dir_name()
            .and_then(|name| value.get_mut(name))
            .and_then(Value::as_array_mut)
        {
            super::sort_document_rows(rows, collection.unique_field());
        }
    }
    // The loader interpolates every pack_config.json string but never sidecar
    // contents, so only the JSON is escaped to read back as exactly itself.
    crate::pack::interpolate::escape_values(&mut value);
    let mut bytes = serde_json::to_vec_pretty(&value)?;
    bytes.push(b'\n');
    fs::write(root.join("pack_config.json"), bytes)?;
    Ok(())
}

fn prepare_root(root: &Path, force: bool) -> Result<()> {
    if !root.exists() {
        fs::create_dir_all(root)?;
        return Ok(());
    }
    anyhow::ensure!(
        !fs::symlink_metadata(root)?.file_type().is_symlink(),
        "refusing to overwrite a symlink manifest root"
    );
    if fs::read_dir(root)?.next().is_some() {
        anyhow::ensure!(
            force,
            "manifest root is non-empty; pass --force to overwrite: {}",
            root.display()
        );
        anyhow::ensure!(
            root.join("pack_config.json").is_file(),
            "refusing to overwrite {}: not a manifest root",
            root.display()
        );
        fs::remove_dir_all(root)?;
    }
    fs::create_dir_all(root)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config() -> PackConfig {
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
