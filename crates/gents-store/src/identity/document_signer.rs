use rapidhash::{HashMapExt, RapidHashMap};
use serde_json::Value;

use crate::config_client::{ConfigClientError, ConfigRead};
use crate::graphql::escape_graphql_string;

#[derive(Debug, thiserror::Error)]
pub enum DocumentSignerError {
    #[error("reading the commit history of {doc_ids:?}")]
    Read {
        doc_ids: Vec<String>,
        #[source]
        source: ConfigClientError,
    },
    #[error("the store returned no commit history for document {doc_id}")]
    MissingHistory { doc_id: String },
}

/// The DID that signed every commit of each document, read from the
/// commits' signature headers. A document with an unsigned commit, a
/// signature naming no key, or commits from two identities maps to `None`.
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
                r#"d{index}: _commits(docID: "{}") {{ signature {{ identity type }} }}"#,
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
        let commits = response["data"][format!("d{index}")]
            .as_array()
            .filter(|commits| !commits.is_empty())
            .ok_or_else(|| DocumentSignerError::MissingHistory {
                doc_id: (*doc_id).to_owned(),
            })?;
        signers.insert((*doc_id).to_owned(), sole_signer(commits));
    }
    Ok(signers)
}

fn sole_signer(commits: &[Value]) -> Option<String> {
    let mut signers = commits
        .iter()
        .map(|commit| commit_signer(&commit["signature"]));
    let first = signers.next()??;
    signers
        .all(|signer| signer.as_deref() == Some(first.as_str()))
        .then_some(first)
}

fn commit_signer(signature: &Value) -> Option<String> {
    let key_type = match signature["type"].as_str()? {
        "ES256K" => crypto::KeyType::Secp256k1,
        "ES256" => crypto::KeyType::Secp256r1,
        "EdDSA" => crypto::KeyType::Ed25519,
        "BLS_AUG_V1" => crypto::KeyType::Bls12381,
        _ => return None,
    };
    crypto::public_key_from_string(key_type, signature["identity"].as_str()?)
        .ok()?
        .did()
        .ok()
}
