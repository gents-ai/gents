use std::collections::BTreeSet;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

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
    #[serde(default)]
    links: Option<Vec<GoalCommitHead>>,
}

#[derive(Debug, Deserialize)]
struct GoalCommitHead {
    #[serde(default)]
    cid: String,
    #[serde(default, rename = "fieldName")]
    field_name: String,
    #[serde(default)]
    height: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
struct ReuseFacts {
    parent_count: usize,
    metadata_complete: bool,
    assignment_written: bool,
}

impl ReuseFacts {
    fn can_reuse(&self) -> bool {
        self.parent_count == 1 && self.metadata_complete && !self.assignment_written
    }
}

impl GoalCompositeCommit {
    fn reuse_facts(&self) -> ReuseFacts {
        let complete = |link: &GoalCommitHead| {
            !link.cid.is_empty() && link.height.is_some_and(|height| height >= 0)
        };
        ReuseFacts {
            parent_count: self.heads.len(),
            metadata_complete: self.heads.iter().all(|head| {
                complete(head)
                    && head.field_name == "_C"
                    && head.height.is_some_and(|height| height < self.height)
            }) && self.links.as_ref().is_some_and(|links| {
                links.iter().all(|link| {
                    complete(link)
                        && GOAL_FIELDS
                            .split_whitespace()
                            .any(|field| field == link.field_name)
                })
            }),
            assignment_written: self.links.as_ref().is_some_and(|links| {
                links.iter().any(|link| {
                    matches!(
                        link.field_name.as_str(),
                        "_docID"
                            | "goal_id"
                            | "node_did"
                            | "session_id"
                            | "assignment_root_request_doc_id"
                            | "status"
                    )
                })
            }),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct GoalVersionIdentity {
    pub(crate) doc_id: String,
    pub(crate) goal_id: String,
    pub(crate) node_did: String,
    pub(crate) session_id: String,
    pub(crate) assignment_root_request_doc_id: Option<String>,
    pub(crate) status: String,
}

impl From<&GoalDocument> for GoalVersionIdentity {
    fn from(goal: &GoalDocument) -> Self {
        Self {
            doc_id: goal.doc_id.clone(),
            goal_id: goal.goal_id.clone(),
            node_did: goal.node_did.clone(),
            session_id: goal.session_id.clone(),
            assignment_root_request_doc_id: goal.assignment_root_request_doc_id.clone(),
            status: goal.status.clone(),
        }
    }
}

/// Every native revision retains its assignment projection and causal edges.
/// Full payloads are available only where reconstructed, including every
/// assignment frontier. An inherited projection is never a display snapshot.
#[derive(Debug, Clone)]
pub(crate) struct GoalAssignmentVersion {
    pub(crate) identity: GoalVersionIdentity,
    pub(crate) snapshot: Option<GoalDocument>,
    pub(crate) commit_cid: String,
    pub(crate) height: i64,
    pub(crate) parent_commit_cids: Vec<String>,
}

impl GoalAssignmentVersion {
    pub(crate) fn from_snapshot(
        goal: GoalDocument,
        commit_cid: String,
        height: i64,
        parent_commit_cids: Vec<String>,
    ) -> Self {
        Self {
            identity: GoalVersionIdentity::from(&goal),
            snapshot: Some(goal),
            commit_cid,
            height,
            parent_commit_cids,
        }
    }
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
    load_versions_for_session(access, node_did, session_id, false)
        .await?
        .into_iter()
        .map(|version| {
            Ok(GoalVersion {
                goal: version
                    .snapshot
                    .context("Goal history revision has no native snapshot")?,
                commit_cid: version.commit_cid,
                height: version.height,
                parent_commit_cids: version.parent_commit_cids,
            })
        })
        .collect()
}

pub(crate) async fn load_assignment_versions_for_session(
    access: &impl ConfigRead,
    node_did: &str,
    session_id: &str,
) -> Result<Vec<GoalAssignmentVersion>> {
    load_versions_for_session(access, node_did, session_id, true).await
}

async fn load_versions_for_session(
    access: &impl ConfigRead,
    node_did: &str,
    session_id: &str,
    reuse: bool,
) -> Result<Vec<GoalAssignmentVersion>> {
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
        versions.extend(
            load_goal_document_versions(access, node_did, session_id, &doc.doc_id, reuse).await?,
        );
    }
    Ok(versions)
}

async fn load_goal_document_versions(
    access: &impl ConfigRead,
    expected_node_did: &str,
    expected_session_id: &str,
    doc_id: &str,
    reuse: bool,
) -> Result<Vec<GoalAssignmentVersion>> {
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
                    links {{ cid height fieldName }}
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

    use rapidhash::{HashMapExt, RapidHashMap};
    let required = commits
        .iter()
        .filter(|commit| !reuse || !commit.reuse_facts().can_reuse())
        .collect::<Vec<_>>();
    let mut snapshots = load_snapshots(
        access,
        expected_node_did,
        expected_session_id,
        doc_id,
        &required,
    )
    .await?;
    let mut versions: Vec<GoalAssignmentVersion> = Vec::with_capacity(commits.len());
    let mut positions = RapidHashMap::new();
    for commit in &commits {
        let parents = commit
            .heads
            .iter()
            .filter(|head| head.field_name == "_C")
            .map(|head| head.cid.clone())
            .collect::<Vec<_>>();
        let version = if let Some(snapshot) = snapshots.remove(&commit.cid) {
            GoalAssignmentVersion::from_snapshot(
                snapshot,
                commit.cid.clone(),
                commit.height,
                parents,
            )
        } else {
            anyhow::ensure!(
                commit.reuse_facts().can_reuse(),
                "Goal revision has no native reconstruction"
            );
            let head = &commit.heads[0];
            let parent: &GoalAssignmentVersion = &versions[*positions
                .get(head.cid.as_str())
                .context("Goal history parent is unavailable before projection")?];
            anyhow::ensure!(
                Some(parent.height) == head.height,
                "Goal history parent metadata disagrees with its native revision"
            );
            GoalAssignmentVersion {
                identity: parent.identity.clone(),
                snapshot: None,
                commit_cid: commit.cid.clone(),
                height: commit.height,
                parent_commit_cids: parents,
            }
        };
        positions.insert(commit.cid.as_str(), versions.len());
        versions.push(version);
    }
    if !reuse {
        return Ok(versions);
    }
    let frontier = super::request_assignment::latest_assignment_versions(&versions)?;
    let missing = frontier
        .into_iter()
        .filter(|version| version.snapshot.is_none())
        .map(|version| &commits[positions[version.commit_cid.as_str()]])
        .collect::<Vec<_>>();
    let complete = load_snapshots(
        access,
        expected_node_did,
        expected_session_id,
        doc_id,
        &missing,
    )
    .await?;
    for (cid, snapshot) in complete {
        let version = &mut versions[positions[cid.as_str()]];
        anyhow::ensure!(
            version.identity == GoalVersionIdentity::from(&snapshot),
            "Goal assignment projection disagrees with its native frontier"
        );
        version.snapshot = Some(snapshot);
    }
    Ok(versions)
}

async fn load_snapshots(
    access: &impl ConfigRead,
    expected_node_did: &str,
    expected_session_id: &str,
    doc_id: &str,
    commits: &[&GoalCompositeCommit],
) -> Result<rapidhash::RapidHashMap<String, GoalDocument>> {
    use rapidhash::{HashMapExt, RapidHashMap};
    let escaped_doc_id = escape_graphql_string(doc_id);
    let mut snapshots = RapidHashMap::new();
    for (batch_index, batch) in commits.chunks(32).enumerate() {
        let cid_list = batch
            .iter()
            .map(|commit| format!(r#""{}""#, escape_graphql_string(&commit.cid)))
            .collect::<Vec<_>>()
            .join(", ");
        let response = access.execute_read(&format!(r#"query {{
            Goal(cid: [{cid_list}], docID: "{escaped_doc_id}", showDeleted: true) {{ {GOAL_FIELDS} }}
        }}"#)).await.with_context(|| format!(
            "reconstructing native Goal versions for {doc_id}, batch {} ({} commits)",
            batch_index + 1, batch.len()))?;
        let rows: Vec<GoalDocument> = serde_json::from_value(
            response
                .pointer("/data/Goal")
                .cloned()
                .context("Goal version query omitted its rows")?,
        )
        .context("decoding historical Goal versions")?;
        anyhow::ensure!(
            rows.len() == batch.len(),
            "Goal {doc_id} history batch {} returned {} snapshots for {} visible commits",
            batch_index + 1,
            rows.len(),
            batch.len()
        );
        for (commit, snapshot) in batch.iter().zip(rows) {
            anyhow::ensure!(
                snapshot.doc_id == doc_id,
                "Goal commit {} reconstructed document {}, expected {}",
                commit.cid,
                snapshot.doc_id,
                doc_id
            );
            anyhow::ensure!(
                snapshot.node_did == expected_node_did,
                "Goal commit {} belongs to a different owner",
                commit.cid
            );
            anyhow::ensure!(
                snapshot.session_id == expected_session_id,
                "Goal commit {} reconstructed session {}, expected {}",
                commit.cid,
                snapshot.session_id,
                expected_session_id
            );
            anyhow::ensure!(
                !snapshot.goal_id.trim().is_empty()
                    && !snapshot.node_did.trim().is_empty()
                    && !snapshot.status.trim().is_empty(),
                "Goal commit {} reconstructed without complete goal, agent, and status identity",
                commit.cid
            );
            snapshots.insert(commit.cid.clone(), snapshot);
        }
    }
    Ok(snapshots)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod native_projection_tests;
