use std::collections::BTreeSet;

use anyhow::{bail, Context, Result};
use defra_node::EmbeddedNode;
use gents::NodeIdentity;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_protocol::row::AgentRequestRow;
use gents_protocol::session_hydration::{
    decode_manifest_json, SessionHydrationCollection, SessionHydrationDocumentKey,
    SessionHydrationReceipt, SESSION_HYDRATION_RECEIPT_VERSION,
};
use serde::Deserialize;

use super::LocalHydrationStartEvidence;

#[cfg(test)]
mod progress_tests;
#[cfg(test)]
mod tests;

#[derive(Deserialize)]
struct HydrationDocIdRow {
    #[serde(rename = "_docID")]
    doc_id: Option<String>,
}

pub(super) async fn load_local_hydration_start_evidence(
    node: &EmbeddedNode,
    requester_did: &str,
    session_id: &str,
    node_did: &str,
) -> Result<LocalHydrationStartEvidence> {
    let requester_did = gents::store::graphql::escape_graphql_string(requester_did);
    let session_id = gents::store::graphql::escape_graphql_string(session_id);
    let node_did = gents::store::graphql::escape_graphql_string(node_did);
    let scope = format!(
        "requester_did: {{ _eq: \"{requester_did}\" }}, node_did: {{ _eq: \"{node_did}\" }}, session_id: {{ _eq: \"{session_id}\" }}"
    );
    let response = gents::store::graphql::graphql_with_transaction_retry(
        node,
        &format!(
            r#"{{
                AgentSession(filter: {{ {scope} }}, limit: 1) {{ _docID }}
                AgentRequest(filter: {{ {scope} }}) {{ request_id lifecycle_state }}
            }}"#
        ),
        "query local hydration start evidence",
    )
    .await?;
    local_hydration_start_evidence_from_response(&response)
}

fn local_hydration_start_evidence_from_response(
    response: &defra_node::QueryResponse,
) -> Result<LocalHydrationStartEvidence> {
    let owned_session_present =
        !gents::store::graphql::rows::<HydrationDocIdRow>(response, "AgentSession")?.is_empty();
    let nonterminal_request_present =
        gents::store::graphql::rows::<AgentRequestRow>(response, "AgentRequest")?
            .iter()
            .any(|row| {
                !row.lifecycle_state
                    .is_some_and(RequestLifecycleState::is_terminal)
            });
    Ok(LocalHydrationStartEvidence {
        owned_session_present,
        nonterminal_request_present,
    })
}

pub(super) async fn load_local_hydration_documents(
    node: &EmbeddedNode,
    requester_did: &str,
    session_id: &str,
    node_did: &str,
    verified_manifest: Option<&BTreeSet<SessionHydrationDocumentKey>>,
) -> Result<BTreeSet<SessionHydrationDocumentKey>> {
    let query = local_hydration_query(requester_did, session_id, node_did, verified_manifest);
    let response = gents::store::graphql::graphql_with_transaction_retry(
        node,
        &query,
        "query local session hydration documents",
    )
    .await?;
    local_hydration_documents_from_response(&response)
}

const HYDRATION_COLLECTIONS: [(&str, SessionHydrationCollection); 5] = [
    ("AgentRequest", SessionHydrationCollection::AgentRequest),
    ("AgentMessage", SessionHydrationCollection::AgentMessage),
    (
        "AgentOutputSegment",
        SessionHydrationCollection::AgentOutputSegment,
    ),
    ("AgentToolCall", SessionHydrationCollection::AgentToolCall),
    (
        "CompactionEntry",
        SessionHydrationCollection::CompactionEntry,
    ),
];

/// Fork origin dependencies retain their source session. Only the verified
/// receipt's physical keys extend the session read; ownership filters still apply.
fn local_hydration_query(
    requester_did: &str,
    session_id: &str,
    node_did: &str,
    verified_manifest: Option<&BTreeSet<SessionHydrationDocumentKey>>,
) -> String {
    let requester_did = gents::store::graphql::escape_graphql_string(requester_did);
    let session_id = gents::store::graphql::escape_graphql_string(session_id);
    let node_did = gents::store::graphql::escape_graphql_string(node_did);
    let fields = HYDRATION_COLLECTIONS
        .into_iter()
        .map(|(name, collection)| {
            let ids = verified_manifest
                .into_iter()
                .flatten()
                .filter(|key| key.collection == collection)
                .map(|key| format!("\"{}\"", gents::store::graphql::escape_graphql_string(&key.doc_id)))
                .collect::<Vec<_>>()
                .join(",");
            let session_filter = format!("session_id: {{ _eq: \"{session_id}\" }}");
            let selected = if ids.is_empty() {
                session_filter
            } else {
                format!("_or: [{{{session_filter}}}, {{_docID: {{_in: [{ids}]}}}}]")
            };
            format!(
                "{name}(filter: {{requester_did: {{_eq: \"{requester_did}\"}}, node_did: {{_eq: \"{node_did}\"}}, {selected}}}) {{_docID}}"
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    format!("{{{fields}}}")
}

fn local_hydration_documents_from_response(
    response: &defra_node::QueryResponse,
) -> Result<BTreeSet<SessionHydrationDocumentKey>> {
    let mut ids = BTreeSet::new();
    for (collection_name, collection) in HYDRATION_COLLECTIONS {
        for row in gents::store::graphql::rows::<HydrationDocIdRow>(response, collection_name)? {
            if let Some(doc_id) = row.doc_id.filter(|value| !value.is_empty()) {
                ids.insert(SessionHydrationDocumentKey { collection, doc_id });
            }
        }
    }
    Ok(ids)
}

pub(super) async fn load_hydration_server_state(
    node: &EmbeddedNode,
    peer_id: &str,
    requester_did: &str,
    session_id: &str,
    node_did: &str,
    node_identity: &crate::client::principal_identity::PrincipalIdentity,
) -> Result<(
    gents::agent::p2p_reconcile::session_hydration::ClientHydrationRequestState,
    Option<String>,
)> {
    use gents::agent::p2p_reconcile::session_hydration::ClientHydrationRequestState;
    let expected_request_key = format!("{peer_id}:{session_id}");
    let request_key = gents::store::graphql::escape_graphql_string(&expected_request_key);
    let query = format!(
        r#"{{ SessionHydrationRequest(filter: {{ request_key: {{ _eq: "{request_key}" }} }}) {{
            request_key requester_did node_did session_id status status_detail
            served_doc_count served_manifest_json processed_at
            outcome_signer_did outcome_signature
        }} }}"#
    );
    let response = gents::store::graphql::graphql_with_transaction_retry(
        node,
        &query,
        "query session hydration request",
    )
    .await?;
    let Some(data) = response.data else {
        return Ok((ClientHydrationRequestState::Missing, None));
    };
    let rows = data
        .get("SessionHydrationRequest")
        .and_then(|value| value.as_array())
        .cloned()
        .unwrap_or_default();
    if rows.len() > 1 {
        bail!("session hydration request key resolved to multiple rows");
    }
    let Some(row) = rows.first() else {
        return Ok((ClientHydrationRequestState::Missing, None));
    };
    for (field, expected) in [
        ("request_key", expected_request_key.as_str()),
        ("requester_did", requester_did),
        ("node_did", node_did),
        ("session_id", session_id),
    ] {
        if row.get(field).and_then(|value| value.as_str()) != Some(expected) {
            bail!("session hydration request {field} does not match the selected target");
        }
    }
    let status = row
        .get("status")
        .and_then(|value| value.as_str())
        .unwrap_or_default();
    if status == "pending" {
        return Ok((ClientHydrationRequestState::Pending, None));
    }
    if !matches!(status, "served" | "rejected") {
        bail!("unknown session hydration request status {status:?}");
    }
    let served_count = row
        .get("served_doc_count")
        .and_then(|value| value.as_i64())
        .map(usize::try_from)
        .transpose()
        .context("session hydration served_doc_count must be non-negative")?;
    let manifest = decode_manifest_json(
        row.get("served_manifest_json")
            .and_then(|value| value.as_str())
            .context("terminal hydration request is missing served_manifest_json")?,
    )?;
    if served_count != Some(manifest.len()) {
        bail!("session hydration served_doc_count does not match signed manifest");
    }
    let signature = bs58::decode(
        row.get("outcome_signature")
            .and_then(|value| value.as_str())
            .context("terminal hydration request is missing outcome_signature")?,
    )
    .into_vec()
    .context("decode session hydration receipt signature")?;
    let receipt = SessionHydrationReceipt {
        version: SESSION_HYDRATION_RECEIPT_VERSION,
        request_key: expected_request_key,
        requester_did: requester_did.to_string(),
        node_did: node_did.to_string(),
        session_id: session_id.to_string(),
        status: status.to_string(),
        status_detail: row
            .get("status_detail")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .to_string(),
        served_manifest: manifest,
        processed_at: row
            .get("processed_at")
            .and_then(|value| value.as_str())
            .context("terminal hydration request is missing processed_at")?
            .to_string(),
        signer_did: row
            .get("outcome_signer_did")
            .and_then(|value| value.as_str())
            .context("terminal hydration request is missing outcome_signer_did")?
            .to_string(),
        signature,
    };
    receipt.validate_shape()?;
    if !node_identity
        .verify(node_did, &receipt.signing_payload()?, &receipt.signature)
        .await?
    {
        bail!("session hydration receipt signature is invalid");
    }
    let documents = receipt.served_manifest.into_iter().collect::<BTreeSet<_>>();
    match status {
        "served" => Ok((ClientHydrationRequestState::Served(documents), None)),
        "rejected" => {
            let detail = Some(receipt.status_detail).filter(|detail| !detail.is_empty());
            Ok((
                ClientHydrationRequestState::Rejected(Some(documents)),
                detail,
            ))
        }
        _ => unreachable!(),
    }
}
