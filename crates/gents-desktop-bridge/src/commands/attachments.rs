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
    if let Some(reason) = core.session_unreadable_reason(node_did) {
        return Err(BridgeError::untyped(reason));
    }
    let attachments = if let Some(message_key) = message_key {
        super::message_attachments(core, session_id, node_did, message_key).await?
    } else {
        let projection = core
            .refresh_local_request(node_did, session_id, Some(request_id), None, &[])
            .await
            .map_err(BridgeError::classify_transport_error)?
            .ok_or_else(|| {
                BridgeError::untyped("Attachment session is unavailable or unreadable")
            })?;
        let mut rows = projection.requests.into_iter().filter(|row| {
            row.request_id == request_id
                && row.session_id.as_deref() == Some(session_id)
                && row.node_did.as_deref() == Some(node_did)
        });
        let row = rows.next().ok_or_else(|| {
            BridgeError::untyped("Attachment request is unavailable or unreadable")
        })?;
        if rows.next().is_some() {
            return Err(BridgeError::untyped(
                "Attachment request identity is ambiguous",
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
    async fn attachment_read_uses_the_authenticated_runtime_and_exact_session() {
        use base64::{engine::general_purpose::STANDARD, Engine};
        let (core, runtime, _home) = crate::tests::support::runtime_core().await;
        let node_did = runtime.node_identity_did().unwrap().to_owned();
        let requester = core.node_identity().did().to_owned();
        let access =
            gents::ConfigAccess::Graphql(core.operator_observation_graphql(&node_did).unwrap());
        let image = RequestAttachment {
            name: "../not-a-file-to-open.png".into(),
            media_type: "image/png".into(),
            data: STANDARD.encode(b"\x89PNG\r\nretained test bytes"),
        };
        let runtime_identity =
            gents::store::identity::RegisteredIdentity::from_registered_did(node_did.clone(), None)
                .unwrap();
        let agent = gents::store::identity::provision_agent_identity(&runtime_identity).unwrap();
        let owner = gents::ConfigAccess::local_as(
            runtime.clone(),
            ::identity::Did::new(&requester).unwrap(),
        );
        let (node_doc, agent_doc) = owner
            .transact_write("attachment_fixture.identities", |txn| {
                Box::pin(async {
                    let node = txn.execute_with_variables(
                        "mutation($input: NodeMutationInputArg!) { create_Node(input: $input) { _docID } }",
                        &serde_json::json!({"input": {"node_did": node_did}}),
                    ).await?;
                    let agent_row = txn.execute_with_variables(
                        "mutation($input: AgentMutationInputArg!) { create_Agent(input: $input) { _docID } }",
                        &serde_json::json!({"input": {"node_did": node_did, "agent_did": agent.agent_did,
                            "creation_nonce": agent.creation_nonce}}),
                    ).await?;
                    Ok::<_, anyhow::Error>((
                        gents::store::graphql::created_doc_id(&node, "Node")?,
                        gents::store::graphql::created_doc_id(&agent_row, "Agent")?,
                    ))
                })
            })
            .await
            .unwrap();
        owner
            .add_resource_relationship("Agent", &agent_doc, "parent", "nodes", &node_doc)
            .await
            .unwrap();
        owner
            .add_actor_relationship(
                "Agent",
                &agent_doc,
                "executor",
                &::identity::Did::new(&agent.agent_did).unwrap(),
            )
            .await
            .unwrap();
        let mut create = gents_protocol::request_admission::AgentRequestCreate::base(
            gents_protocol::request_admission::RequestPurpose::Normal,
            "image-request",
            &node_did,
            &requester,
            &agent.agent_did,
            "image-session",
            "",
            "interactive",
            "2026-10-28T00:00:00Z",
            gents_protocol::request_admission::AgentRequestAdmissionRecord::client(&requester),
        );
        create.input.attachments = vec![image.clone()];
        create.session_doc_id = Some(
            gents::request::lifecycle::reserve_goal_session(&owner, &create)
                .await
                .unwrap(),
        );
        create.agent_doc_id = Some(agent_doc);
        gents::sign_agent_request_create(core.node_identity(), &mut create)
            .await
            .unwrap();
        gents::request::lifecycle::submit_client_request(&access, &create)
            .await
            .unwrap();
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
            vec![image]
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
        core.shutdown().await.unwrap();
        runtime.shutdown().await.unwrap();
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
