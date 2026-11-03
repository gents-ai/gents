use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use serde_json::Value;

use crate::pack::store::StoredName;
use crate::pack::PackKind;
use crate::store::config_client::ConfigAccess;

/// A pack installed on a node and the stored artifact its installation names.
pub struct InstalledCatalogEntry {
    /// The installation record, as the pack catalog reports it.
    pub record: Value,
    pub kind: PackKind,
    pub stored: StoredName,
}

/// The packs installed on `owner`, by coordinate. A document pack is read from
/// its `PackInstallation` record, never from the home's copy of it; other kinds
/// are read from `home`'s install records. A coordinate installed both ways is
/// refused.
pub async fn installed_catalog(
    access: &ConfigAccess,
    owner: &str,
    home: Option<&Path>,
) -> Result<BTreeMap<String, InstalledCatalogEntry>> {
    let mut installed = BTreeMap::new();
    for record in super::list_installed_packs(access, owner).await? {
        let entry = InstalledCatalogEntry {
            stored: StoredName {
                version: record.version.clone(),
                digest: record.digest.clone(),
            },
            record: serde_json::to_value(&record)?,
            kind: PackKind::Documents,
        };
        installed.insert(record.coordinate, entry);
    }
    let Some(home) = home else {
        return Ok(installed);
    };
    for record in crate::pack::list_home_installs(home)? {
        if record.kind == PackKind::Documents {
            continue;
        }
        anyhow::ensure!(
            !installed.contains_key(&record.coordinate),
            "pack {} has conflicting installed kinds",
            record.coordinate
        );
        let entry = InstalledCatalogEntry {
            stored: StoredName {
                version: record.version.clone(),
                digest: record.digest.clone(),
            },
            record: serde_json::to_value(&record)?,
            kind: record.kind.clone(),
        };
        installed.insert(record.coordinate, entry);
    }
    Ok(installed)
}
