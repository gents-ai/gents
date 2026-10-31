use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};

#[derive(Debug, thiserror::Error)]
pub enum SchemaContractError {
    #[error("collection schema field is missing Name")]
    FieldNameMissing,
    #[error("collection index field is missing Name")]
    IndexFieldNameMissing,
    #[error(transparent)]
    Serialize(#[from] serde_json::Error),
    #[error("collection constraints do not match requested schema")]
    ConstraintsMismatch,
    #[error("field {name:?} does not match requested schema")]
    FieldMismatch { name: String },
}

type Result<T, E = SchemaContractError> = std::result::Result<T, E>;

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
            .ok_or(SchemaContractError::FieldNameMissing)?;
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
        .map(|index| -> Result<SchemaIndexContract> {
            let fields = index
                .get("Fields")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .map(|field| -> Result<(String, bool)> {
                    Ok((
                        field
                            .get("Name")
                            .and_then(Value::as_str)
                            .ok_or(SchemaContractError::IndexFieldNameMissing)?
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

pub(crate) fn collection_schema_contract_digest(version: &Value) -> Result<String> {
    let contract = collection_schema_contract(version)?;
    Ok(format!(
        "sha256:{:x}",
        Sha256::digest(serde_json::to_vec(&contract)?)
    ))
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
pub struct SchemaFieldDelta {
    pub pending: BTreeSet<String>,
    pub extra: BTreeSet<String>,
}

pub(crate) fn collection_schema_field_delta(
    expected: &Value,
    live: &Value,
) -> Result<SchemaFieldDelta> {
    let expected = collection_schema_contract(expected)?;
    let live = collection_schema_contract(live)?;
    if expected.indexes != live.indexes
        || expected.branchable != live.branchable
        || expected.embedded_only != live.embedded_only
    {
        return Err(SchemaContractError::ConstraintsMismatch);
    }
    for (name, field) in &expected.fields {
        if let Some(existing) = live.fields.get(name) {
            if existing != field {
                return Err(SchemaContractError::FieldMismatch { name: name.clone() });
            }
        }
    }
    Ok(SchemaFieldDelta {
        pending: expected
            .fields
            .keys()
            .filter(|name| !live.fields.contains_key(*name))
            .cloned()
            .collect(),
        extra: live
            .fields
            .keys()
            .filter(|name| !expected.fields.contains_key(*name))
            .cloned()
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn additive_delta_rejects_shared_field_and_collection_constraint_drift() {
        let parsed = query::parse_sdl("type Widget { message: String }").unwrap();
        let expected = serde_json::to_value(&parsed[0]).unwrap();
        for (key, changed) in [
            ("Kind", json!("Int")),
            ("Typ", json!("changed-crdt")),
            ("RelationName", json!("changed-relation")),
            ("IsPrimary", json!(true)),
            ("DefaultValue", json!("changed-default")),
            ("Size", json!(27)),
            ("Immutable", json!(true)),
        ] {
            let mut live = expected.clone();
            let field = live["Fields"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .find(|field| field["Name"] == "message")
                .unwrap();
            field[key] = changed;
            assert!(
                collection_schema_field_delta(&expected, &live).is_err(),
                "{key}"
            );
        }
        for (key, changed) in [
            ("IsBranchable", json!(true)),
            ("IsEmbeddedOnly", json!(true)),
            (
                "Indexes",
                json!([{"Fields":[{"Name":"message"}], "Unique":true}]),
            ),
        ] {
            let mut live = expected.clone();
            live[key] = changed;
            assert!(
                collection_schema_field_delta(&expected, &live).is_err(),
                "{key}"
            );
        }
    }
}
