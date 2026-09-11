use super::{DesiredStateManifest, DesiredStateValidationReport};
use anyhow::{Context, Result};
use serde_json::Value;
use std::{
    fs,
    path::{Path, PathBuf},
};

pub(crate) fn load_manifest_root(
    root: &Path,
) -> (Option<DesiredStateManifest>, DesiredStateValidationReport) {
    load_manifest_root_for_owner(root, None)
}

pub(crate) fn load_manifest_root_for_owner(
    root: &Path,
    owner: Option<&str>,
) -> (Option<DesiredStateManifest>, DesiredStateValidationReport) {
    let mut errors = Vec::new();
    let manifest = match load_root(root, owner) {
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

fn load_root(root: &Path, owner: Option<&str>) -> Result<DesiredStateManifest> {
    anyhow::ensure!(
        root.is_dir(),
        "manifest root is not a directory: {}",
        root.display()
    );
    let compact = root.join("pack_config.json");
    anyhow::ensure!(
        compact.is_file(),
        "manifest root is missing canonical pack_config.json: {}",
        root.display()
    );
    let value = read_json(&compact)?;
    let config = gents::pack::decode_pack_config(
        value,
        owner
            .map(|agent_did| gents::pack::PackInstallOptions {
                agent_did: agent_did.to_owned(),
            })
            .as_ref(),
        &|name| std::env::var(name).ok(),
        &|_, _, reference| {
            let mut value = Some(reference.to_owned());
            hydrate_sidecar(&mut value, root).map_err(anyhow::Error::msg)?;
            value.context("sidecar contents missing")
        },
    )?;
    let plan = gents::config_client::DesiredStateApplyPlan::from_pack_config(&config)?;
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
    fn compact_export_roundtrips_literal_sidecars() {
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

    #[test]
    fn sidecar_literal_none_missing_and_utf8_semantics() {
        let dir = tempfile::tempdir().unwrap();
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
