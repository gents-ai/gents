use super::{DesiredStateManifest, DesiredStateValidationReport};
use anyhow::{Context, Result};
use gents::Collection;
use serde_json::{Map, Value};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

pub(crate) fn load_manifest_root(
    root: &Path,
) -> (Option<DesiredStateManifest>, DesiredStateValidationReport) {
    let mut errors = Vec::new();
    let manifest = match load_root(root) {
        Ok(config) => Some(config),
        Err(error) => {
            errors.push(format!("{error:#}"));
            None
        }
    };
    let counts = manifest
        .as_ref()
        .map(crate::shared::ConfigApplyCounts::from_config)
        .transpose();
    let counts = match counts {
        Ok(counts) => counts.unwrap_or_default(),
        Err(error) => {
            errors.push(error.to_string());
            Default::default()
        }
    };
    let report = DesiredStateValidationReport {
        validation_scope: "offline_shape",
        status: if errors.is_empty() {
            "validated"
        } else {
            "invalid"
        },
        ok: errors.is_empty(),
        root: root.display().to_string(),
        agent_did: manifest
            .as_ref()
            .map(|config| config.agent_principal.agent_did.clone()),
        counts,
        errors,
    };
    (manifest, report)
}

fn load_root(root: &Path) -> Result<DesiredStateManifest> {
    anyhow::ensure!(
        root.is_dir(),
        "manifest root is not a directory: {}",
        root.display()
    );
    // A document-bearing unknown directory must not silently disappear on apply.
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') || !entry.path().is_dir() {
            continue;
        }
        let known = Collection::ALL
            .into_iter()
            .any(|collection| collection.dir_name() == Some(name.as_str()));
        if !known {
            for child in fs::read_dir(entry.path())? {
                anyhow::ensure!(
                    !child?.path().join("object.json").exists(),
                    "unsupported document collection directory {name}"
                );
            }
        }
    }
    let compact = root.join("pack_config.json");
    let mut locations = BTreeMap::new();
    let mut handles = Vec::new();
    let value = if compact.exists() {
        anyhow::ensure!(
            !root.join("agent_principal.json").exists(),
            "mixed compact and per-document manifest roots"
        );
        for collection in Collection::ALL {
            if let Some(directory) = collection.dir_name() {
                let path = root.join(directory);
                if path.is_dir() {
                    for entry in fs::read_dir(path)? {
                        anyhow::ensure!(
                            !entry?.path().join("object.json").exists(),
                            "mixed compact and per-document manifest roots"
                        );
                    }
                }
            }
        }
        read_json(&compact)?
    } else {
        let mut object = Map::new();
        object.insert(
            "agent_principal".into(),
            read_json(&root.join("agent_principal.json"))?,
        );
        for collection in Collection::ALL {
            let Some(directory) = collection.dir_name() else {
                continue;
            };
            let legacy = directory.replace('_', "-");
            anyhow::ensure!(
                legacy == directory || !root.join(&legacy).exists(),
                "unsupported directory {legacy}; use {directory}"
            );
            let path = root.join(directory);
            if !path.exists() {
                continue;
            }
            anyhow::ensure!(
                path.is_dir(),
                "manifest collection is not a directory: {}",
                path.display()
            );
            let mut entries = fs::read_dir(path)?.collect::<std::io::Result<Vec<_>>>()?;
            entries.sort_by_key(|entry| entry.file_name());
            let mut rows = Vec::new();
            for entry in entries {
                let handle = entry
                    .file_name()
                    .into_string()
                    .map_err(|_| anyhow::anyhow!("document handle is not UTF-8"))?;
                if handle.starts_with('.') || !entry.path().is_dir() {
                    continue;
                }
                let path = entry.path();
                let row = read_json(&path.join("object.json"))?;
                let index = rows.len();
                // Decode once through the common owner before checking the logical ID.
                locations.insert((collection, index), path);
                handles.push((collection, index, handle));
                rows.push(row);
            }
            object.insert(directory.into(), Value::Array(rows));
        }
        Value::Object(object)
    };
    // Sidecar locations use decoded exact IDs, never path fragments supplied as IDs.
    let config = gents::pack::decode_pack_config(
        value,
        None,
        &|name| std::env::var(name).ok(),
        &|collection, id, reference| {
            let directory = if compact.exists() {
                root.to_path_buf()
            } else {
                let handle = super::document_handle(id);
                let (_, index, _) = handles
                    .iter()
                    .find(|(candidate, _, stored)| *candidate == collection && stored == &handle)
                    .context("sidecar document has a noncanonical filesystem handle")?;
                locations
                    .get(&(collection, *index))
                    .context("sidecar document location missing")?
                    .clone()
            };
            let mut value = Some(reference.to_owned());
            hydrate_sidecar(&mut value, &directory).map_err(anyhow::Error::msg)?;
            value.context("sidecar contents missing")
        },
    )?;
    let plan = gents::config_client::DesiredStateApplyPlan::from_pack_config(&config)?;
    let bundle = serde_json::to_value(&config)?;
    for (collection, index, handle) in handles {
        let row = &bundle[collection.dir_name().expect("directory collection")][index];
        let id = row[collection.unique_field()]
            .as_str()
            .context("document identity missing")?;
        anyhow::ensure!(
            super::document_handle(id) == handle,
            "directory name '{handle}' does not match {} '{id}'",
            collection.unique_field()
        );
    }
    // Offline validation checks canonical shape, duplicate IDs and owner scope.
    // Existence closure belongs to the transaction over retained + authored docs.
    gents::document_config::ConfigReferences::from_documents(
        &config.agent_principal.agent_did,
        plan.documents()
            .iter()
            .map(|doc| (doc.collection, doc.add.clone())),
    )?;
    Ok(config)
}

fn read_json(path: &Path) -> Result<Value> {
    let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("invalid JSON in {}", path.display()))
}

pub(crate) fn hydrate_sidecar(value: &mut Option<String>, json_dir: &Path) -> Result<(), String> {
    let Some(reference) = value.as_deref().filter(|value| value.starts_with("./")) else {
        return Ok(());
    };
    let relative = PathBuf::from(&reference[2..]);
    if relative
        .components()
        .any(|component| !matches!(component, std::path::Component::Normal(_)))
    {
        return Err(format!(
            "sidecar path escapes document directory or is not canonical: {reference}"
        ));
    }
    let root = fs::canonicalize(json_dir).map_err(|error| error.to_string())?;
    let path = fs::canonicalize(root.join(relative))
        .map_err(|error| format!("sidecar path does not resolve: {reference}: {error}"))?;
    if !path.starts_with(&root) {
        return Err(format!(
            "sidecar path escapes document directory: {reference}"
        ));
    }
    *value = Some(
        fs::read_to_string(&path)
            .map_err(|error| format!("reading sidecar {}: {error}", path.display()))?,
    );
    Ok(())
}

#[cfg(test)]
mod filesystem_tests {
    use super::*;
    use serde_json::json;

    fn config() -> DesiredStateManifest {
        gents::pack::decode_pack_config(json!({
            "agent_principal":{"agent_did":"owner"},
            "contexts":[{"context_id":"context","system_prompt":"  literal ${NOT_EXPANDED} {{node.did}}\n"}],
            "inference_backends":[{"backend_id":"backend","name":"backend","provider_kind":"OpenAiCompatible","endpoint":"http://localhost:8000/v1","auth":{"kind":"unauthenticated"}}],
            "inference_profiles":[{"profile_id":"profile","backend_id":"backend","model_name":"model"}],
            "agent_behaviors":[{"behavior_id":"behavior","context_id":"context","inference_profile_id":"profile"}],
            "tasks":[{"task_id":"task","behavior_id":"behavior","prompt_template":"  {{args.name}}\n"}]
        }),None,&|name|(name=="NOT_EXPANDED").then(||"${NOT_EXPANDED}".into()),&|_,_,_|unreachable!()).unwrap()
    }

    #[test]
    fn compact_export_roundtrips_literal_sidecars_and_rejects_mixed_roots() {
        let dir = tempfile::tempdir().unwrap();
        let expected = config();
        super::super::write::write_manifest_root(dir.path(), &expected, false).unwrap();
        assert!(dir.path().join("pack_config.json").is_file());
        let (actual, report) = load_manifest_root(dir.path());
        assert!(report.ok, "{:?}", report.errors);
        assert_eq!(
            serde_json::to_value(actual.unwrap()).unwrap(),
            serde_json::to_value(expected).unwrap()
        );
        fs::write(dir.path().join("agent_principal.json"), "{}").unwrap();
        assert!(!load_manifest_root(dir.path()).1.ok);
    }

    #[test]
    fn per_document_roots_use_canonical_defaults_and_reject_foreign_owner() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("agent_principal.json"),
            r#"{"agent_did":"owner"}"#,
        )
        .unwrap();
        let context_dir = dir.path().join("contexts/context");
        fs::create_dir_all(&context_dir).unwrap();
        fs::write(
            context_dir.join("object.json"),
            r#"{"context_id":"context","system_prompt":"./prompt.md"}"#,
        )
        .unwrap();
        fs::write(
            context_dir.join("prompt.md"),
            "  {{node.did}} ${UNSET_LITERAL}\n",
        )
        .unwrap();
        let (loaded, report) = load_manifest_root(dir.path());
        assert!(report.ok, "{:?}", report.errors);
        let context = &loaded.unwrap().contexts[0];
        assert_eq!(context.agent_did, "owner");
        assert_eq!(
            context.system_prompt.as_deref(),
            Some("  {{node.did}} ${UNSET_LITERAL}\n")
        );
        fs::write(
            context_dir.join("object.json"),
            r#"{"agent_did":"foreign","context_id":"context"}"#,
        )
        .unwrap();
        assert!(!load_manifest_root(dir.path()).1.ok);
    }

    #[test]
    fn malformed_unknown_and_escaping_sidecar_authoring_fails() {
        let dir = tempfile::tempdir().unwrap();
        for text in [
            "{broken",
            r#"{"agent_principal":{"agent_did":"owner"},"unknown":true}"#,
            r#"{"agent_principal":{"agent_did":"owner"},"contexts":[{"context_id":"context","system_prompt":"./../outside.md"}]}"#,
        ] {
            fs::write(dir.path().join("pack_config.json"), text).unwrap();
            assert!(!load_manifest_root(dir.path()).1.ok, "{text}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn sidecars_cannot_escape_through_symlinks() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::NamedTempFile::new().unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("linked.md")).unwrap();
        assert!(hydrate_sidecar(&mut Some("./linked.md".into()), dir.path()).is_err());
    }

    #[test]
    fn invalid_export_does_not_destroy_existing_manifest() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = config();
        super::super::write::write_manifest_root(dir.path(), &config, false).unwrap();
        let path = dir.path().join("pack_config.json");
        let before = fs::read(&path).unwrap();
        config.agent_behaviors[0].agent_did = "foreign".into();
        assert!(super::super::write::write_manifest_root(dir.path(), &config, true).is_err());
        assert_eq!(fs::read(path).unwrap(), before);
    }
}

#[cfg(test)]
mod filesystem_boundary_tests {
    use super::*;

    fn root() -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("agent_principal.json"),
            r#"{"agent_did":"owner"}"#,
        )
        .unwrap();
        dir
    }
    #[test]
    fn missing_document_and_collection_shape_errors_are_visible() {
        let empty = tempfile::tempdir().unwrap();
        assert!(!load_manifest_root(empty.path()).1.ok);
        let dir = root();
        fs::write(dir.path().join("contexts"), "not a directory").unwrap();
        assert!(!load_manifest_root(dir.path()).1.ok);
        fs::remove_file(dir.path().join("contexts")).unwrap();
        fs::create_dir_all(dir.path().join("contexts/context")).unwrap();
        assert!(!load_manifest_root(dir.path()).1.ok);
    }
    #[test]
    fn canonical_handles_duplicates_and_unknown_collections_are_checked() {
        for (directory, ids) in [
            ("contexts", vec![("wrong", "context")]),
            ("contexts", vec![("one", "same"), ("two", "same")]),
            ("tool_selections", vec![("old", "old")]),
        ] {
            let dir = root();
            for (handle, id) in ids {
                let path = dir.path().join(directory).join(handle);
                fs::create_dir_all(&path).unwrap();
                fs::write(
                    path.join("object.json"),
                    serde_json::to_vec(&serde_json::json!({"context_id":id})).unwrap(),
                )
                .unwrap();
            }
            assert!(!load_manifest_root(dir.path()).1.ok, "{directory}");
        }
        let dir = root();
        fs::create_dir_all(dir.path().join("contexts/.hidden")).unwrap();
        fs::write(dir.path().join("contexts/notes.md"), "ignored sibling").unwrap();
        assert!(load_manifest_root(dir.path()).1.ok);
    }
    #[test]
    fn sidecar_literal_none_missing_and_utf8_semantics() {
        let dir = root();
        for value in [
            None,
            Some("literal".into()),
            Some("/absolute-is-literal".into()),
            Some("../parent-is-literal".into()),
        ] {
            let mut actual = value.clone();
            hydrate_sidecar(&mut actual, dir.path()).unwrap();
            assert_eq!(actual, value);
        }
        for path in [
            "./missing.md",
            "./../outside.md",
            "./nested/../../outside.md",
        ] {
            assert!(hydrate_sidecar(&mut Some(path.into()), dir.path()).is_err());
        }
        fs::write(dir.path().join("bad.md"), [0xff, 0xfe]).unwrap();
        assert!(hydrate_sidecar(&mut Some("./bad.md".into()), dir.path()).is_err());
    }
}

#[cfg(test)]
mod interpolation_tests {
    use super::*;
    #[test]
    fn shared_decoder_interpolates_values_after_json_parse_and_enforces_owner() {
        let value = serde_json::json!({"agent_principal":{"agent_did":"owner"},"contexts":[{"context_id":"context","description":"${DESCRIPTION}"}]});
        let config = gents::pack::decode_pack_config(
            value.clone(),
            None,
            &|name| (name == "DESCRIPTION").then(|| "quoted \"value\"\nnext".into()),
            &|_, _, _| unreachable!(),
        )
        .unwrap();
        assert_eq!(
            config.contexts[0].description.as_deref(),
            Some("quoted \"value\"\nnext")
        );
        assert!(
            gents::pack::decode_pack_config(value, None, &|_| None, &|_, _, _| unreachable!())
                .is_err()
        );
        let config = gents::pack::decode_pack_config(
            serde_json::json!({"agent_principal":{"agent_did":"${GENTS_PACK_AGENT_DID}"}}),
            Some(&gents::pack::PackInstallOptions {
                agent_did: "selected-owner".into(),
            }),
            &|_| Some("spoofed-owner".into()),
            &|_, _, _| unreachable!(),
        )
        .unwrap();
        assert_eq!(config.agent_principal.agent_did, "selected-owner");
    }
}

#[cfg(test)]
mod retained_reference_tests {
    use super::*;
    #[test]
    fn offline_load_and_export_allow_references_to_retained_documents() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("pack_config.json"),r#"{"agent_principal":{"agent_did":"owner"},"agent_behaviors":[{"behavior_id":"behavior","inference_profile_id":"retained-profile"}]}"#).unwrap();
        let (config, report) = load_manifest_root(dir.path());
        assert!(report.ok, "{:?}", report.errors);
        let config = config.unwrap();
        assert!(config.inference_profiles.is_empty());
        assert_eq!(
            config.agent_behaviors[0].inference_profile_id,
            "retained-profile"
        );
        let exported = tempfile::tempdir().unwrap();
        super::super::write::write_manifest_root(exported.path(), &config, false).unwrap();
        assert!(load_manifest_root(exported.path()).1.ok);
    }
}
