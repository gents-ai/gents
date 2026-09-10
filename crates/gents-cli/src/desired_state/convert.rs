use super::DesiredStateManifest;
use crate::shared::ConfigExportBundle;
use anyhow::{Context, Result};
use serde_json::Value;

pub(crate) fn manifest_from_export_bundle(
    bundle: &ConfigExportBundle,
) -> Result<DesiredStateManifest> {
    anyhow::ensure!(
        bundle.agent_did == bundle.config.agent_principal.agent_did,
        "export envelope and principal owners differ"
    );
    let encoded = serde_json::to_value(&bundle.config)?;
    for (name, value) in encoded
        .as_object()
        .context("canonical pack must be an object")?
    {
        let rows = match value {
            Value::Array(rows) => rows.as_slice(),
            Value::Object(_) => std::slice::from_ref(value),
            _ => anyhow::bail!("canonical pack field {name} is not a document root"),
        };
        for row in rows {
            anyhow::ensure!(
                row.get("agent_did").and_then(Value::as_str) == Some(bundle.agent_did.as_str()),
                "export contains foreign-owned {name}"
            );
        }
    }
    Ok(bundle.config.clone())
}

pub(crate) fn export_bundle_from_manifest(
    manifest: &DesiredStateManifest,
    access_mode: &str,
) -> Result<super::DesiredApplyBundle> {
    let bundle = ConfigExportBundle {
        format: crate::CONFIG_EXPORT_FORMAT.into(),
        agent_did: manifest.agent_principal.agent_did.clone(),
        exported_at: chrono::Utc::now().to_rfc3339(),
        access_mode: access_mode.into(),
        config: manifest.clone(),
    };
    manifest_from_export_bundle(&bundle)?;
    Ok(super::DesiredApplyBundle::from_trusted_bundle(bundle))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn import_preserves_canonical_roots_and_rejects_cross_owner_documents() {
        let config: DesiredStateManifest = serde_json::from_value(json!({"agent_principal":{"agent_did":"owner"},"contexts":[{"context_id":"context","agent_did":"owner","system_prompt":"literal {{ text }}"}],"tools":[{"tools_id":"unused","agent_did":"owner","remote":{"services":[]}}]})).unwrap();
        let wrapped = export_bundle_from_manifest(&config, "test").unwrap();
        let loaded = manifest_from_export_bundle(wrapped.as_bundle()).unwrap();
        assert_eq!(
            serde_json::to_value(loaded).unwrap(),
            serde_json::to_value(&config).unwrap()
        );
        let mut foreign = wrapped.as_bundle().clone();
        foreign.config.tools[0].agent_did = "other".into();
        assert!(manifest_from_export_bundle(&foreign).is_err());
        foreign.config = config;
        foreign.agent_did = "other".into();
        assert!(manifest_from_export_bundle(&foreign).is_err());
    }
}
