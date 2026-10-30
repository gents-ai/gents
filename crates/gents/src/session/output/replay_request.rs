use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use std::collections::{BTreeMap, BTreeSet};

use super::access::{rows_value, ReadAccess, ReadCache};
use super::headers::load_header;
use super::{replay_violation, CanonicalOutputReadError, CanonicalReplayScope};

async fn load_replay_request_facts_batch(
    node: &EmbeddedNode,
    request_doc_ids: &[String],
) -> Result<BTreeMap<String, ReplayRequestFacts>> {
    let distinct = request_doc_ids
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let mut facts = BTreeMap::new();
    for chunk in
        distinct.chunks(crate::inference::rendered_request::commits::FIELD_COMMIT_ALIAS_BATCH)
    {
        let ids = chunk
            .iter()
            .map(|request_doc_id| {
                format!(
                    r#""{}""#,
                    crate::graphql::escape_graphql_string(request_doc_id)
                )
            })
            .collect::<Vec<_>>()
            .join(", ");
        let mut query = format!(
            r#"{{ requests: AgentRequest(docIDs: [{ids}]) {{ _docID request_id purpose session_id node_did requester_did }}"#
        );
        for (index, request_doc_id) in chunk.iter().enumerate() {
            let escaped = crate::graphql::escape_graphql_string(request_doc_id);
            query.push_str(&format!(
                r#" commit{index}: _commits(docID: "{escaped}") {{ cid height fieldName signature {{ identity type }} }}"#
            ));
        }
        query.push_str(" }");
        let response = ReadAccess::Node(node)
            .query(&query, "load_canonical_replay_request_facts")
            .await?;
        let request_rows = rows_value(&response, "requests")?;
        let allowed = chunk.iter().cloned().collect::<BTreeSet<_>>();
        let mut grouped = chunk
            .iter()
            .map(|request_doc_id| (request_doc_id.clone(), Vec::new()))
            .collect::<BTreeMap<_, Vec<_>>>();
        for row in request_rows {
            let request_doc_id = row
                .get("_docID")
                .and_then(serde_json::Value::as_str)
                .with_context(|| "canonical replay request batch returned a row without _docID")?;
            anyhow::ensure!(
                allowed.contains(request_doc_id),
                "canonical replay request batch returned an unrequested physical request"
            );
            grouped
                .get_mut(request_doc_id)
                .expect("requested request IDs were initialized")
                .push(row.clone());
        }
        for (index, request_doc_id) in chunk.iter().enumerate() {
            let commit_rows = response
                .pointer(&format!("/data/commit{index}"))
                .and_then(serde_json::Value::as_array)
                .with_context(|| {
                    format!("canonical replay commit batch omitted {request_doc_id}")
                })?;
            let commits = crate::graphql::decode_composite_commits(commit_rows)?;
            let rows = grouped
                .remove(request_doc_id)
                .expect("requested request IDs were initialized");
            facts.insert(request_doc_id.clone(), ReplayRequestFacts { rows, commits });
        }
    }
    Ok(facts)
}

/// Physical-request reads for replay. Implementations read a bounded batch so
/// replay does not issue one request and commit query per historical turn. A
/// failed store read is an error, never empty rows, so it cannot pass for a
/// missing request.
#[async_trait::async_trait]
pub(crate) trait ReplayRequestReader: Sync {
    async fn request_facts(
        &self,
        request_doc_ids: &[String],
    ) -> Result<BTreeMap<String, ReplayRequestFacts>>;
}

#[derive(Clone)]
pub(crate) struct ReplayRequestFacts {
    pub(in crate::session) rows: Vec<serde_json::Value>,
    commits: Vec<crate::graphql::CompositeCommit>,
}

#[async_trait::async_trait]
impl ReplayRequestReader for EmbeddedNode {
    async fn request_facts(
        &self,
        request_doc_ids: &[String],
    ) -> Result<BTreeMap<String, ReplayRequestFacts>> {
        load_replay_request_facts_batch(self, request_doc_ids).await
    }
}

/// Validate a restored replay's physical request and bounded transcript view
/// even when no current assistant needs provider evidence.
pub(crate) async fn validate_canonical_replay_boundary(
    node: &EmbeddedNode,
    scope: CanonicalReplayScope<'_>,
    boundary: &crate::provider_context_reduction::SourceBoundary,
) -> Result<()> {
    validated_canonical_replay_boundary(node, node, scope, boundary)
        .await
        .map(|_| ())
}

pub(super) async fn validated_canonical_replay_boundary(
    node: &EmbeddedNode,
    requests: &impl ReplayRequestReader,
    scope: CanonicalReplayScope<'_>,
    boundary: &crate::provider_context_reduction::SourceBoundary,
) -> Result<(
    BTreeSet<String>,
    Option<crate::provider_context_reduction::TranscriptFactRef>,
)> {
    crate::provider_context_reduction::validate_source_boundary(
        boundary,
        scope.request_doc_id,
        scope.request_commit_cid,
    )
    .map_err(|error| replay_violation(error.to_string()))?;
    replay_ensure!(
        !scope.node_did.is_empty()
            && !scope.session_id.is_empty()
            && !scope.request_id.is_empty()
            && !scope.request_doc_id.is_empty()
            && !scope.request_commit_cid.is_empty()
            && scope.requester_did.is_none_or(|did| !did.is_empty()),
        "canonical replay scope is incomplete"
    );
    let request_commits = validate_replay_request(requests, scope).await?;
    let Some(high_water) = boundary.canonical_through.as_ref() else {
        return Ok((request_commits, None));
    };
    let mut cache = ReadCache::default();
    let high_water_header = load_header(
        ReadAccess::Node(node),
        &high_water.doc_id,
        scope.node_did,
        scope.requester_did,
        &mut cache,
    )
    .await
    .map_err(|error| {
        if error.downcast_ref::<CanonicalOutputReadError>().is_some() {
            replay_violation(error.to_string())
        } else {
            error
        }
    })?;
    replay_ensure!(
        high_water_header.message.session_id == scope.session_id
            && i64::from(high_water_header.message.sequence) == high_water.sequence,
        "canonical replay high-water header disagrees with its physical scope or sequence"
    );
    let high_water_commit = crate::graphql::newest_document_composite_commit(
        node,
        &high_water.doc_id,
        "canonical replay high-water header",
    )
    .await?
    .ok_or_else(|| {
        replay_violation("canonical replay high-water header has no composite commit")
    })?;
    replay_ensure!(
        high_water_commit.cid == high_water.commit_cid,
        "canonical replay high-water CID is not the physical header version"
    );
    Ok((request_commits, Some(high_water.clone())))
}

async fn validate_replay_request(
    requests: &impl ReplayRequestReader,
    scope: CanonicalReplayScope<'_>,
) -> Result<BTreeSet<String>> {
    let (request_id, commits) = load_replay_physical_request(
        requests,
        scope.request_doc_id,
        scope.node_did,
        scope.requester_did,
        scope.session_id,
    )
    .await?;
    replay_ensure!(
        request_id == scope.request_id,
        "canonical replay request has a different logical request ID"
    );
    replay_ensure!(
        commits.contains(scope.request_commit_cid),
        "canonical replay request boundary CID is not a commit of its physical request"
    );
    Ok(commits)
}

/// A missing, ambiguous or out-of-scope request is a `ReplayEvidenceViolation`;
/// a failed store read keeps its own error so callers cannot mistake it for one.
async fn load_replay_physical_request(
    requests: &impl ReplayRequestReader,
    request_doc_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
    session_id: &str,
) -> Result<(String, BTreeSet<String>)> {
    let request_doc_ids = [request_doc_id.to_owned()];
    let mut facts = requests.request_facts(&request_doc_ids).await?;
    let facts = facts
        .remove(request_doc_id)
        .with_context(|| format!("canonical replay request batch omitted {request_doc_id}"))?;
    validate_replay_request_facts(request_doc_id, node_did, requester_did, session_id, &facts)
}

pub(super) fn validate_replay_request_facts(
    request_doc_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
    session_id: &str,
    facts: &ReplayRequestFacts,
) -> Result<(String, BTreeSet<String>)> {
    let rows = &facts.rows;
    replay_ensure!(
        rows.len() == 1,
        "canonical replay request is missing or ambiguous"
    );
    let row = &rows[0];
    replay_ensure!(
        required_row_str(row, "_docID")? == request_doc_id
            && required_row_str(row, "purpose")? == "normal"
            && required_row_str(row, "session_id")? == session_id
            && required_row_str(row, "node_did")? == node_did
            && row.get("requester_did").is_some()
            && row["requester_did"].as_str() == requester_did,
        "canonical replay request crossed its physical node/session scope"
    );
    let commits = facts
        .commits
        .iter()
        .map(|commit| commit.cid.clone())
        .collect::<BTreeSet<_>>();
    Ok((required_row_str(row, "request_id")?.to_owned(), commits))
}

pub(super) fn required_row_str<'a>(row: &'a serde_json::Value, field: &str) -> Result<&'a str> {
    row.get(field)
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| replay_violation(format!("canonical replay row omitted {field}")))
}
