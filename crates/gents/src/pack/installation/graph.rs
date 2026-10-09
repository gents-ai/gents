//! Graph packs' installation record, and removal of what it lists.
//!
//! A graph pack's [`super::Record`] lists every document its install and
//! activation created: the `GraphDefinition`, every `GraphRevision` this
//! package has ever produced for that graph (including retired ones, so a
//! reinstall after an untracked install picks up every one of them), each
//! revision's derived `EventSource`/`Trigger`/`Callback`/`CallbackBinding`,
//! and the package's own authored documents (agents, tasks, and so on).
//!
//! The runtime-derived documents are never drift-checked: a `GraphDefinition`'s
//! active pointer and generation change on activation, and a `GraphRevision`
//! and its derived artifacts are wholly owned by the graph pipeline. They are
//! recorded with an empty digest and, when created by this install, removed
//! unconditionally, never through the ordinary per-document digest comparison
//! [`super::remove_record_in_txn`] runs for a pack's own authored documents.
//! A `GraphDefinition` or `GraphRevision` (and its derived artifacts) that
//! already existed before this install wrote anything is recorded as not
//! created, the same as an adopted authored document, so removal leaves an
//! operator's untracked graph alone.

use std::collections::BTreeSet;

use anyhow::{Context, Result};

use crate::config_client::{ConfigApplyTxn, DesiredStateApplyDocument};
use crate::graph_pipeline::{graph_artifact_is_reserved, revision_artifact_ids, GraphPlan};
use crate::graphql::{escape_graphql_string, graphql_string_list_literal};

use super::{key, PackIdentity, Record, RecordedDocument, Retained};

/// A run query is bounded, never scanning a whole graph's history: refusal
/// names a handful of offending runs and says there may be more.
const UNFINISHED_RUN_REPORT_LIMIT: usize = 8;

/// A retained-run report is bounded the same way: it names a handful of runs
/// pinned to a removed revision and reports the exact remaining total via a
/// bounded `COUNT` rather than scanning the rest.
const RETAINED_RUN_REPORT_LIMIT: usize = 20;

/// Revisions are deleted in pages of this size, one mutation per page,
/// instead of one mutation per row.
const REVISION_DELETE_PAGE_SIZE: usize = 256;

/// Whether `document` is owned by the graph runtime rather than authored by
/// a pack.
pub(super) fn is_graph_owned(document: &RecordedDocument) -> bool {
    document.collection == "GraphRevision"
        || document.collection == "GraphDefinition"
        || graph_artifact_is_reserved(&document.id)
}

/// What was true before a graph package's install/activation ran, observed
/// inside the same transaction so [`record_graph_install_in_txn`] can tell a
/// document the install created from one that already existed.
pub(crate) struct ObservedGraphInstall {
    prior: Record,
    preexisting: BTreeSet<String>,
    /// Whether `graph_id`'s `GraphDefinition` already existed before this
    /// install wrote anything: an untracked (legacy, or import-carried)
    /// graph that must be adopted rather than claimed as created.
    graph_definition_preexisted: bool,
    /// Digests of `graph_id`'s `GraphRevision`s that already existed before
    /// this install wrote anything, for the same reason.
    preexisting_revision_digests: BTreeSet<String>,
}

/// Observes the prior installation record and which of `desired`'s package
/// documents, `graph_id`'s `GraphDefinition`, and `graph_id`'s existing
/// revisions already exist. Called from inside the graph publish
/// transaction, after schemas are ensured and before the desired-state plan
/// and revision are applied, so [`record_graph_install_in_txn`] can tell a
/// document this install created from one it only adopted.
pub(crate) async fn observe_graph_install_in_txn(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    coordinate: &str,
    desired: &[DesiredStateApplyDocument],
    graph_id: &str,
) -> Result<ObservedGraphInstall> {
    let prior = super::read_record(txn, owner, coordinate).await?;
    let desired_ids: Vec<(crate::Collection, &str)> = desired
        .iter()
        .map(|document| {
            let id = document.add[document.collection.unique_field()]
                .as_str()
                .context("package document is missing its logical ID")?;
            Ok((document.collection, id))
        })
        .collect::<Result<_>>()?;
    let desired_live = super::live_digests(txn, owner, desired_ids).await?;
    let mut preexisting = BTreeSet::new();
    for document in desired {
        let id = document.add[document.collection.unique_field()]
            .as_str()
            .context("package document is missing its logical ID")?;
        if desired_live.contains_key(&(document.collection, id.to_owned())) {
            preexisting.insert(key(document.collection.graphql_type(), id));
        }
    }
    let graph_definition_preexisted =
        super::live_digest(txn, crate::Collection::GraphDefinition, owner, graph_id)
            .await?
            .is_some();
    let response = txn
        .execute(&format!(
            r#"{{ GraphRevision(filter: {{ owner_did: {{ _eq: "{}" }}, graph_id: {{ _eq: "{}" }} }}) {{ digest }} }}"#,
            escape_graphql_string(owner),
            escape_graphql_string(graph_id),
        ))
        .await?;
    let preexisting_revision_digests = response["data"]["GraphRevision"]
        .as_array()
        .context("reading the graph's existing revisions")?
        .iter()
        .map(|row| {
            row["digest"]
                .as_str()
                .context("graph revision is missing digest")
                .map(str::to_owned)
        })
        .collect::<Result<BTreeSet<String>>>()?;
    Ok(ObservedGraphInstall {
        prior,
        preexisting,
        graph_definition_preexisted,
        preexisting_revision_digests,
    })
}

/// Records a graph package install/activation after it succeeded, in the
/// shared `PackInstallation` record, through the one shared writer
/// [`super::write_record_in_txn`] so the graph and documents installers
/// cannot diverge on how a record is written.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn record_graph_install_in_txn(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    pack: &PackIdentity,
    explicit: bool,
    observed: ObservedGraphInstall,
    desired: &[DesiredStateApplyDocument],
    graph_id: &str,
    package_name: &str,
) -> Result<()> {
    let ObservedGraphInstall {
        prior,
        preexisting,
        graph_definition_preexisted,
        preexisting_revision_digests,
    } = observed;
    let mut documents = prior.documents.clone();

    // Every revision this package has ever produced for this graph,
    // including a retired one: a reinstall after an untracked (legacy)
    // install picks up all of them at once (see module docs).
    let response = txn
        .execute(&format!(
            r#"{{ GraphRevision(filter: {{ owner_did: {{ _eq: "{}" }}, graph_id: {{ _eq: "{}" }} }}) {{ digest plan_json }} }}"#,
            escape_graphql_string(owner),
            escape_graphql_string(graph_id),
        ))
        .await?;
    let rows = response["data"]["GraphRevision"]
        .as_array()
        .context("reading the graph's revisions")?
        .clone();
    for row in &rows {
        let plan_json = row["plan_json"]
            .as_str()
            .context("graph revision is missing plan_json")?;
        let plan: GraphPlan =
            serde_json::from_str(plan_json).context("graph revision plan is not valid JSON")?;
        let Some(package) = plan.package.as_ref() else {
            continue;
        };
        if package.name != package_name {
            continue;
        }
        let digest = row["digest"]
            .as_str()
            .context("graph revision is missing digest")?
            .to_owned();
        let revision_name = key("GraphRevision", &digest);
        // A revision already recorded keeps its recorded `created`; one seen
        // live before this install wrote anything is adopted, not claimed.
        let revision_created = documents
            .get(&revision_name)
            .map(|document| document.created)
            .unwrap_or(!preexisting_revision_digests.contains(&digest));
        documents.insert(
            revision_name,
            RecordedDocument {
                collection: "GraphRevision".to_owned(),
                id: digest,
                digest: String::new(),
                created: revision_created,
            },
        );
        for (collection, id) in revision_artifact_ids(&plan)? {
            let artifact_name = key(collection.graphql_type(), &id);
            // A derived artifact is created exactly when its revision is,
            // unless it was already recorded with a different verdict.
            let artifact_created = documents
                .get(&artifact_name)
                .map(|document| document.created)
                .unwrap_or(revision_created);
            documents.insert(
                artifact_name,
                RecordedDocument {
                    collection: collection.graphql_type().to_owned(),
                    id,
                    digest: String::new(),
                    created: artifact_created,
                },
            );
        }
    }

    let graph_definition_name = key("GraphDefinition", graph_id);
    let graph_definition_created = documents
        .get(&graph_definition_name)
        .map(|document| document.created)
        .unwrap_or(!graph_definition_preexisted);
    documents.insert(
        graph_definition_name,
        RecordedDocument {
            collection: "GraphDefinition".to_owned(),
            id: graph_id.to_owned(),
            digest: String::new(),
            created: graph_definition_created,
        },
    );

    let desired_ids: Vec<(crate::Collection, &str)> = desired
        .iter()
        .map(|document| {
            let id = document.add[document.collection.unique_field()]
                .as_str()
                .context("package document is missing its logical ID")?;
            Ok((document.collection, id))
        })
        .collect::<Result<_>>()?;
    let desired_live = super::live_digests(txn, owner, desired_ids).await?;

    for document in desired {
        let id = document.add[document.collection.unique_field()]
            .as_str()
            .context("package document is missing its logical ID")?;
        let name = key(document.collection.graphql_type(), id);
        let created = documents
            .get(&name)
            .map(|document| document.created)
            .unwrap_or(!preexisting.contains(&name));
        let digest = desired_live
            .get(&(document.collection, id.to_owned()))
            .cloned()
            .or_else(|| documents.get(&name).map(|document| document.digest.clone()))
            .unwrap_or_default();
        documents.insert(
            name,
            RecordedDocument {
                collection: document.collection.graphql_type().to_owned(),
                id: id.to_owned(),
                digest,
                created,
            },
        );
    }

    super::write_record_in_txn(
        txn,
        owner,
        pack,
        &prior,
        documents.into_values().collect(),
        explicit,
        false,
    )
    .await
}

/// Refuses removal while any of `graph_ids`' runs is not terminal, naming
/// the graph and the run so the operator knows what to cancel.
pub(super) async fn ensure_no_unfinished_runs_in_txn(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    graph_ids: &BTreeSet<String>,
) -> Result<()> {
    let ids = graphql_string_list_literal(graph_ids.iter().map(String::as_str));
    let statuses = graphql_string_list_literal(
        crate::graph_pipeline::GRAPH_RUN_TERMINAL_STATUSES
            .iter()
            .copied(),
    );
    let response = txn
        .execute(&format!(
            r#"{{ GraphRun(filter: {{ owner_did: {{ _eq: "{}" }}, graph_id: {{ _in: {ids} }}, status: {{ _nin: {statuses} }} }}, limit: {}) {{ run_id graph_id }} }}"#,
            escape_graphql_string(owner),
            UNFINISHED_RUN_REPORT_LIMIT + 1,
        ))
        .await?;
    let rows = response["data"]["GraphRun"]
        .as_array()
        .context("reading graph runs")?;
    if rows.is_empty() {
        return Ok(());
    }
    let mut names: Vec<String> = rows
        .iter()
        .take(UNFINISHED_RUN_REPORT_LIMIT)
        .map(|row| {
            let graph_id = row["graph_id"].as_str().unwrap_or("?");
            let run_id = row["run_id"].as_str().unwrap_or("?");
            format!(
                "graph {graph_id} has an unfinished run {run_id}; cancel it with \
                 gents graph cancel {run_id}"
            )
        })
        .collect();
    if rows.len() > UNFINISHED_RUN_REPORT_LIMIT {
        names.push("(and possibly more)".to_owned());
    }
    anyhow::bail!("{}; retry after cancelling", names.join("; "))
}

/// Deletes every `GraphRevision` named by `digests`, returning what was
/// removed and, for each SDL schema its package required, a
/// [`Retained`] entry: DefraDB cannot drop a collection schema, so package
/// schemas outlive their revision.
pub(super) async fn delete_revisions_in_txn(
    txn: &ConfigApplyTxn<'_>,
    owner: &str,
    digests: &[String],
) -> Result<(Vec<String>, Vec<Retained>)> {
    if digests.is_empty() {
        return Ok((Vec::new(), Vec::new()));
    }
    let list = graphql_string_list_literal(digests.iter().map(String::as_str));
    let response = txn
        .execute(&format!(
            r#"{{ GraphRevision(filter: {{ owner_did: {{ _eq: "{}" }}, digest: {{ _in: {list} }} }}) {{ _docID digest plan_json }} }}"#,
            escape_graphql_string(owner)
        ))
        .await?;
    let rows = response["data"]["GraphRevision"]
        .as_array()
        .context("reading graph revisions to remove")?
        .clone();
    let mut removed = Vec::new();
    let mut retained_schemas = BTreeSet::new();
    let mut doc_ids = Vec::new();
    for row in &rows {
        let digest = row["digest"].as_str().context("revision missing digest")?;
        let doc_id = row["_docID"].as_str().context("revision missing _docID")?;
        let plan_json = row["plan_json"]
            .as_str()
            .context("revision missing plan_json")?;
        let plan: GraphPlan =
            serde_json::from_str(plan_json).context("revision plan is not valid JSON")?;
        if let Some(package) = plan.package.as_ref() {
            for schema in &package.required_schema_digests {
                retained_schemas.extend(schema.collection_contract_digests.keys().cloned());
            }
        }
        doc_ids.push(doc_id.to_owned());
        removed.push(key("GraphRevision", digest));
    }
    for page in doc_ids.chunks(REVISION_DELETE_PAGE_SIZE) {
        let page_ids = graphql_string_list_literal(page.iter().map(String::as_str));
        txn.execute(&format!(
            r#"mutation {{ delete_GraphRevision(filter: {{ owner_did: {{ _eq: "{}" }}, _docID: {{ _in: {page_ids} }} }}) {{ _docID }} }}"#,
            escape_graphql_string(owner)
        ))
        .await?;
    }
    let mut retained: Vec<Retained> = retained_schemas
        .into_iter()
        .map(|name| Retained {
            item: format!("schema {name}"),
            reason: "collection schemas cannot be dropped".to_owned(),
        })
        .collect();

    // GraphRun history stays (it is never part of this record), but its
    // pinned revision is gone: `graph result`/`graph watch` need the graph
    // reinstalled before that run's view can be read again. The report is
    // bounded to a handful of runs plus, past the limit, one item naming the
    // exact remaining total from a `COUNT` aggregate over the same filter,
    // never a full scan of a graph's run history.
    let runs = txn
        .execute(&format!(
            r#"{{ GraphRun(filter: {{ owner_did: {{ _eq: "{}" }}, revision_digest: {{ _in: {list} }} }}, limit: {}) {{ run_id }} }}"#,
            escape_graphql_string(owner),
            RETAINED_RUN_REPORT_LIMIT + 1,
        ))
        .await?;
    let run_rows = runs["data"]["GraphRun"]
        .as_array()
        .context("reading graph runs pinned to a removed revision")?;
    for run in run_rows.iter().take(RETAINED_RUN_REPORT_LIMIT) {
        let run_id = run["run_id"].as_str().context("run missing run_id")?;
        retained.push(Retained {
            item: format!("run {run_id}"),
            reason: "result view needs the graph reinstalled".to_owned(),
        });
    }
    if run_rows.len() > RETAINED_RUN_REPORT_LIMIT {
        let count_response = txn
            .execute(&format!(
                r#"{{ COUNT(GraphRun: {{ filter: {{ owner_did: {{ _eq: "{}" }}, revision_digest: {{ _in: {list} }} }} }}) }}"#,
                escape_graphql_string(owner),
            ))
            .await?;
        let more = match count_response["data"]["COUNT"].as_i64() {
            Some(total) => {
                format!(
                    "{} more run(s)",
                    (total - RETAINED_RUN_REPORT_LIMIT as i64).max(0)
                )
            }
            None => format!("more than {RETAINED_RUN_REPORT_LIMIT} run(s)"),
        };
        retained.push(Retained {
            item: format!("{more} pinned to a removed revision"),
            reason: "result view needs the graph reinstalled; list truncated".to_owned(),
        });
    }

    Ok((removed, retained))
}

#[cfg(test)]
mod tests;
