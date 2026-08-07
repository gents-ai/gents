use serde::{Deserialize, Serialize};

/// An immutable point in one DefraDB document's history.
///
/// `_docID` is the stable document identity. `composite_commit_cid` is the
/// content-addressed composite commit that reconstructs the exact snapshot the
/// runtime consumed. Neither value substitutes for the other.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentVersionRef {
    pub doc_id: String,
    pub composite_commit_cid: String,
}

impl DocumentVersionRef {
    pub(crate) fn new(doc_id: impl Into<String>, composite_commit_cid: impl Into<String>) -> Self {
        Self {
            doc_id: doc_id.into(),
            composite_commit_cid: composite_commit_cid.into(),
        }
    }
}
