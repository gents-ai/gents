use super::DesiredStateManifest;
use crate::shared::ConfigExportBundle;
use anyhow::{Context, Result};
use serde_json::Value;

pub(crate) fn manifest_from_export_bundle(
    bundle: &ConfigExportBundle,
) -> Result<DesiredStateManifest> {
    anyhow::ensure!(
        bundle.node_did == bundle.config.node.node_did,
        "export envelope and node owners differ"
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
                row.get("node_did").and_then(Value::as_str) == Some(bundle.node_did.as_str()),
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
        node_did: manifest.node.node_did.clone(),
        exported_at: chrono::Utc::now().to_rfc3339(),
        access_mode: access_mode.into(),
        config: manifest.clone(),
    };
    manifest_from_export_bundle(&bundle)?;
    Ok(super::DesiredApplyBundle::from_trusted_bundle(bundle))
}
