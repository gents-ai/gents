#[cfg(test)]
use super::plan::{
    document_identity, ensure_expectations_hold, DesiredStateApplyPlan, DesiredStateExpectation,
};
use super::projection::config_projection;
#[cfg(test)]
use super::projection::desired_state_document_digest;
use super::ConfigApplyTxn;
use crate::graphql::{escape_graphql_string, graphql_string_list_literal};
use crate::Collection;
use anyhow::{Context, Result};
use gents_protocol::graphql::graphql_rows_from_response;
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

const READ_RECORDS_PAGE_SIZE: usize = 256;

pub(super) type DesiredStateRecordKey = (Collection, String, String);
pub(super) type DesiredStateRecord = (String, Value);

#[cfg(test)]
tokio::task_local! {
    static DESIRED_STATE_READ_PAGE_QUERIES: std::sync::Arc<std::sync::atomic::AtomicUsize>;
}

#[cfg(test)]
fn record_desired_state_read_page_query() {
    let _ = DESIRED_STATE_READ_PAGE_QUERIES
        .try_with(|queries| queries.fetch_add(1, std::sync::atomic::Ordering::Relaxed));
}

#[cfg(test)]
pub(crate) async fn count_desired_state_read_page_queries<F>(future: F) -> (F::Output, usize)
where
    F: std::future::Future,
{
    let queries = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let output = DESIRED_STATE_READ_PAGE_QUERIES
        .scope(queries.clone(), future)
        .await;
    (output, queries.load(std::sync::atomic::Ordering::Relaxed))
}

pub async fn read_record(
    txn: &ConfigApplyTxn<'_>,
    collection: Collection,
    owner: &str,
    id: &str,
) -> Result<Option<(String, Value)>> {
    Ok(read_records(txn, collection, owner, &[id])
        .await?
        .remove(id))
}

pub async fn read_records(
    txn: &ConfigApplyTxn<'_>,
    collection: Collection,
    owner: &str,
    ids: &[&str],
) -> Result<BTreeMap<String, (String, Value)>> {
    read_records_paged(txn, collection, owner, ids, READ_RECORDS_PAGE_SIZE).await
}

#[cfg(test)]
pub(crate) async fn read_records_with_page_size(
    txn: &ConfigApplyTxn<'_>,
    collection: Collection,
    owner: &str,
    ids: &[&str],
    page_size: usize,
) -> Result<BTreeMap<String, (String, Value)>> {
    read_records_paged(txn, collection, owner, ids, page_size).await
}

async fn read_records_paged(
    txn: &ConfigApplyTxn<'_>,
    collection: Collection,
    owner: &str,
    ids: &[&str],
    page_size: usize,
) -> Result<BTreeMap<String, (String, Value)>> {
    let mut unique_ids = BTreeSet::new();
    for id in ids {
        anyhow::ensure!(
            !owner.trim().is_empty() && !id.trim().is_empty(),
            "configuration reference requires owner and ID"
        );
        if collection == Collection::Node {
            anyhow::ensure!(*id == owner, "node reference must match owner");
        }
        unique_ids.insert(*id);
    }
    let mut out = BTreeMap::new();
    if unique_ids.is_empty() {
        return Ok(out);
    }
    let (fields, _) = config_projection(collection, None)?;
    let name = collection.graphql_type();
    let unique_field = collection.unique_field();
    let ordered: Vec<&str> = unique_ids.into_iter().collect();
    for page in ordered.chunks(page_size.max(1)) {
        let filter = if collection == Collection::Node {
            format!(
                r#"{{ node_did: {{ _eq: "{}" }} }}"#,
                escape_graphql_string(owner)
            )
        } else {
            format!(
                r#"{{ node_did: {{ _eq: "{}" }}, {unique_field}: {{ _in: {} }} }}"#,
                escape_graphql_string(owner),
                graphql_string_list_literal(page.iter().copied())
            )
        };
        let response = txn
            .execute(&format!(
                "{{ {name}(filter: {filter}) {{ _docID {} }} }}",
                fields.join(" ")
            ))
            .await?;
        #[cfg(test)]
        record_desired_state_read_page_query();
        let mut by_id: BTreeMap<String, Vec<Value>> = BTreeMap::new();
        for row in graphql_rows_from_response(&response, name) {
            let id = row
                .get(unique_field)
                .and_then(Value::as_str)
                .context("configuration row missing logical ID")?
                .to_owned();
            by_id.entry(id).or_default().push(row);
        }
        for (id, mut rows) in by_id {
            anyhow::ensure!(
                rows.len() <= 1,
                "multiple live {name} documents share owner {owner:?} and ID {id:?}"
            );
            let mut row = rows.pop().context("grouped configuration row is empty")?;
            let doc_id = row
                .as_object_mut()
                .context("configuration row must be an object")?
                .remove("_docID")
                .and_then(|value| value.as_str().map(str::to_owned))
                .context("configuration row missing physical ID")?;
            let (_, value) = config_projection(collection, Some(&row))?;
            out.insert(id, (doc_id, value.context("canonical projection missing")?));
        }
    }
    Ok(out)
}

pub(super) async fn read_identity_records(
    txn: &ConfigApplyTxn<'_>,
    identities: &[(Collection, String, String)],
) -> Result<BTreeMap<DesiredStateRecordKey, DesiredStateRecord>> {
    let mut requested: BTreeMap<(Collection, String), BTreeSet<String>> = BTreeMap::new();
    for (collection, owner, id) in identities {
        requested
            .entry((*collection, owner.clone()))
            .or_default()
            .insert(id.clone());
    }

    let mut records = BTreeMap::new();
    for ((collection, owner), ids) in requested {
        let ids = ids.into_iter().collect::<Vec<_>>();
        let id_refs = ids.iter().map(String::as_str).collect::<Vec<_>>();
        for (id, record) in read_records(txn, collection, &owner, &id_refs).await? {
            records.insert((collection, owner.clone(), id), record);
        }
    }
    Ok(records)
}

pub(crate) async fn read_desired_state_document_in_txn(
    txn: &ConfigApplyTxn<'_>,
    collection: Collection,
    node_did: &str,
    unique_value: &str,
) -> Result<Option<Value>> {
    Ok(read_record(txn, collection, node_did, unique_value)
        .await?
        .map(|(_, value)| value))
}

#[cfg(test)]
pub(crate) async fn expect_existing_documents_unchanged(
    txn: &ConfigApplyTxn<'_>,
    plan: &DesiredStateApplyPlan,
) -> Result<()> {
    let identities = plan
        .documents()
        .iter()
        .map(|document| {
            let (owner, id) = document_identity(document.collection, &document.add)?;
            Ok((document.collection, owner.to_owned(), id.to_owned()))
        })
        .collect::<Result<Vec<_>>>()?;
    let records = read_identity_records(txn, &identities).await?;
    let mut expected = Vec::new();
    for document in plan.documents() {
        let (owner, id) = document_identity(document.collection, &document.add)?;
        if records.contains_key(&(document.collection, owner.to_owned(), id.to_owned())) {
            expected.push(DesiredStateExpectation {
                collection: document.collection,
                owner: owner.to_owned(),
                id: id.to_owned(),
                digest: Some(desired_state_document_digest(&document.add)?),
            });
        }
    }
    let guarded = DesiredStateApplyPlan::new(Vec::new())?.with_expected(expected)?;
    ensure_expectations_hold(txn, &guarded).await
}
