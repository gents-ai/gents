use std::collections::BTreeMap;
use std::path::Path;

use anyhow::Result;
use serde_json::Value;

use crate::pack::store::StoredName;
use crate::pack::PackKind;
use gents_store::config_client::ConfigAccess;

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
    use gents_protocol::document_query::{
        fields::pack_installation as fields, Direction, Filter, Query, Selection,
    };
    let mut installed = BTreeMap::new();
    let rendered = Query::select(
        Selection::new()
            .field(fields::COORDINATE)
            .field(fields::VERSION)
            .field(fields::DIGEST)
            .field(fields::PLUGIN_STORE)
            .field(fields::SOURCE)
            .field(fields::SOURCE_HISTORY),
    )
    .filter(Filter::eq(fields::NODE_DID, owner))
    .order_by(fields::COORDINATE, Direction::Asc)
    .render();
    let response = access.read(&rendered).await?;
    let records = response["data"]["PackInstallation"]
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("reading pack installation records"))?;
    for record in records
        .iter()
        .filter(|row| !super::is_plugin_store_row(row))
    {
        let coordinate = super::required_record_field_str(record, "coordinate")?;
        let version = super::required_record_field_str(record, "version")?;
        let digest = super::required_record_field_str(record, "digest")?;
        let public_record = serde_json::json!({
            "coordinate": coordinate, "version": version, "digest": digest,
            "source": record["source"], "source_history": record["source_history"],
        });
        let entry = InstalledCatalogEntry {
            stored: StoredName {
                version: version.to_owned(),
                digest: digest.to_owned(),
            },
            record: public_record,
            kind: PackKind::Documents,
        };
        installed.insert(coordinate.to_owned(), entry);
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
