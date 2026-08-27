use std::collections::BTreeMap;

use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct SchemaFieldContract {
    kind: Value,
    crdt_type: Value,
    relation_name: Value,
    is_primary: bool,
    default_value: Value,
    size: u64,
    immutable: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
struct SchemaIndexContract {
    fields: Vec<(String, bool)>,
    unique: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
struct CollectionSchemaContract {
    fields: BTreeMap<String, SchemaFieldContract>,
    indexes: Vec<SchemaIndexContract>,
    branchable: bool,
    embedded_only: bool,
}

fn collection_schema_contract(version: &Value) -> Result<CollectionSchemaContract> {
    let mut fields = BTreeMap::new();
    for field in version
        .get("Fields")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let name = field
            .get("Name")
            .and_then(Value::as_str)
            .context("collection schema field is missing Name")?;
        if name == "_docID" {
            continue;
        }
        fields.insert(
            name.to_owned(),
            SchemaFieldContract {
                kind: field.get("Kind").cloned().unwrap_or(Value::Null),
                crdt_type: field.get("Typ").cloned().unwrap_or(Value::Null),
                relation_name: field.get("RelationName").cloned().unwrap_or(Value::Null),
                is_primary: field
                    .get("IsPrimary")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                default_value: field.get("DefaultValue").cloned().unwrap_or(Value::Null),
                size: field.get("Size").and_then(Value::as_u64).unwrap_or(0),
                immutable: field
                    .get("Immutable")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            },
        );
    }
    let mut indexes = version
        .get("Indexes")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|index| {
            let fields = index
                .get("Fields")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|field| {
                    Ok((
                        field
                            .get("Name")
                            .and_then(Value::as_str)
                            .context("collection index field is missing Name")?
                            .to_owned(),
                        field
                            .get("Descending")
                            .and_then(Value::as_bool)
                            .unwrap_or(false),
                    ))
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(SchemaIndexContract {
                fields,
                unique: index
                    .get("Unique")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    indexes.sort();
    Ok(CollectionSchemaContract {
        fields,
        indexes,
        branchable: version
            .get("IsBranchable")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        embedded_only: version
            .get("IsEmbeddedOnly")
            .and_then(Value::as_bool)
            .unwrap_or(false),
    })
}

/// Canonical semantic digest of a DefraDB collection contract.
///
/// Both package installation and runtime replay readiness use this shared
/// control-plane boundary so they cannot silently validate different shapes.
pub(crate) fn collection_schema_contract_digest(version: &Value) -> Result<String> {
    let contract = collection_schema_contract(version)?;
    Ok(format!(
        "sha256:{:x}",
        Sha256::digest(serde_json::to_vec(&contract)?)
    ))
}
