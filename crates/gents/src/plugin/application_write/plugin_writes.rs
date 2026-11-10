use anyhow::{bail, ensure, Context, Result};
use gents_protocol::job::{DataOp, DataWrite};
use serde_json::{json, Map, Value};
use std::collections::{BTreeMap, BTreeSet};

use crate::plugin::data::{
    admit_write, insert_note, AdmittedWrite, InsertOutcome, OnConflict, PluginData, StampContext,
};
use crate::store::config_client::ConfigApplyTxn;

/// Applies a plugin's data writes in the caller's transaction and returns one
/// receipt per write. Any refused or failed write fails the whole output; the
/// caller's rollback discards the rest.
///
/// `actor` is bound by the host to the invoker. Runtime bookkeeping may share
/// the transaction, but plugin data reads and writes retain this actor.
pub async fn apply_plugin_writes_in_txn(
    txn: &ConfigApplyTxn<'_>,
    actor: &identity::Did,
    grant: &PluginData,
    stamps: &StampContext,
    writes: Vec<DataWrite>,
) -> Result<Vec<Value>> {
    apply_signed_plugin_writes_in_txn(txn, actor, grant, stamps, writes, &BTreeMap::new()).await
}

/// As [`apply_plugin_writes_in_txn`], for a transaction whose actor must sign
/// its own document graphs (a User's client-signed HTTP transaction): each
/// create, update and delete of a collection in `schemas` is signed against
/// the pack's own trusted SDL for it. On an embedded transaction the same
/// calls execute natively.
pub async fn apply_signed_plugin_writes_in_txn(
    txn: &ConfigApplyTxn<'_>,
    actor: &identity::Did,
    grant: &PluginData,
    stamps: &StampContext,
    writes: Vec<DataWrite>,
    schemas: &BTreeMap<String, String>,
) -> Result<Vec<Value>> {
    txn.validate_effect_actor(actor)?;
    ensure!(
        stamps.value(crate::plugin::data::Stamp::CallerDid) == Some(actor.as_str()),
        "plugin provenance must match its authenticated actor"
    );
    let admitted = admit_write(grant, stamps, writes)?;
    let mut receipts = Vec::with_capacity(admitted.len());
    for AdmittedWrite { write } in admitted {
        let schema = schemas.get(&write.collection).map(String::as_str);
        receipts.push(apply_one(txn, grant, write, schema).await?);
    }
    Ok(receipts)
}

async fn apply_one(
    txn: &ConfigApplyTxn<'_>,
    grant: &PluginData,
    write: DataWrite,
    schema: Option<&str>,
) -> Result<Value> {
    if let Some(on_conflict) = write.on_conflict {
        if let Some(receipt) = resolve_conflict(txn, grant, &write, on_conflict).await? {
            return Ok(receipt);
        }
    }
    if let Some(schema) = schema {
        return apply_signed(txn, &write, schema).await;
    }
    let collection = write.collection.as_str();
    // Admission requires a doc_id for update and delete.
    let filter = || {
        crate::tools::builtin::defra_query::render::render_filter(
            &json!({"_docID": {"_eq": write.doc_id}}),
        )
    };
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
    ensure!(
        rows.len() == 1,
        "{name} affected {} documents, expected 1; check the doc_id",
        rows.len()
    );
    Ok(rows[0].clone())
}

/// One write signed against the pack's trusted `schema`. Fields are checked
/// exactly as the native path checks them, then passed as variables.
async fn apply_signed(txn: &ConfigApplyTxn<'_>, write: &DataWrite, schema: &str) -> Result<Value> {
    let collection = write.collection.as_str();
    crate::store::graphql::validate_graphql_name(collection)?;
    let variables = || -> Result<Value> {
        let input: Map<String, Value> = write.fields.clone().into_iter().collect();
        Ok(json!({ "input": Value::Object(input) }))
    };
    let doc_id = || -> Result<String> {
        write
            .doc_id
            .clone()
            .context("an update or delete needs the doc_id of its document")
    };
    match write.op {
        DataOp::Insert => {
            input(txn, write).await?;
            let response = txn
                .execute_client_signed_create(
                    &format!(
                        "mutation($input: {collection}MutationInputArg!) {{ create_{collection}(input: $input) {{ _docID }} }}"
                    ),
                    &variables()?,
                    schema,
                )
                .await?;
            Ok(json!({ "_docID": crate::store::graphql::created_doc_id(&response, collection)? }))
        }
        DataOp::Update => {
            input(txn, write).await?;
            let doc = doc_id()?;
            let name = format!("update_{collection}");
            let response = txn
                .execute_client_signed_update(
                    &format!(
                        "mutation($input: {collection}MutationInputArg!) {{ {name}(docID: \"{}\", input: $input) {{ _docID }} }}",
                        crate::store::graphql::escape_graphql_string(&doc)
                    ),
                    &variables()?,
                    schema,
                )
                .await?;
            let rows = match &response["data"][&name] {
                Value::Array(rows) => rows.clone(),
                row if row.is_object() => vec![row.clone()],
                _ => Vec::new(),
            };
            ensure!(
                rows.len() == 1 && rows[0]["_docID"] == doc.as_str(),
                "{name} affected {} documents, expected 1; check the doc_id",
                rows.len()
            );
            Ok(rows[0].clone())
        }
        DataOp::Delete => {
            let doc = doc_id()?;
            txn.delete_document_with_schema(collection, &doc, schema)
                .await?;
            Ok(json!({ "_docID": doc }))
        }
    }
}

async fn input(txn: &ConfigApplyTxn<'_>, write: &DataWrite) -> Result<String> {
    let collection = write.collection.as_str();
    ensure!(
        write.op == DataOp::Insert || !write.fields.is_empty(),
        "an update needs at least one field"
    );
    let version: ::schema::CollectionVersion = serde_json::from_value(
        txn.collection_version(collection)
            .await?
            .with_context(|| format!("collection {collection} is not installed"))?,
    )?;
    let mut parts = Vec::with_capacity(write.fields.len());
    for (name, value) in &write.fields {
        crate::store::graphql::validate_graphql_name(name)?;
        ensure!(
            !name.starts_with('_'),
            "internal field {name:?} cannot be written"
        );
        let field = version
            .fields
            .iter()
            .find(|f| &f.name == name)
            .with_context(|| format!("unknown field {name:?} in {collection}"))?;
        let literal = crate::tools::builtin::defra_write::field_literal(
            field.kind.graphql_type_name(),
            value,
        )
        .with_context(|| format!("invalid field `{name}`"))?;
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
    crate::store::graphql::validate_graphql_name(collection)?;
    let keys = grant
        .unique_keys
        .get(collection)
        .map(Vec::as_slice)
        .unwrap_or_default();
    let [key] = keys else {
        bail!(
            "on_conflict needs a collection with exactly one unique key; {collection} has {}",
            keys.len()
        );
    };
    let mut filter = Map::new();
    let mut label = Vec::with_capacity(key.len());
    for field in key {
        let value = write.fields.get(field).with_context(|| {
            format!("on_conflict needs the unique key field {field:?} in fields")
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
        crate::store::graphql::validate_graphql_name(field)?;
    }
    let response = txn
        .execute(&format!(
            "{{ {collection}(filter: {}) {{ {} }} }}",
            crate::tools::builtin::defra_query::render::render_filter(&Value::Object(filter))?,
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
            row.as_object().context("row is not an object")?,
        )?),
        None => None,
    };
    Ok(match insert_note(old.as_deref(), &new, on_conflict) {
        InsertOutcome::Inserted => None,
        InsertOutcome::Unchanged if old.as_deref() == Some(&new) => {
            Some(json!({"_docID": existing.map(|row| &row["_docID"])}))
        }
        InsertOutcome::Unchanged => Some(json!({"duplicate": true})),
        InsertOutcome::AbortStep => bail!("duplicate {collection} {}", label.join(",")),
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
