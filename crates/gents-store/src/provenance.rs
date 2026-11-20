//! Who wrote a document, read from DefraDB's verified version signatures.
//! This is provenance; authorization stays with ACP.

use std::collections::BTreeSet;

use serde::Serialize;

use crate::config_client::{ConfigClientError, ConfigRead};
use crate::graphql::{
    composite_commits_at, composite_commits_query, unique_newest_composite_commit, CompositeCommit,
    GraphqlResponseError,
};
use crate::identity::{commit_signer_did, IdentityError};

/// The DIDs behind a document's versions. Each version's composite (`_C`)
/// block is signed and links its field blocks by CID, so its signature covers
/// them; DefraDB leaves an update's field blocks unsigned, so only composites
/// count.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct DocumentSigners {
    /// The genesis (height 1) signer; none when it is unsigned, absent from
    /// the readable history, or two genesis versions tie.
    pub created: Option<String>,
    /// The newest version's signer; none when it is unsigned or concurrent
    /// heads tie at the newest height.
    pub latest: Option<String>,
    /// Every DID that signed a readable version, so an edit in between shows
    /// even when `created` and `latest` agree.
    pub writers: BTreeSet<String>,
    /// Some version is not attributed to a verified signature: it carries
    /// none, or the readable history is empty or lacks its genesis. DefraDB
    /// drops commits it cannot authorize or resolve without an error, so a
    /// missing version reads as unattributed rather than absent.
    pub unsigned: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum DocumentSignersError {
    #[error("read the version history of document {doc_id}")]
    Read {
        doc_id: String,
        #[source]
        source: ConfigClientError,
    },
    #[error("decode the version history of document {doc_id}")]
    Decode {
        doc_id: String,
        #[source]
        source: GraphqlResponseError,
    },
    #[error("name the signer of version {cid} of document {doc_id}")]
    Signer {
        doc_id: String,
        cid: String,
        #[source]
        source: IdentityError,
    },
}

/// Read one document's version history as `access` and name its signers.
pub async fn document_signers(
    access: &dyn ConfigRead,
    doc_id: &str,
) -> Result<DocumentSigners, DocumentSignersError> {
    let response = access
        .execute_read(&composite_commits_query(doc_id))
        .await
        .map_err(|source| DocumentSignersError::Read {
            doc_id: doc_id.to_owned(),
            source,
        })?;
    let commits = composite_commits_at(
        response.get("data").and_then(|data| data.get("_commits")),
        "_commits",
    )
    .map_err(|source| DocumentSignersError::Decode {
        doc_id: doc_id.to_owned(),
        source,
    })?;
    signers_of(doc_id, commits)
}

fn signers_of(
    doc_id: &str,
    commits: Vec<CompositeCommit>,
) -> Result<DocumentSigners, DocumentSignersError> {
    let signed = commits
        .iter()
        .map(|commit| {
            let did = commit
                .signature
                .as_ref()
                .filter(|signature| !signature.identity.is_empty())
                .map(|signature| commit_signer_did(&signature.identity, &signature.signature_type))
                .transpose()
                .map_err(|source| DocumentSignersError::Signer {
                    doc_id: doc_id.to_owned(),
                    cid: commit.cid.clone(),
                    source,
                })?;
            Ok((commit.cid.as_str(), commit.height, did))
        })
        .collect::<Result<Vec<_>, DocumentSignersError>>()?;
    let signer_of = |cid: &str| {
        signed
            .iter()
            .find(|(signed_cid, _, _)| *signed_cid == cid)
            .and_then(|(_, _, did)| did.clone())
    };
    let genesis = signed
        .iter()
        .filter(|(_, height, _)| *height == 1)
        .collect::<Vec<_>>();
    let created = match genesis.as_slice() {
        [(_, _, did)] => did.clone(),
        _ => None,
    };
    let latest = match unique_newest_composite_commit(commits.clone(), "name the latest signer") {
        Ok(newest) => newest.and_then(|newest| signer_of(&newest.cid)),
        Err(GraphqlResponseError::AmbiguousVersion { .. }) => None,
        Err(source) => {
            return Err(DocumentSignersError::Decode {
                doc_id: doc_id.to_owned(),
                source,
            })
        }
    };
    let writers = signed
        .iter()
        .filter_map(|(_, _, did)| did.clone())
        .collect();
    let unsigned = genesis.is_empty() || signed.iter().any(|(_, _, did)| did.is_none());
    Ok(DocumentSigners {
        created,
        latest,
        writers,
        unsigned,
    })
}

#[cfg(test)]
mod tests;
