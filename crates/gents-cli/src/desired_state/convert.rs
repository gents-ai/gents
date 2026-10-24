use super::DesiredStateManifest;
use crate::shared::ConfigExportBundle;
use serde_json::Value;
use std::path::PathBuf;

/// Shared by the desired-state load, convert and validation paths; Display text
/// is shown to operators and model-facing tool results, keep it stable.
#[derive(Debug, thiserror::Error)]
pub(crate) enum DesiredStateError {
    #[error("export envelope and node owners differ")]
    EnvelopeOwnerMismatch,
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("canonical pack must be an object")]
    PackNotObject,
    #[error("canonical pack field {name} is not a document root")]
    PackFieldNotDocumentRoot { name: String },
    #[error("export contains foreign-owned {name}")]
    ForeignOwnedField { name: String },
    #[error("reading {}", path.display())]
    ReadFile {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("invalid JSON in {}", path.display())]
    InvalidJson {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("invalid pack manifest {}", path.display())]
    InvalidPackManifest {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
}

type Result<T, E = DesiredStateError> = std::result::Result<T, E>;

pub(crate) fn manifest_from_export_bundle(
    bundle: &ConfigExportBundle,
) -> Result<DesiredStateManifest> {
    if bundle.node_did != bundle.config.node.node_did {
        return Err(DesiredStateError::EnvelopeOwnerMismatch);
    }
    let encoded = serde_json::to_value(&bundle.config)?;
    for (name, value) in encoded
        .as_object()
        .ok_or(DesiredStateError::PackNotObject)?
    {
        let rows = match value {
            Value::Array(rows) => rows.as_slice(),
            Value::Object(_) => std::slice::from_ref(value),
            _ => return Err(DesiredStateError::PackFieldNotDocumentRoot { name: name.clone() }),
        };
        for row in rows {
            if row.get("node_did").and_then(Value::as_str) != Some(bundle.node_did.as_str()) {
                return Err(DesiredStateError::ForeignOwnedField { name: name.clone() });
            }
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
