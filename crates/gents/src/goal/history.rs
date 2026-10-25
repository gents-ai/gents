use std::collections::BTreeSet;

use serde::Deserialize;

use super::{GoalDocument, GOAL_FIELDS};
use crate::config_client::{ConfigClientError, ConfigRead};
use crate::graphql::escape_graphql_string;

type Result<T, E = GoalHistoryError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub(crate) enum GoalHistoryError {
    #[error("Goal history has an invalid native revision or status")]
    InvalidRevision,
    #[error("Goal history contains a duplicate native revision")]
    DuplicateRevision,
    #[error("Goal history is incomplete at revision {commit_cid}")]
    IncompleteRevision { commit_cid: String },
    #[error("Goal history has a cyclic or invalid native parent")]
    InvalidParent,
    #[error(
        "Goal assignment has concurrent native revisions; refresh before observing its outcome"
    )]
    ConcurrentRevisions,
    #[error(transparent)]
    Access(#[from] ConfigClientError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("Goal history query omitted its rows")]
    HistoryRowsOmitted,
    #[error("reading native Goal history for {doc_id}")]
    ReadHistory {
        doc_id: String,
        #[source]
        source: ConfigClientError,
    },
    #[error("Goal _commits response carried no data._commits array")]
    CommitsOmitted,
    #[error("decoding Goal composite commits")]
    DecodeCommits(#[source] serde_json::Error),
    #[error("Goal {doc_id} is visible but its native composite history is unavailable")]
    HistoryUnavailable { doc_id: String },
    #[error(
        "Goal {doc_id} history is incomplete: composite {commit_cid} references unavailable parent {parent_cid}"
    )]
    UnavailableParent {
        doc_id: String,
        commit_cid: String,
        parent_cid: String,
    },
    #[error("reconstructing native Goal versions for {doc_id}")]
    ReconstructVersions {
        doc_id: String,
        #[source]
        source: ConfigClientError,
    },
    #[error("Goal version query omitted its rows")]
    VersionRowsOmitted,
    #[error("decoding historical Goal versions")]
    DecodeVersions(#[source] serde_json::Error),
    #[error(
        "Goal {doc_id} history reconstruction returned {snapshots} snapshots for {commits} visible commits"
    )]
    SnapshotCount {
        doc_id: String,
        snapshots: usize,
        commits: usize,
    },
    #[error("Goal commit {commit_cid} reconstructed document {actual}, expected {expected}")]
    SnapshotDocument {
        commit_cid: String,
        actual: String,
        expected: String,
    },
    #[error("Goal commit {commit_cid} belongs to a different owner")]
    SnapshotOwner { commit_cid: String },
    #[error("Goal commit {commit_cid} reconstructed session {actual}, expected {expected}")]
    SnapshotSession {
        commit_cid: String,
        actual: String,
        expected: String,
    },
    #[error(
        "Goal commit {commit_cid} reconstructed without complete goal, agent, and status identity"
    )]
    SnapshotIdentity { commit_cid: String },
}

impl crate::config_client::TransactionFailure for GoalHistoryError {
    fn config_client_error(&self) -> Option<&ConfigClientError> {
        match self {
            Self::Access(error)
            | Self::ReadHistory { source: error, .. }
            | Self::ReconstructVersions { source: error, .. } => Some(error),
            _ => None,
        }
    }
}

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
            .ok_or(GoalHistoryError::HistoryRowsOmitted)?,
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
        .map_err(|source| GoalHistoryError::ReadHistory {
            doc_id: doc_id.to_owned(),
            source,
        })?;
    let commit_values = response
        .pointer("/data/_commits")
        .and_then(serde_json::Value::as_array)
        .ok_or(GoalHistoryError::CommitsOmitted)?;
    let mut commits = commit_values
        .iter()
        .cloned()
        .map(serde_json::from_value::<GoalCompositeCommit>)
        .collect::<std::result::Result<Vec<_>, _>>()
        .map_err(GoalHistoryError::DecodeCommits)?
        .into_iter()
        .filter(|commit| commit.field_name == "_C" && !commit.cid.trim().is_empty())
        .collect::<Vec<_>>();
    commits.sort_by(|left, right| (left.height, &left.cid).cmp(&(right.height, &right.cid)));
    commits.dedup_by(|left, right| left.cid == right.cid);
    if commits.is_empty() {
        return Err(GoalHistoryError::HistoryUnavailable {
            doc_id: doc_id.to_owned(),
        });
    }

    let known_cids = commits
        .iter()
        .map(|commit| commit.cid.as_str())
        .collect::<BTreeSet<_>>();
    for commit in &commits {
        for parent in commit.heads.iter().filter(|head| head.field_name == "_C") {
            if !known_cids.contains(parent.cid.as_str()) {
                return Err(GoalHistoryError::UnavailableParent {
                    doc_id: doc_id.to_owned(),
                    commit_cid: commit.cid.clone(),
                    parent_cid: parent.cid.clone(),
                });
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
        .map_err(|source| GoalHistoryError::ReconstructVersions {
            doc_id: doc_id.to_owned(),
            source,
        })?;
    let snapshots: Vec<GoalDocument> = serde_json::from_value(
        response
            .pointer("/data/Goal")
            .cloned()
            .ok_or(GoalHistoryError::VersionRowsOmitted)?,
    )
    .map_err(GoalHistoryError::DecodeVersions)?;
    if snapshots.len() != commits.len() {
        return Err(GoalHistoryError::SnapshotCount {
            doc_id: doc_id.to_owned(),
            snapshots: snapshots.len(),
            commits: commits.len(),
        });
    }

    commits
        .into_iter()
        .zip(snapshots)
        .map(|(commit, snapshot)| {
            if snapshot.doc_id != doc_id {
                return Err(GoalHistoryError::SnapshotDocument {
                    commit_cid: commit.cid,
                    actual: snapshot.doc_id,
                    expected: doc_id.to_owned(),
                });
            }
            if snapshot.node_did != expected_node_did {
                return Err(GoalHistoryError::SnapshotOwner {
                    commit_cid: commit.cid,
                });
            }
            if snapshot.session_id != expected_session_id {
                return Err(GoalHistoryError::SnapshotSession {
                    commit_cid: commit.cid,
                    actual: snapshot.session_id,
                    expected: expected_session_id.to_owned(),
                });
            }
            if snapshot.goal_id.trim().is_empty()
                || snapshot.node_did.trim().is_empty()
                || snapshot.status.trim().is_empty()
            {
                return Err(GoalHistoryError::SnapshotIdentity {
                    commit_cid: commit.cid,
                });
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
