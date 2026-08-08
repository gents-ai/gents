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

/// One exact DefraDB document version together with its cryptographically
/// verified commit author.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedDocumentVersionRef {
    pub version: DocumentVersionRef,
    pub signer_did: String,
}

impl SignedDocumentVersionRef {
    pub(crate) fn new(version: DocumentVersionRef, signer_did: impl Into<String>) -> Self {
        Self {
            version,
            signer_did: signer_did.into(),
        }
    }
}

/// Provenance boundary for one request execution.
///
/// `source` is the sole current composite head admitted before the claim.
/// `claim` is the exact target-agent-authored child version whose only
/// composite parent is `source`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RequestExecutionProvenance {
    pub source: SignedDocumentVersionRef,
    pub claim: SignedDocumentVersionRef,
}

impl RequestExecutionProvenance {
    pub(crate) fn new(source: SignedDocumentVersionRef, claim: SignedDocumentVersionRef) -> Self {
        Self { source, claim }
    }

    pub(crate) fn validate_for_request(
        &self,
        request_doc_id: &str,
        target_agent_did: &str,
    ) -> anyhow::Result<()> {
        if request_doc_id.trim().is_empty() {
            anyhow::bail!("request provenance requires a non-empty document id");
        }
        if target_agent_did.trim().is_empty() {
            anyhow::bail!("request provenance requires a non-empty target agent DID");
        }
        if self.source.version.doc_id != request_doc_id
            || self.claim.version.doc_id != request_doc_id
        {
            anyhow::bail!(
                "request provenance source and claim must both reference document {request_doc_id}"
            );
        }
        if self.source.version.composite_commit_cid.trim().is_empty()
            || self.claim.version.composite_commit_cid.trim().is_empty()
        {
            anyhow::bail!("request provenance source and claim CIDs must be non-empty");
        }
        if self.source.version.composite_commit_cid == self.claim.version.composite_commit_cid {
            anyhow::bail!("request provenance source and claim CIDs must be distinct");
        }
        if self.source.signer_did.trim().is_empty() || self.claim.signer_did.trim().is_empty() {
            anyhow::bail!("request provenance source and claim signer DIDs must be non-empty");
        }
        if self.claim.signer_did != target_agent_did {
            anyhow::bail!(
                "request provenance claim signer {} does not match target agent {}",
                self.claim.signer_did,
                target_agent_did
            );
        }
        Ok(())
    }
}

#[doc(hidden)]
pub(crate) fn test_request_execution_provenance(
    doc_id: &str,
    claim_signer_did: &str,
) -> RequestExecutionProvenance {
    RequestExecutionProvenance::new(
        SignedDocumentVersionRef::new(
            DocumentVersionRef::new(doc_id, "bafy-source-1"),
            "did:key:source",
        ),
        SignedDocumentVersionRef::new(
            DocumentVersionRef::new(doc_id, "bafy-claim-1"),
            claim_signer_did,
        ),
    )
}
