use std::collections::BTreeSet;

use anyhow::{Context, Result};
use serde::Deserialize;

use super::*;

#[derive(Debug, Deserialize)]
struct GoalDocumentRef {
    #[serde(rename = "_docID")]
    doc_id: String,
}

#[derive(Debug, Deserialize)]
struct GoalCompositeCommit {
    #[serde(default)]
    cid: String,
    #[serde(default)]
    height: i64,
    #[serde(default, rename = "fieldName")]
    field_name: String,
    #[serde(default)]
    heads: Vec<GoalCommitHead>,
}

#[derive(Debug, Deserialize)]
struct GoalCommitHead {
    #[serde(default)]
    cid: String,
    #[serde(default, rename = "fieldName")]
    field_name: String,
}

#[derive(Debug, Deserialize)]
struct HistoricalGoalDocument {
    #[serde(rename = "_docID")]
    doc_id: String,
    #[serde(default)]
    goal_id: String,
    #[serde(default)]
    session_id: String,
    #[serde(default)]
    agent_did: String,
    #[serde(default)]
    status: String,
    #[serde(default)]
    consecutive_blocked_audits: Option<i64>,
    #[serde(default)]
    wrapup_requested: Option<bool>,
    #[serde(default)]
    wrapup_completed: Option<bool>,
    #[serde(default)]
    token_budget: Option<i64>,
    #[serde(default)]
    tokens_used: Option<i64>,
    #[serde(default)]
    last_blocked_request_id: Option<String>,
    #[serde(default)]
    last_blocked_reason: Option<String>,
    #[serde(default)]
    last_failure: Option<String>,
    #[serde(default)]
    completion_evidence: Option<String>,
    #[serde(default)]
    created_at: Option<String>,
    #[serde(default)]
    updated_at: Option<String>,
}

pub(super) async fn load_timeline_goal_versions_for_session(
    access: &ConfigAccess,
    session_id: &str,
) -> Result<Vec<TimelineGoalVersionRow>> {
    let escaped_session_id = escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            Goal(
                filter: {{ session_id: {{ _eq: "{escaped_session_id}" }} }},
                showDeleted: true
            ) {{ _docID }}
        }}"#
    );
    let docs = load_rows::<GoalDocumentRef>(access, "Goal", &query).await?;
    let mut versions = Vec::new();
    for doc in docs {
        versions.extend(load_goal_document_versions(access, session_id, &doc.doc_id).await?);
    }
    Ok(versions)
}

async fn load_goal_document_versions(
    access: &ConfigAccess,
    expected_session_id: &str,
    doc_id: &str,
) -> Result<Vec<TimelineGoalVersionRow>> {
    // Do not send a fieldName filter. DefraDB evaluates that filter in memory
    // and currently degrades malformed filters to no filter. Selecting the
    // composite commits in typed Rust keeps history integrity fail-closed.
    let escaped_doc_id = escape_graphql_string(doc_id);
    let response = access
        .execute(&format!(
            r#"query {{
                _commits(docID: "{escaped_doc_id}") {{
                    cid
                    height
                    fieldName
                    heads {{ cid height fieldName }}
                }}
            }}"#
        ))
        .await
        .with_context(|| format!("reading native Goal history for {doc_id}"))?;
    let commit_values = response
        .pointer("/data/_commits")
        .and_then(serde_json::Value::as_array)
        .context("Goal _commits response carried no data._commits array")?;
    let mut commits = commit_values
        .iter()
        .cloned()
        .map(serde_json::from_value::<GoalCompositeCommit>)
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("decoding Goal composite commits")?
        .into_iter()
        .filter(|commit| commit.field_name == "_C" && !commit.cid.trim().is_empty())
        .collect::<Vec<_>>();
    commits.sort_by(|left, right| (left.height, &left.cid).cmp(&(right.height, &right.cid)));
    commits.dedup_by(|left, right| left.cid == right.cid);
    if commits.is_empty() {
        anyhow::bail!("Goal {doc_id} is visible but its native composite history is unavailable");
    }

    let known_cids = commits
        .iter()
        .map(|commit| commit.cid.as_str())
        .collect::<BTreeSet<_>>();
    for commit in &commits {
        for parent in commit.heads.iter().filter(|head| head.field_name == "_C") {
            if !known_cids.contains(parent.cid.as_str()) {
                anyhow::bail!(
                    "Goal {doc_id} history is incomplete: composite {} references unavailable parent {}",
                    commit.cid,
                    parent.cid
                );
            }
        }
    }

    // DefraDB's CID collection query reconstructs each historical document
    // and applies DocumentACP to it. Input CID order is preserved by the
    // runner, giving an exact commit-to-snapshot pairing.
    let cid_list = commits
        .iter()
        .map(|commit| format!(r#""{}""#, escape_graphql_string(&commit.cid)))
        .collect::<Vec<_>>()
        .join(", ");
    let response = access
        .execute(&format!(
            r#"query {{
                Goal(
                    cid: [{cid_list}],
                    docID: "{escaped_doc_id}",
                    showDeleted: true
                ) {{
                    _docID
                    goal_id
                    session_id
                    agent_did
                    status
                    consecutive_blocked_audits
                    wrapup_requested
                    wrapup_completed
                    token_budget
                    tokens_used
                    last_blocked_request_id
                    last_blocked_reason
                    last_failure
                    completion_evidence
                    created_at
                    updated_at
                }}
            }}"#
        ))
        .await
        .with_context(|| format!("reconstructing native Goal versions for {doc_id}"))?;
    let snapshots = graphql_rows_from_response(&response, "Goal")
        .into_iter()
        .map(serde_json::from_value::<HistoricalGoalDocument>)
        .collect::<std::result::Result<Vec<_>, _>>()
        .context("decoding historical Goal versions")?;
    if snapshots.len() != commits.len() {
        anyhow::bail!(
            "Goal {doc_id} history reconstruction returned {} snapshots for {} visible commits",
            snapshots.len(),
            commits.len()
        );
    }

    commits
        .into_iter()
        .zip(snapshots)
        .map(|(commit, snapshot)| {
            if snapshot.doc_id != doc_id {
                anyhow::bail!(
                    "Goal commit {} reconstructed document {}, expected {}",
                    commit.cid,
                    snapshot.doc_id,
                    doc_id
                );
            }
            if snapshot.session_id != expected_session_id {
                anyhow::bail!(
                    "Goal commit {} reconstructed session {}, expected {}",
                    commit.cid,
                    snapshot.session_id,
                    expected_session_id
                );
            }
            if snapshot.goal_id.trim().is_empty()
                || snapshot.agent_did.trim().is_empty()
                || snapshot.status.trim().is_empty()
            {
                anyhow::bail!(
                    "Goal commit {} reconstructed without complete goal, agent, and status identity",
                    commit.cid
                );
            }
            let mut parent_commit_cids = commit
                .heads
                .into_iter()
                .filter(|head| head.field_name == "_C")
                .map(|head| head.cid)
                .collect::<Vec<_>>();
            parent_commit_cids.sort();
            parent_commit_cids.dedup();
            Ok(TimelineGoalVersionRow {
                goal_doc_id: snapshot.doc_id,
                goal_id: snapshot.goal_id,
                session_id: snapshot.session_id,
                agent_did: snapshot.agent_did,
                commit_cid: commit.cid,
                height: commit.height,
                parent_commit_cids,
                status: snapshot.status,
                consecutive_blocked_audits: snapshot
                    .consecutive_blocked_audits
                    .unwrap_or_default()
                    .max(0),
                wrapup_requested: snapshot.wrapup_requested.unwrap_or(false),
                wrapup_completed: snapshot.wrapup_completed.unwrap_or(false),
                token_budget: snapshot.token_budget,
                tokens_used: snapshot.tokens_used.unwrap_or_default().max(0),
                last_blocked_request_id: snapshot.last_blocked_request_id,
                last_blocked_reason: snapshot.last_blocked_reason,
                last_failure: snapshot.last_failure,
                completion_evidence: snapshot.completion_evidence,
                created_at: snapshot.created_at,
                updated_at: snapshot.updated_at,
            })
        })
        .collect()
}
