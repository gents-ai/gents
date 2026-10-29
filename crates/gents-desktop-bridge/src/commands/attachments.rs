use gents_desktop_core::client::ClientCore;
use gents_protocol::request_input::RequestAttachment;

use crate::error::BridgeError;

/// Attachment names are display metadata. Reads use the authorized retained
/// message or request, never a host filesystem path supplied by a client.
pub async fn request_attachments(
    core: &ClientCore,
    node_did: &str,
    session_id: &str,
    request_id: &str,
    message_key: Option<&str>,
    index: Option<u32>,
) -> Result<Vec<RequestAttachment>, BridgeError> {
    let session_is_cached = core
        .store()
        .snapshot()
        .sessions
        .iter()
        .any(|row| row.session_id == session_id && row.node_did == node_did);
    if !session_is_cached {
        return Err(BridgeError::untyped(
            "Attachment session is unavailable or unreadable",
        ));
    }
    if let Some(reason) = core.session_unreadable_reason(session_id, node_did) {
        return Err(BridgeError::untyped(reason));
    }
    let attachments = if let Some(message_key) = message_key {
        super::message_attachments(core, session_id, node_did, message_key).await?
    } else {
        let row = core
            .request_in_scope(request_id, Some(node_did))
            .map_err(|e| BridgeError::untyped(e.to_string()))?;
        if row.session_id.as_deref() != Some(session_id) {
            return Err(BridgeError::untyped(
                "Attachment request belongs to another session",
            ));
        }
        row.input.unwrap_or_default().attachments
    };
    select_attachments(attachments, index)
}

/// Native requests can hold more encoded bytes than a client-host frame. A
/// reader can select one attachment without terminating its shared connection.
fn select_attachments(
    mut attachments: Vec<RequestAttachment>,
    index: Option<u32>,
) -> Result<Vec<RequestAttachment>, BridgeError> {
    if let Some(index) = index {
        let index = index as usize;
        if index >= attachments.len() {
            return Err(BridgeError::untyped(
                "This attachment is not present in the retained message",
            ));
        }
        attachments = vec![attachments.remove(index)];
    }
    Ok(attachments)
}

pub(crate) fn bounded_attachment_response(
    attachments: Vec<RequestAttachment>,
) -> Result<Vec<RequestAttachment>, BridgeError> {
    let size = serde_json::to_vec(&attachments)
        .map_err(|e| BridgeError::untyped(e.to_string()))?
        .len();
    if size > crate::host::protocol::MAX_FRAME_BYTES - 64 * 1024 {
        return Err(BridgeError::untyped(
            "Read attachments individually using their index; this message exceeds one client frame",
        ));
    }
    Ok(attachments)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn attachment_read_preserves_request_scope_and_does_not_reopen_a_filename() {
        use base64::{engine::general_purpose::STANDARD, Engine};
        let (core, home) = crate::tests::support::boot_core().await;
        let node_did = "did:test:attachment-runtime".to_owned();
        let requester_did = core.node_identity().did().to_owned();
        core.add_managed_enrollment_peer_for_test(
            &node_did,
            "http://127.0.0.1:1/api/v0/graphql",
            home.path().join("runtime").to_str().unwrap(),
            1,
        )
        .await
        .unwrap();
        let image = RequestAttachment {
            name: "../not-a-file-to-open.png".into(),
            media_type: "image/png".into(),
            data: STANDARD.encode(b"\x89PNG\r\n\x1a\nretained test bytes"),
        };
        let document = serde_json::json!({
            "purpose": "normal", "request_id": "image-request", "node_did": node_did,
            "requester_did": requester_did, "agent_id": "test-agent", "session_id": "image-session",
            "content": "", "lifecycle_state": "pending", "created_at": "2026-10-28T00:00:00Z",
            "input": {"attachments": [image]},
        });
        let input = gents_protocol::graphql::graphql_input_literal(&document).unwrap();
        let result = async {
            let session = gents_protocol::graphql::graphql_input_literal(&serde_json::json!({
                "session_id": "image-session", "node_did": node_did, "requester_did": requester_did,
                "agent_id": "test-agent", "created_at": "2026-10-28T00:00:00Z",
            }))
            .unwrap();
            gents::config_client::ConfigAccess::local(core.node_arc())
                .write(
                    "test.image_session",
                    &format!("mutation {{ create_AgentSession(input: {session}) {{ _docID }} }}"),
                )
                .await
                .unwrap();
            gents::config_client::ConfigAccess::local(core.node_arc())
                .write(
                    "test.image_attachment",
                    &format!("mutation {{ create_AgentRequest(input: {input}) {{ _docID }} }}"),
                )
                .await
                .unwrap();
            core.refresh_store().await.unwrap();
            assert_eq!(
                request_attachments(
                    &core,
                    &node_did,
                    "image-session",
                    "image-request",
                    None,
                    Some(0)
                )
                .await
                .unwrap(),
                vec![image.clone()]
            );
            assert!(request_attachments(
                &core,
                &node_did,
                "different-session",
                "image-request",
                None,
                Some(0)
            )
            .await
            .is_err());
            assert!(request_attachments(
                &core,
                "did:fixture:other",
                "image-session",
                "image-request",
                None,
                Some(0)
            )
            .await
            .is_err());

            let access = gents::config_client::ConfigAccess::local(core.node_arc());
            let orphan_request =
                gents_protocol::graphql::graphql_input_literal(&serde_json::json!({
                    "purpose": "normal", "request_id": "orphan-image-request",
                    "node_did": node_did, "requester_did": requester_did,
                    "agent_id": "test-agent", "session_id": "missing-image-session",
                    "content": "", "lifecycle_state": "pending",
                    "created_at": "2026-10-28T00:00:00Z",
                    "input": {"attachments": [image]},
                }))
                .unwrap();
            access
                .write(
                    "test.orphan_image_request",
                    &format!(
                        "mutation {{ create_AgentRequest(input: {orphan_request}) {{ _docID }} }}"
                    ),
                )
                .await
                .unwrap();
            core.refresh_store().await.unwrap();
            assert!(
                request_attachments(
                    &core,
                    &node_did,
                    "missing-image-session",
                    "orphan-image-request",
                    None,
                    Some(0)
                )
                .await
                .is_err(),
                "request attachments must not be readable without an in-scope session"
            );

            let foreign_session =
                gents_protocol::graphql::graphql_input_literal(&serde_json::json!({
                    "session_id": "foreign-image-session", "node_did": node_did,
                    "requester_did": "did:fixture:foreign", "agent_id": "test-agent",
                    "created_at": "2026-10-28T00:00:00Z",
                }))
                .unwrap();
            let foreign_request =
                gents_protocol::graphql::graphql_input_literal(&serde_json::json!({
                    "purpose": "normal", "request_id": "foreign-image-request",
                    "node_did": node_did, "requester_did": "did:fixture:foreign",
                    "agent_id": "test-agent", "session_id": "foreign-image-session",
                    "content": "", "lifecycle_state": "pending",
                    "created_at": "2026-10-28T00:00:00Z",
                    "input": {"attachments": [image]},
                }))
                .unwrap();
            access
                .write(
                    "test.foreign_image_session",
                    &format!(
                        "mutation {{ create_AgentSession(input: {foreign_session}) {{ _docID }} }}"
                    ),
                )
                .await
                .unwrap();
            access
                .write(
                    "test.foreign_image_request",
                    &format!(
                        "mutation {{ create_AgentRequest(input: {foreign_request}) {{ _docID }} }}"
                    ),
                )
                .await
                .unwrap();
            core.refresh_store().await.unwrap();
            assert!(
                request_attachments(
                    &core,
                    &node_did,
                    "foreign-image-session",
                    "foreign-image-request",
                    None,
                    Some(0)
                )
                .await
                .is_err(),
                "request attachment reads must respect session requester scope"
            );
        }
        .await;
        core.shutdown().await.unwrap();
        result
    }

    #[test]
    fn attachment_reads_bound_frames_and_preserve_selected_frozen_bytes() {
        let file = |name: &str, data: String| RequestAttachment {
            name: name.into(),
            media_type: "image/png".into(),
            data,
        };
        let files = vec![
            file("one.png", "a".repeat(5_592_404)),
            file("two.png", "b".repeat(5_592_404)),
        ];
        assert!(bounded_attachment_response(files.clone()).is_err());
        for index in [0, 1] {
            assert_eq!(
                bounded_attachment_response(
                    select_attachments(files.clone(), Some(index)).unwrap()
                )
                .unwrap(),
                vec![files[index as usize].clone()]
            );
        }
        assert!(select_attachments(files, Some(2)).is_err());
        assert_eq!(select_attachments(vec![], None).unwrap(), vec![]);
    }
}
