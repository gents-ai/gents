use rapidhash::{HashMapExt, RapidHashMap};
use std::collections::BTreeSet;

use defra_node::EmbeddedNode;
use gents_protocol::graphql::graphql_string_list_literal;
use serde_json::Value;

use super::runtime::{graph_artifact_revision_digest, GraphPipelineError};
use crate::graphql::escape_graphql_string;

pub(crate) async fn live_run_correlations<'a>(
    node: &EmbeddedNode,
    owner: &str,
    consumer_ids: impl IntoIterator<Item = &'a str>,
) -> Result<RapidHashMap<String, String>, GraphPipelineError> {
    let digests = consumer_ids
        .into_iter()
        .filter_map(graph_artifact_revision_digest)
        .collect::<BTreeSet<_>>();
    if digests.is_empty() {
        return Ok(RapidHashMap::new());
    }
    let query = format!(
        r#"{{ GraphRun(filter: {{ owner_did: {{ _eq: "{}" }}, status: {{ _eq: "running" }},
            revision_digest: {{ _in: {} }} }}) {{ correlation revision_digest }} }}"#,
        escape_graphql_string(owner),
        graphql_string_list_literal(&digests.into_iter().collect::<Vec<_>>()),
    );
    let response = crate::graphql::graphql_with_transaction_retry(node, &query, "graph.live_runs")
        .await
        .map_err(|source| GraphPipelineError::LiveRunQuery { source })?;
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

pub(crate) fn is_live_for(
    live: &RapidHashMap<String, String>,
    consumer_id: &str,
    correlation: &str,
) -> bool {
    live.get(correlation).is_some_and(|digest| {
        graph_artifact_revision_digest(consumer_id).as_deref() == Some(digest.as_str())
    })
}
