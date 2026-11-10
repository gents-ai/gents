use anyhow::Result;
use gents_protocol::output::MessagePublication;
use std::collections::BTreeSet;

use super::access::{rows_value, ReadAccess, ReadCache, SessionHeaderRows};
use super::CanonicalOutputReadError;
use gents_protocol::output::rows::{decode_transcript_message_row, AGENT_MESSAGE_FIELDS};
use gents_protocol::session::session_scope_filter;

pub(super) async fn load_header(
    access: ReadAccess<'_, '_>,
    header_doc_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
    cache: &mut ReadCache,
) -> Result<gents_protocol::output::rows::TranscriptMessageRow> {
    find_header(access, header_doc_id, node_did, requester_did, cache)
        .await?
        .ok_or_else(|| {
            anyhow::Error::new(CanonicalOutputReadError::MissingCanonicalHeader {
                header_doc_id: header_doc_id.to_owned(),
            })
        })
}

/// `None` when no header is observed for this reader.
async fn find_header(
    access: ReadAccess<'_, '_>,
    header_doc_id: &str,
    node_did: &str,
    requester_did: Option<&str>,
    cache: &mut ReadCache,
) -> Result<Option<gents_protocol::output::rows::TranscriptMessageRow>> {
    anyhow::ensure!(
        !header_doc_id.trim().is_empty(),
        "canonical header id is blank"
    );
    if let Some(header) = cache.headers.get(header_doc_id) {
        return Ok(Some(header.clone()));
    }
    let mut headers = if let Some(observed) = cache.observed.get(header_doc_id) {
        vec![observed.clone()]
    } else {
        let requester = requester_did
            .map(|did| format!(r#""{}""#, crate::store::graphql::escape_graphql_string(did)))
            .unwrap_or_else(|| "null".into());
        let query = format!(
            r#"{{ AgentMessage(filter: {{ _docID: {{ _eq: "{}" }}, node_did: {{ _eq: "{}" }}, requester_did: {{ _eq: {requester} }} }}, limit: 2) {{ {AGENT_MESSAGE_FIELDS} }} }}"#,
            crate::store::graphql::escape_graphql_string(header_doc_id),
            crate::store::graphql::escape_graphql_string(node_did)
        );
        let response = access.query(&query, "load_canonical_header").await?;
        rows_value(&response, "AgentMessage")?
            .iter()
            .map(|row| decode_transcript_message_row(row).map_err(anyhow::Error::from))
            .collect::<Result<Vec<_>>>()?
    };
    match headers.len() {
        0 => Ok(None),
        1 => {
            let header = headers.pop().expect("one canonical header checked");
            anyhow::ensure!(
                header.doc_id == header_doc_id
                    && header.message.node_did == node_did
                    && header.message.requester_did.as_deref() == requester_did,
                "canonical header scope mismatch"
            );
            // A pinned physical ID does not make a second header at the same
            // logical coordinate harmless. Include visible key/sequence twins
            // in the shared identity validator before caching the observation.
            let mut observed =
                coordinate_twins(access, &header, node_did, requester_did, cache).await?;
            observed.push(header.clone());
            let facts = observed
                .iter()
                .map(|row| gents_protocol::output::origin::ObservedMessage {
                    doc_id: &row.doc_id,
                    message: &row.message,
                })
                .collect::<Vec<_>>();
            gents_protocol::output::origin::lookup_message(
                &facts,
                &[],
                header_doc_id,
                node_did,
                requester_did,
            )
            .map_err(anyhow::Error::new)?;
            cache.headers.insert(header.doc_id.clone(), header.clone());
            Ok(Some(header))
        }
        _ => Err(anyhow::Error::new(
            CanonicalOutputReadError::AmbiguousCanonicalHeader {
                header_doc_id: header_doc_id.to_owned(),
            },
        )),
    }
}

async fn coordinate_twins(
    access: ReadAccess<'_, '_>,
    header: &gents_protocol::output::rows::TranscriptMessageRow,
    node_did: &str,
    requester_did: Option<&str>,
    cache: &mut ReadCache,
) -> Result<Vec<gents_protocol::output::rows::TranscriptMessageRow>> {
    let session_id = &header.message.session_id;
    let scope = session_scope_filter(node_did, session_id, requester_did);
    let key = &header.message.message_key;
    let sequence = header.message.sequence;
    if cache.sessions.is_none() {
        let escaped = crate::store::graphql::escape_graphql_string(key);
        let query = format!(
            r#"{{
            key_matches: AgentMessage(filter: {{ {scope}, message_key: {{ _eq: "{escaped}" }} }}, limit: 2) {{ {AGENT_MESSAGE_FIELDS} }}
            sequence_matches: AgentMessage(filter: {{ {scope}, sequence: {{ _eq: {sequence} }} }}, limit: 2) {{ {AGENT_MESSAGE_FIELDS} }}
        }}"#
        );
        let response = access
            .query(&query, "validate_canonical_header_coordinate")
            .await?;
        let mut seen = BTreeSet::new();
        let mut rows = Vec::new();
        for selection in ["key_matches", "sequence_matches"] {
            for value in rows_value(&response, selection)? {
                let row = decode_transcript_message_row(value)?;
                if seen.insert(row.doc_id.clone()) {
                    rows.push(row);
                }
            }
        }
        return Ok(rows);
    }
    let sessions = cache.sessions.as_mut().expect("bulk coordinate cache");
    if !sessions.contains_key(session_id) {
        let query =
            format!(r#"{{ AgentMessage(filter: {{ {scope} }}) {{ {AGENT_MESSAGE_FIELDS} }} }}"#);
        let response = access
            .query(&query, "validate_canonical_header_coordinate")
            .await?;
        let rows = rows_value(&response, "AgentMessage")?.clone();
        sessions.insert(session_id.clone(), SessionHeaderRows::from(rows));
    }
    sessions[session_id]
        .coordinate_matches(key, sequence)
        .map(|row| decode_transcript_message_row(row).map_err(anyhow::Error::from))
        .collect()
}

fn validate_fork(
    child: &gents_protocol::output::rows::TranscriptMessageRow,
    origin: &gents_protocol::output::rows::TranscriptMessageRow,
) -> Result<()> {
    use gents_protocol::output::origin::{validate_fork_metadata, ObservedMessage};
    validate_fork_metadata(
        ObservedMessage {
            doc_id: &child.doc_id,
            message: &child.message,
        },
        ObservedMessage {
            doc_id: &origin.doc_id,
            message: &origin.message,
        },
    )
    .map_err(anyhow::Error::new)
}

/// How a fork header's origin chain bears on a read.
#[derive(Clone, Copy)]
pub(super) enum OriginWalk {
    /// The content resolves through the origin, so each hop must be visible.
    Required,
    /// The content resolves from destination-owned copies; the chain is
    /// provenance only and ends at the first origin this reader cannot see.
    Provenance,
}

/// Returns the deepest origin reached, validated hop by hop.
pub(super) async fn validate_origin_chain(
    access: ReadAccess<'_, '_>,
    first: &gents_protocol::output::rows::TranscriptMessageRow,
    node_did: &str,
    requester_did: Option<&str>,
    cache: &mut ReadCache,
    walk: OriginWalk,
) -> Result<gents_protocol::output::rows::TranscriptMessageRow> {
    let mut active = BTreeSet::new();
    active.insert(first.doc_id.clone());
    let mut child = first.clone();
    loop {
        let MessagePublication::Fork {
            origin_message_doc_id,
        } = &child.message.publication
        else {
            return Ok(child);
        };
        anyhow::ensure!(
            active.insert(origin_message_doc_id.clone()),
            "canonical fork origin cycle at {origin_message_doc_id}"
        );
        let origin = find_header(
            access,
            origin_message_doc_id,
            node_did,
            requester_did,
            cache,
        )
        .await?;
        let origin = match (origin, walk) {
            (Some(origin), _) => origin,
            (None, OriginWalk::Provenance) => return Ok(child),
            (None, OriginWalk::Required) => {
                return Err(anyhow::Error::new(
                    CanonicalOutputReadError::MissingCanonicalHeader {
                        header_doc_id: origin_message_doc_id.clone(),
                    },
                ))
            }
        };
        validate_fork(&child, &origin)?;
        child = origin;
    }
}
