use anyhow::Result;
use gents_protocol::job::{DataOp, DataWrite};
use serde_json::{json, Map, Value};
use std::collections::BTreeSet;

use crate::config_client::ConfigApplyTxn;
use crate::plugin::data::{
    admit_write, insert_note, AdmittedWrite, InsertOutcome, OnConflict, PluginData, StampContext,
};

#[derive(Debug, thiserror::Error)]
pub(crate) enum PluginWritesError {
    #[error("{name} affected {count} documents, expected 1; check the doc_id")]
    AffectedCount { name: String, count: usize },
    #[error("an update needs at least one field")]
    UpdateNeedsField,
    #[error("collection {collection} is not installed")]
    CollectionNotInstalled { collection: String },
    #[error("internal field {name:?} cannot be written")]
    InternalField { name: String },
    #[error("unknown field {name:?} in {collection}")]
    UnknownField { name: String, collection: String },
    #[error("invalid field `{name}`")]
    InvalidField {
        name: String,
        #[source]
        source: crate::defra_write::DefraWriteFailure,
    },
    #[error(
        "on_conflict needs a collection with exactly one unique key; {collection} has {count}"
    )]
    UniqueKeyCount { collection: String, count: usize },
    #[error("on_conflict needs the unique key field {field:?} in fields")]
    MissingKeyField { field: String },
    #[error("row is not an object")]
    RowNotObject,
    #[error("duplicate {collection} {label}")]
    DuplicateRow { collection: String, label: String },
}

/// Applies a plugin's data writes in the caller's transaction and returns one
/// receipt per write. Any refused or failed write fails the whole output; the
/// caller's rollback discards the rest.
///
/// `txn` must be opened as the invoker: `admit_write` admits each write on
/// that precondition. This opens no transaction of its own.
pub async fn apply_plugin_writes_in_txn(
    txn: &ConfigApplyTxn<'_>,
    grant: &PluginData,
    stamps: &StampContext,
    writes: Vec<DataWrite>,
) -> Result<Vec<Value>> {
    let admitted = admit_write(grant, stamps, writes)?;
    let mut receipts = Vec::with_capacity(admitted.len());
    for AdmittedWrite { write } in admitted {
        receipts.push(apply_one(txn, grant, write).await?);
    }
    Ok(receipts)
}

async fn apply_one(
    txn: &ConfigApplyTxn<'_>,
    grant: &PluginData,
    write: DataWrite,
) -> Result<Value> {
    if let Some(on_conflict) = write.on_conflict {
        if let Some(receipt) = resolve_conflict(txn, grant, &write, on_conflict).await? {
            return Ok(receipt);
        }
    }
    let collection = write.collection.as_str();
    // Admission requires a doc_id for update and delete.
    let filter =
        || crate::defra_query::render::render_filter(&json!({"_docID": {"_eq": write.doc_id}}));
    let (name, arguments) = match write.op {
        DataOp::Insert => (
            format!("add_{collection}"),
            format!("input: {{ {} }}", input(txn, &write).await?),
        ),
        DataOp::Update => (
            format!("update_{collection}"),
            format!(
                "filter: {}, input: {{ {} }}",
                filter()?,
                input(txn, &write).await?
            ),
        ),
        DataOp::Delete => (
            format!("delete_{collection}"),
            format!("filter: {}", filter()?),
        ),
    };
    let response = txn
        .execute(&format!("mutation {{ {name}({arguments}) {{ _docID }} }}"))
        .await?;
    let rows = match &response["data"][&name] {
        Value::Array(rows) => rows.clone(),
        row if row.is_object() => vec![row.clone()],
        _ => Vec::new(),
    };
    if rows.len() != 1 {
        return Err(PluginWritesError::AffectedCount {
            name,
            count: rows.len(),
        }
        .into());
    }
    Ok(rows[0].clone())
}

async fn input(txn: &ConfigApplyTxn<'_>, write: &DataWrite) -> Result<String> {
    let collection = write.collection.as_str();
    if write.op != DataOp::Insert && write.fields.is_empty() {
        return Err(PluginWritesError::UpdateNeedsField.into());
    }
    let version: ::schema::CollectionVersion =
        serde_json::from_value(txn.collection_version(collection).await?.ok_or_else(|| {
            PluginWritesError::CollectionNotInstalled {
                collection: collection.to_owned(),
            }
        })?)?;
    let mut parts = Vec::with_capacity(write.fields.len());
    for (name, value) in &write.fields {
        crate::graphql::validate_graphql_name(name)?;
        if name.starts_with('_') {
            return Err(PluginWritesError::InternalField { name: name.clone() }.into());
        }
        let field = version
            .fields
            .iter()
            .find(|f| &f.name == name)
            .ok_or_else(|| PluginWritesError::UnknownField {
                name: name.clone(),
                collection: collection.to_owned(),
            })?;
        let literal = crate::defra_write::field_literal(field.kind.graphql_type_name(), value)
            .map_err(|source| PluginWritesError::InvalidField {
                name: name.clone(),
                source,
            })?;
        parts.push(format!("{name}: {literal}"));
    }
    Ok(parts.join(", "))
}

/// Reads the row already stored under the collection's one unique key, in the
/// caller's transaction, and lets `insert_note` decide. `Some(receipt)` ends the
/// write; `None` means insert. The unique index only backstops this read.
async fn resolve_conflict(
    txn: &ConfigApplyTxn<'_>,
    grant: &PluginData,
    write: &DataWrite,
    on_conflict: OnConflict,
) -> Result<Option<Value>> {
    let collection = write.collection.as_str();
    crate::graphql::validate_graphql_name(collection)?;
    let keys = grant
        .unique_keys
        .get(collection)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let [key] = keys else {
        return Err(PluginWritesError::UniqueKeyCount {
            collection: collection.to_owned(),
            count: keys.len(),
        }
        .into());
    };
    let mut filter = Map::new();
    let mut label = Vec::with_capacity(key.len());
    for field in key {
        let value = write
            .fields
            .get(field)
            .ok_or_else(|| PluginWritesError::MissingKeyField {
                field: field.clone(),
            })?;
        filter.insert(field.clone(), json!({"_eq": value}));
        label.push(match value {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        });
    }
    // The guest's own fields: host-stamped values differ between replays.
    let guest: Vec<&str> = write
        .fields
        .keys()
        .filter(|field| !grant.declaration.stamped.contains_key(*field))
        .map(String::as_str)
        .collect();
    let mut selection: BTreeSet<&str> = guest.iter().copied().collect();
    selection.insert("_docID");
    if grant
        .collection_fields
        .get(collection)
        .is_some_and(|fields| fields.iter().any(|f| f == "created_at"))
    {
        selection.insert("created_at");
    }
    for field in &selection {
        crate::graphql::validate_graphql_name(field)?;
    }
    let response = txn
        .execute(&format!(
            "{{ {collection}(filter: {}) {{ {} }} }}",
            crate::defra_query::render::render_filter(&Value::Object(filter))?,
            selection.into_iter().collect::<Vec<_>>().join(" ")
        ))
        .await?;
    let rows = response["data"][collection].as_array().map(Vec::as_slice);
    // Unique indexes are node-local, so P2P twins can coexist: every reader
    // picks the earliest created_at, then the lowest _docID.
    let existing = rows.unwrap_or_default().iter().min_by_key(|row| {
        (
            row["created_at"].as_str().map(str::to_owned),
            row["_docID"].as_str().map(str::to_owned),
        )
    });
    let fingerprint = |row: &Map<String, Value>| {
        let fields: std::collections::BTreeMap<&str, Value> = guest
            .iter()
            .map(|field| (*field, canonical(row.get(*field).unwrap_or(&Value::Null))))
            .collect();
        serde_json::to_string(&fields)
    };
    let new = fingerprint(&write.fields)?;
    let old = match existing {
        Some(row) => Some(fingerprint(
            row.as_object().ok_or(PluginWritesError::RowNotObject)?,
        )?),
        None => None,
    };
    Ok(match insert_note(old.as_deref(), &new, on_conflict) {
        InsertOutcome::Inserted => None,
        InsertOutcome::Unchanged if old.as_deref() == Some(&new) => {
            Some(json!({"_docID": existing.map(|row| &row["_docID"])}))
        }
        InsertOutcome::Unchanged => Some(json!({"duplicate": true})),
        InsertOutcome::AbortStep => {
            return Err(PluginWritesError::DuplicateRow {
                collection: collection.to_owned(),
                label: label.join(","),
            }
            .into())
        }
    })
}

/// A row read back and a guest value compare equal when they store the same
/// thing: an empty list is written as null, and a number may read back as a float.
fn canonical(value: &Value) -> Value {
    match value {
        Value::Array(items) if items.is_empty() => Value::Null,
        Value::Array(items) => Value::Array(items.iter().map(canonical).collect()),
        Value::Number(number) => json!(number.as_f64()),
        other => other.clone(),
    }
}
