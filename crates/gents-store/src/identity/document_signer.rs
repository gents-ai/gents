use rapidhash::{HashMapExt, RapidHashMap};

use crate::config_client::{ConfigClientError, ConfigRead};
use crate::graphql::{
    composite_commits_at, escape_graphql_string, CommitSignature, CompositeCommit,
    GraphqlResponseError,
};

#[derive(Debug, thiserror::Error)]
pub enum DocumentSignerError {
    #[error("reading the commit history of {doc_ids:?}")]
    Read {
        doc_ids: Vec<String>,
        #[source]
        source: ConfigClientError,
    },
    #[error("decoding the commit history of document {doc_id}")]
    Decode {
        doc_id: String,
        #[source]
        source: GraphqlResponseError,
    },
    #[error("the store returned no commit history for document {doc_id}")]
    MissingHistory { doc_id: String },
}

/// The DID that signed every composite commit (each create and update) of
/// each document, read from the commits' signature headers. A document with
/// an unsigned composite commit, a signature naming no key, or composite
/// commits from two identities maps to `None`.
pub async fn document_signers(
    reader: &dyn ConfigRead,
    doc_ids: &[&str],
) -> Result<RapidHashMap<String, Option<String>>, DocumentSignerError> {
    let mut signers = RapidHashMap::with_capacity(doc_ids.len());
    if doc_ids.is_empty() {
        return Ok(signers);
    }
    let selections = doc_ids
        .iter()
        .enumerate()
        .map(|(index, doc_id)| {
            format!(
                r#"d{index}: _commits(docID: "{}") {{ cid height fieldName signature {{ identity type }} }}"#,
                escape_graphql_string(doc_id)
            )
        })
        .collect::<Vec<_>>()
        .join(" ");
    let response = reader
        .execute_read(&format!("{{ {selections} }}"))
        .await
        .map_err(|source| DocumentSignerError::Read {
            doc_ids: doc_ids.iter().map(|id| (*id).to_owned()).collect(),
            source,
        })?;
    for (index, doc_id) in doc_ids.iter().enumerate() {
        let field = format!("d{index}");
        let commits =
            composite_commits_at(response["data"].get(&field), &field).map_err(|source| {
                DocumentSignerError::Decode {
                    doc_id: (*doc_id).to_owned(),
                    source,
                }
            })?;
        if commits.is_empty() {
            return Err(DocumentSignerError::MissingHistory {
                doc_id: (*doc_id).to_owned(),
            });
        }
        signers.insert((*doc_id).to_owned(), sole_signer(&commits));
    }
    Ok(signers)
}

fn sole_signer(commits: &[CompositeCommit]) -> Option<String> {
    let mut signers = commits
        .iter()
        .map(|commit| commit.signature.as_ref().and_then(commit_signer));
    let first = signers.next()??;
    signers
        .all(|signer| signer.as_deref() == Some(first.as_str()))
        .then_some(first)
}

fn commit_signer(signature: &CommitSignature) -> Option<String> {
    let key_type = match signature.signature_type.as_str() {
        "ES256K" => crypto::KeyType::Secp256k1,
        "ES256" => crypto::KeyType::Secp256r1,
        "EdDSA" => crypto::KeyType::Ed25519,
        "BLS_AUG_V1" => crypto::KeyType::Bls12381,
        _ => return None,
    };
    crypto::public_key_from_string(key_type, &signature.identity)
        .ok()?
        .did()
        .ok()
}
