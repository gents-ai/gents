//! Documents a newly noticed graph route must still deliver.
//!
//! An engine that first notices a consumer treats the documents already in its
//! source collection as history, so a new consumer never fires on old data. For a
//! graph route that is wrong in one case: a run of the route's own revision that
//! started before the engine noticed the route. Its documents are live work, and
//! delivering them is what keeps every persisted, unprocessed document eventually
//! handled (the event delivery model's convergence property).

use std::collections::{BTreeSet, HashMap};

use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use gents_protocol::graphql::graphql_string_list_literal;
use serde_json::Value;

use super::runtime::graph_artifact_revision_digest;
use crate::graphql::escape_graphql_string;

/// Running runs of the revisions `consumer_ids` route for, by correlation, each
/// mapped to its revision digest. Empty when none of them is a graph route.
pub(crate) async fn live_run_correlations<'a>(
    node: &EmbeddedNode,
    owner: &str,
    consumer_ids: impl IntoIterator<Item = &'a str>,
) -> Result<HashMap<String, String>> {
    let digests = consumer_ids
        .into_iter()
        .filter_map(graph_artifact_revision_digest)
        .collect::<BTreeSet<_>>();
    if digests.is_empty() {
        return Ok(HashMap::new());
    }
    let query = format!(
        r#"{{ GraphRun(filter: {{ owner_did: {{ _eq: "{}" }}, status: {{ _eq: "running" }},
            revision_digest: {{ _in: {} }} }}) {{ correlation revision_digest }} }}"#,
        escape_graphql_string(owner),
        graphql_string_list_literal(&digests.into_iter().collect::<Vec<_>>()),
    );
    let response = crate::graphql::graphql_with_transaction_retry(node, &query, "graph.live_runs")
        .await
        .context("finding running graph runs")?;
    Ok(crate::graphql::rows::<Value>(&response, "GraphRun")?
        .into_iter()
        .filter_map(|run| {
            Some((
                run.get("correlation")?.as_str()?.to_owned(),
                run.get("revision_digest")?.as_str()?.to_owned(),
            ))
        })
        .collect())
}

/// Whether the document correlated as `correlation` is live work for `consumer_id`.
pub(crate) fn is_live_for(
    live: &HashMap<String, String>,
    consumer_id: &str,
    correlation: &str,
) -> bool {
    live.get(correlation).is_some_and(|digest| {
        graph_artifact_revision_digest(consumer_id).as_deref() == Some(digest.as_str())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_running_run_of_the_consumers_own_revision_is_live() {
        let digest = "a".repeat(64);
        let other = "b".repeat(64);
        let consumer = format!("graph-trigger-{digest}-{}", "0".repeat(16));
        let live = HashMap::from([
            ("run-1".to_owned(), format!("sha256:{digest}")),
            ("run-2".to_owned(), format!("sha256:{other}")),
        ]);
        assert!(is_live_for(&live, &consumer, "run-1"));
        assert!(
            !is_live_for(&live, &consumer, "run-2"),
            "another revision's run"
        );
        assert!(!is_live_for(&live, &consumer, "run-3"), "not a running run");
        assert!(
            !is_live_for(&live, "operator-trigger", "run-1"),
            "not a graph route"
        );
    }
}
