use std::collections::BTreeSet;

use anyhow::{Context, Result};
use serde::Deserialize;

use super::{GoalDocument, GOAL_FIELDS};
use crate::config_client::ConfigRead;
use crate::graphql::escape_graphql_string;

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

#[derive(Debug, Clone)]
pub(crate) struct GoalVersion {
    pub(crate) goal: GoalDocument,
    pub(crate) commit_cid: String,
    pub(crate) height: i64,
    pub(crate) parent_commit_cids: Vec<String>,
}

pub(crate) async fn load_goal_versions_for_session(
    access: &impl ConfigRead,
    node_did: &str,
    session_id: &str,
) -> Result<Vec<GoalVersion>> {
    let escaped_owner = escape_graphql_string(node_did);
    let escaped_session_id = escape_graphql_string(session_id);
    let query = format!(
        r#"{{
            Goal(
                filter: {{ node_did: {{ _eq: "{escaped_owner}" }}, session_id: {{ _eq: "{escaped_session_id}" }} }},
                showDeleted: true
            ) {{ _docID }}
        }}"#
    );
    let response = access.execute_read(&query).await?;
    let docs: Vec<GoalDocumentRef> = serde_json::from_value(
        response
            .pointer("/data/Goal")
            .cloned()
            .context("Goal history query omitted its rows")?,
    )?;
    let mut versions = Vec::new();
    for doc in docs {
        versions
            .extend(load_goal_document_versions(access, node_did, session_id, &doc.doc_id).await?);
    }
    Ok(versions)
}

async fn load_goal_document_versions(
    access: &impl ConfigRead,
    expected_node_did: &str,
    expected_session_id: &str,
    doc_id: &str,
) -> Result<Vec<GoalVersion>> {
    // Do not send a fieldName filter: DefraDB evaluates that filter in memory
    // and currently degrades malformed filters to no filter.
    let escaped_doc_id = escape_graphql_string(doc_id);
    let response = access
        .execute_read(&format!(
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

    // The CID collection query reconstructs each historical document through
    // DefraDB's DocumentACP; input CID order is preserved by the runner.
    let cid_list = commits
        .iter()
        .map(|commit| format!(r#""{}""#, escape_graphql_string(&commit.cid)))
        .collect::<Vec<_>>()
        .join(", ");
    let response = access
        .execute_read(&format!(
            r#"query {{
                Goal(
                    cid: [{cid_list}],
                    docID: "{escaped_doc_id}",
                    showDeleted: true
                ) {{
                    {GOAL_FIELDS}
                }}
            }}"#
        ))
        .await
        .with_context(|| format!("reconstructing native Goal versions for {doc_id}"))?;
    let snapshots: Vec<GoalDocument> = serde_json::from_value(
        response
            .pointer("/data/Goal")
            .cloned()
            .context("Goal version query omitted its rows")?,
    )
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
            anyhow::ensure!(snapshot.node_did == expected_node_did, "Goal commit {} belongs to a different owner", commit.cid);
            if snapshot.session_id != expected_session_id {
                anyhow::bail!(
                    "Goal commit {} reconstructed session {}, expected {}",
                    commit.cid,
                    snapshot.session_id,
                    expected_session_id
                );
            }
            if snapshot.goal_id.trim().is_empty()
                || snapshot.node_did.trim().is_empty()
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
            Ok(GoalVersion {
                goal: snapshot,
                commit_cid: commit.cid,
                height: commit.height,
                parent_commit_cids,
            })
        })
        .collect()
}
