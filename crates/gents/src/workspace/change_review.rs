use std::collections::{HashMap, HashSet};
use std::path::Path;

use anyhow::{ensure, Result};
use serde::{Deserialize, Serialize};

use super::WorkspacePathCapability;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileVersion {
    pub blob: String,
    pub executable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FileObservation {
    pub path: String,
    pub content: Option<FileVersion>,
    pub staged: Option<FileVersion>,
    pub unsaved: bool,
    pub safe_entry: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordedEdit {
    pub path: String,
    pub before: Option<FileVersion>,
    pub after: Option<FileVersion>,
    pub canonical: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum ReviewScope {
    Workspace {
        workspace_id: String,
        owner: String,
        base: String,
        capability: WorkspacePathCapability,
        tree: String,
    },
    RequestResource {
        node_did: String,
        root: String,
        capability: WorkspacePathCapability,
        snapshot: String,
    },
}

impl ReviewScope {
    fn well_formed(&self) -> bool {
        match self {
            Self::Workspace {
                workspace_id,
                owner,
                base,
                tree,
                ..
            } => [workspace_id, owner, base, tree]
                .into_iter()
                .all(|value| !value.is_empty()),
            Self::RequestResource {
                node_did,
                root,
                snapshot,
                ..
            } => [node_did, root, snapshot]
                .into_iter()
                .all(|value| !value.is_empty()),
        }
    }

    fn capability(&self) -> &WorkspacePathCapability {
        match self {
            Self::Workspace { capability, .. } | Self::RequestResource { capability, .. } => {
                capability
            }
        }
    }

    fn snapshot(&self) -> &str {
        match self {
            Self::Workspace { tree, .. } => tree,
            Self::RequestResource { snapshot, .. } => snapshot,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewReference {
    pub scope: ReviewScope,
    pub request_doc_id: String,
    pub review_id: String,
}

impl ReviewReference {
    fn well_formed(&self) -> bool {
        !self.request_doc_id.is_empty() && !self.review_id.is_empty() && self.scope.well_formed()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChangeReview {
    pub reference: ReviewReference,
    pub before_snapshot: String,
    pub edits: Vec<RecordedEdit>,
}

/// These observations must come from the existing authorization, placement,
/// snapshot and action-journal owners. Client assertions cannot supply them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UndoEvidence {
    pub expected: ReviewReference,
    pub authorized: bool,
    pub resource_verified: bool,
    pub snapshots_available: bool,
    pub no_active_writer: bool,
    pub receipt: Option<ReviewReference>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UndoDecision {
    Denied,
    Recovered,
    Restore { edits: Vec<RecordedEdit> },
}

impl UndoDecision {
    /// A preview of the planned result, not an acknowledgement of host writes.
    pub fn project_files(&self, files: &[FileObservation]) -> Vec<FileObservation> {
        let Self::Restore { edits } = self else {
            return files.to_vec();
        };
        let edits: HashMap<_, _> = edits
            .iter()
            .map(|edit| (edit.path.as_str(), edit))
            .collect();
        files
            .iter()
            .map(|file| {
                let mut projected = file.clone();
                if let Some(edit) = edits.get(file.path.as_str()) {
                    projected.content.clone_from(&edit.before);
                }
                projected
            })
            .collect()
    }
}

impl ChangeReview {
    fn structurally_valid(&self) -> bool {
        let WorkspacePathCapability::ExactPaths { paths } = self.reference.scope.capability()
        else {
            return false;
        };
        let allowed: HashSet<_> = paths.iter().map(String::as_str).collect();
        let mut seen = HashSet::new();
        self.reference.well_formed()
            && !self.before_snapshot.is_empty()
            && !self.edits.is_empty()
            && self.edits.iter().all(|edit| {
                edit.canonical
                    && allowed.contains(edit.path.as_str())
                    && seen.insert(&edit.path)
                    && [&edit.before, &edit.after].into_iter().all(|value| {
                        value
                            .as_ref()
                            .is_none_or(|version| !version.blob.is_empty())
                    })
                    && edit.before != edit.after
            })
    }

    pub fn plan_undo(&self, files: &[FileObservation], evidence: &UndoEvidence) -> UndoDecision {
        if !evidence.authorized
            || !evidence.resource_verified
            || evidence.expected != self.reference
            || !self.structurally_valid()
        {
            return UndoDecision::Denied;
        }
        if evidence.receipt.as_ref() == Some(&self.reference) {
            return UndoDecision::Recovered;
        }
        let observed: HashMap<_, _> = files
            .iter()
            .map(|file| (file.path.as_str(), file))
            .collect();
        if evidence.receipt.is_some()
            || !evidence.snapshots_available
            || !evidence.no_active_writer
            || observed.len() != files.len()
            || !self.edits.iter().all(|edit| {
                observed.get(edit.path.as_str()).is_some_and(|file| {
                    file.safe_entry && !file.unsaved && file.content == edit.after
                })
            })
        {
            return UndoDecision::Denied;
        }
        UndoDecision::Restore {
            edits: self.edits.clone(),
        }
    }
}

/// The caller resolves and authorizes the request and workspace through their
/// existing owners. Only retained full object IDs are accepted; mutable branch
/// names and the checkout's current HEAD cannot redefine a recorded review.
pub fn inspect_immutable_changes(
    root: &Path,
    before_tree: &str,
    reference: ReviewReference,
) -> Result<ChangeReview> {
    ensure!(
        reference.well_formed(),
        "incomplete workspace review reference"
    );
    ensure!(
        reference.scope.capability().is_exact(),
        "review requires exact workspace paths"
    );
    let snapshot = reference.scope.snapshot();
    let mut revisions = vec![before_tree, snapshot];
    match &reference.scope {
        ReviewScope::Workspace { base, .. } => revisions.push(base),
        ReviewScope::RequestResource {
            root: admitted_root,
            ..
        } => {
            let admitted_root = Path::new(admitted_root);
            ensure!(
                admitted_root.is_absolute() && std::fs::canonicalize(root)? == admitted_root,
                "workspace review does not match the request resource"
            );
        }
    }
    for revision in revisions {
        ensure!(
            matches!(revision.len(), 40 | 64)
                && revision.bytes().all(|byte| byte.is_ascii_hexdigit()),
            "workspace review requires retained full Git object IDs"
        );
        super::adapter::require_snapshot_tree(root, revision)?;
    }
    reference.scope.capability().validate()?;
    let edits = super::adapter::inspect_snapshot_edits(
        root,
        before_tree,
        snapshot,
        reference.scope.capability(),
    )?;
    Ok(ChangeReview {
        reference,
        before_snapshot: before_tree.to_owned(),
        edits,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generated_change_review_cases_drive_undo_planner() {
        #[derive(Deserialize)]
        struct Case {
            name: String,
            files: Vec<FileObservation>,
            review: ChangeReview,
            evidence: UndoEvidence,
            expected_files: Vec<FileObservation>,
            disposition: String,
        }
        let snapshot: serde_json::Value = gents_lean_contract::load_contract_snapshot().unwrap();
        let cases: Vec<Case> =
            serde_json::from_value(snapshot["workspace_change_review_cases"].clone()).unwrap();
        assert!(!cases.is_empty());
        for case in cases {
            let decision = case.review.plan_undo(&case.files, &case.evidence);
            let disposition = match decision {
                UndoDecision::Denied => "denied",
                UndoDecision::Recovered => "recovered",
                UndoDecision::Restore { .. } => "accepted",
            };
            assert_eq!(disposition, case.disposition, "{}", case.name);
            assert_eq!(
                decision.project_files(&case.files),
                case.expected_files,
                "{}",
                case.name
            );
        }
    }
}
