use super::plan::DesiredStateApplyPlan;
use super::ConfigApplyTxn;
use crate::defra_query::SchemaField;
use crate::Collection;
use anyhow::{Context, Result};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub(super) type IntrospectedFields = BTreeMap<String, Option<BTreeMap<String, SchemaField>>>;

const INTROSPECTION_PAGE: usize = 64;

fn event_source_schema_fields(
    source: &crate::document_config::EventSource,
) -> (Option<&str>, Option<&str>) {
    let correlation = source
        .correlation_field
        .as_deref()
        .map(str::trim)
        .filter(|field| !field.is_empty());
    let count_field = source
        .group
        .as_ref()
        .and_then(|group| group.expected_count.as_ref())
        .and_then(|count| match count {
            crate::document_config::EventGroupCount::SourceField { source_field } => {
                Some(source_field.as_str())
            }
            crate::document_config::EventGroupCount::Fixed(_) => None,
        })
        .map(str::trim)
        .filter(|field| !field.is_empty());
    (correlation, count_field)
}

fn obligation_count_fields(candidate: &Value) -> Result<Vec<(String, String, String)>> {
    let entries = crate::document_config::deserialize_optional_surface_tools(
        candidate.get("entries").cloned().unwrap_or(Value::Null),
    )?;
    Ok(entries
        .unwrap_or_default()
        .into_iter()
        .filter_map(|entry| {
            let crate::document_config::SurfaceToolDecl::Create(decl) = entry else {
                return None;
            };
            let field = decl
                .output_obligation
                .as_ref()
                .and_then(|obligation| obligation.expected_count_field.clone())?;
            Some((decl.tool_name.clone(), decl.collection.clone(), field))
        })
        .collect())
}

fn schema_targets(collection: Collection, candidate: &Value) -> Vec<String> {
    match collection {
        Collection::EventSource => {
            serde_json::from_value::<crate::document_config::EventSource>(candidate.clone())
                .ok()
                .filter(|source| event_source_schema_fields(source) != (None, None))
                .map(|source| source.source_collection)
                .into_iter()
                .collect()
        }
        Collection::DatastoreToolSurface => obligation_count_fields(candidate)
            .map(|fields| {
                fields
                    .into_iter()
                    .map(|(_, collection, _)| collection)
                    .collect()
            })
            .unwrap_or_default(),
        _ => Vec::new(),
    }
}

pub(super) async fn prefetch_declared_fields(
    txn: &ConfigApplyTxn<'_>,
    plan: &DesiredStateApplyPlan,
    introspected: &mut IntrospectedFields,
) -> Result<()> {
    let wanted: Vec<String> = plan
        .documents()
        .iter()
        .flat_map(|document| {
            [&document.add, &document.update]
                .into_iter()
                .flat_map(|payload| schema_targets(document.collection, payload))
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .filter(|collection| {
            !introspected.contains_key(collection)
                && crate::defra_query::schema::introspection_query(collection).is_ok()
        })
        .collect();
    for page in wanted.chunks(INTROSPECTION_PAGE) {
        let names: Vec<&str> = page.iter().map(String::as_str).collect();
        let query = crate::defra_query::schema::introspection_query_many(&names)?;
        let response = txn.execute(&query).await?;
        let schemas =
            crate::defra_query::schema::parse_collection_schemas(response.get("data"), names.len());
        for (name, schema) in names.into_iter().zip(schemas) {
            introspected.insert(
                name.to_owned(),
                schema.map(|schema| {
                    schema
                        .fields
                        .into_iter()
                        .map(|field| (field.name.clone(), field))
                        .collect()
                }),
            );
        }
    }
    Ok(())
}

pub(super) async fn declared_fields<'a>(
    txn: &ConfigApplyTxn<'_>,
    collection: &str,
    introspected: &'a mut IntrospectedFields,
) -> Result<Option<&'a BTreeMap<String, SchemaField>>> {
    if !introspected.contains_key(collection) {
        if let Ok(query) = crate::defra_query::schema::introspection_query(collection) {
            let response = txn.execute(&query).await?;
            let fields = crate::defra_query::schema::parse_collection_schema(response.get("data"))
                .map(|schema| {
                    schema
                        .fields
                        .into_iter()
                        .map(|field| (field.name.clone(), field))
                        .collect::<BTreeMap<_, _>>()
                });
            introspected.insert(collection.to_owned(), fields);
        }
    }
    Ok(introspected.get(collection).and_then(Option::as_ref))
}

pub(crate) async fn collection_is_installed(
    txn: &ConfigApplyTxn<'_>,
    collection: &str,
) -> Result<bool> {
    Ok(
        declared_fields(txn, collection, &mut IntrospectedFields::new())
            .await?
            .is_some(),
    )
}

pub(super) async fn validate_output_obligation_count_fields(
    txn: &ConfigApplyTxn<'_>,
    candidate: &Value,
    introspected: &mut IntrospectedFields,
) -> Result<()> {
    let surface_id = candidate
        .get("surface_id")
        .and_then(Value::as_str)
        .unwrap_or_default();
    for (tool_name, collection, field) in obligation_count_fields(candidate)? {
        let field = field.as_str();
        let Some(fields) = declared_fields(txn, &collection, introspected).await? else {
            continue;
        };
        match fields.get(field) {
            Some(declared)
                if crate::defra_write::can_hold_canonical_count(declared.named_type()) => {}
            Some(declared) => anyhow::bail!(
                "DatastoreToolSurface {surface_id} tool {:?} output_obligation.expected_count_field {field:?} names a {} field of {}, which cannot carry the count; the runtime parses an integer or its canonical decimal spelling out of the call argument",
                tool_name,
                declared.type_name,
                collection,
            ),
            None => anyhow::bail!(
                "DatastoreToolSurface {surface_id} tool {:?} output_obligation.expected_count_field {field:?} does not exist on {}",
                tool_name,
                collection,
            ),
        }
    }
    Ok(())
}

pub(super) async fn validate_event_source_live_fields(
    txn: &ConfigApplyTxn<'_>,
    candidate: &Value,
    introspected: &mut IntrospectedFields,
) -> Result<()> {
    let source: crate::document_config::EventSource = serde_json::from_value(candidate.clone())
        .context("decoding EventSource for live-field validation")?;
    let (correlation, count_field) = event_source_schema_fields(&source);
    if correlation.is_none() && count_field.is_none() {
        return Ok(());
    }
    let Some(fields) = declared_fields(txn, &source.source_collection, introspected).await? else {
        return Ok(());
    };
    if let Some(field) = correlation {
        match fields.get(field) {
            Some(declared) if declared.named_type() == "String" => {}
            Some(declared) => anyhow::bail!(
                "EventSource {} correlation_field {:?} must be String, found {}",
                source.event_source_id,
                field,
                declared.type_name
            ),
            None => anyhow::bail!(
                "EventSource {} correlation_field {:?} does not exist on {}",
                source.event_source_id,
                field,
                source.source_collection
            ),
        }
    }
    if let Some(field) = count_field {
        match fields.get(field) {
            Some(declared)
                if crate::defra_write::can_hold_canonical_count(declared.named_type()) => {}
            Some(declared) => anyhow::bail!(
                "EventSource {} expected_count_field {:?} names a {} field of {}, which cannot \
                 carry the count; the runtime parses an integer or its canonical decimal spelling \
                 out of the source document",
                source.event_source_id,
                field,
                declared.type_name,
                source.source_collection
            ),
            None => anyhow::bail!(
                "EventSource {} expected_count_field {:?} does not exist on {}",
                source.event_source_id,
                field,
                source.source_collection
            ),
        }
    }
    Ok(())
}
