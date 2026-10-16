use anyhow::{bail, Result};
use gents_desktop_core::client::{ClientCore, SubmitRequestOptions};
use uuid::Uuid;

use super::super::types::{ChatSendRequest, ChatSendResult, SessionRenameRequest};

pub async fn send_chat_message(
    core: &ClientCore,
    request: ChatSendRequest,
) -> Result<ChatSendResult> {
    let node_did = request.node_did.trim().to_string();
    if node_did.is_empty() {
        bail!("node_did is required");
    }

    let content = match &request.answer {
        Some(answer) => {
            if !request.content.trim().is_empty() {
                bail!("an answer renders its own content; send empty content");
            }
            super::mailbox::question_reply_content(
                core,
                request.caused_by_source_doc_id.as_deref(),
                request.session_id.as_deref(),
                &node_did,
                answer,
            )?
        }
        None => request.content.trim().to_string(),
    };
    if content.is_empty() {
        bail!("content is required");
    }

    let agent_id = request
        .agent_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned);

    let requested_session_id = request
        .session_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let session_id = requested_session_id
        .map(str::to_string)
        .unwrap_or_else(|| Uuid::new_v4().to_string());

    let input = core
        .prepare_user_message_input_with_delivery(
            &node_did,
            &session_id,
            gents_protocol::request_input::RequestInput {
                cwd: chat_folder(request.cwd.as_deref())?,
                ..Default::default()
            },
            request
                .delivery
                .unwrap_or(gents_protocol::request_input::QueueDelivery::Steer),
        )
        .await?;

    let submitted = core
        .submit_request_with_options(
            &session_id,
            &node_did,
            &content,
            agent_id.as_deref(),
            SubmitRequestOptions {
                caused_by_source_doc_id: request.caused_by_source_doc_id,
                input,
                ..SubmitRequestOptions::default()
            },
        )
        .await?;

    Ok(ChatSendResult {
        session_id,
        request_id: submitted.request_id,
        node_did: submitted.node_did,
        agent_id: submitted.agent_id,
    })
}

pub(super) fn chat_folder(raw: Option<&str>) -> Result<Option<String>> {
    let Some(raw) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let path = std::path::Path::new(raw);
    if !path.is_absolute() || !path.is_dir() {
        bail!("The chat folder must be an existing folder on this computer.");
    }
    Ok(Some(raw.to_string()))
}

pub async fn rename_session(core: &ClientCore, request: SessionRenameRequest) -> Result<()> {
    let node_did = request.node_did.trim().to_string();
    if node_did.is_empty() {
        bail!("node_did is required");
    }
    let session_id = request.session_id.trim().to_string();
    if session_id.is_empty() {
        bail!("session_id is required");
    }
    let title = request.title.trim().to_string();
    if title.is_empty() {
        bail!("title is required");
    }
    core.rename_session(&node_did, &session_id, &title).await?;
    Ok(())
}

pub async fn edit_pending_queue(
    core: &ClientCore,
    request: super::super::types::PendingQueueEditRequest,
) -> Result<super::super::types::PendingQueueEditResult> {
    let result = core
        .edit_pending_queue(
            &request.node_did,
            &request.session_id,
            gents::lifecycle::PendingQueueEdit {
                expected_request_doc_ids: request.expected_request_doc_ids,
                selected_request_doc_ids: request.selected_request_doc_ids,
                messages: request
                    .messages
                    .into_iter()
                    .map(|message| gents::lifecycle::PendingMessageEdit {
                        request_doc_id: message.request_doc_id,
                        content: message.content,
                    })
                    .collect(),
            },
        )
        .await?;
    Ok(super::super::types::PendingQueueEditResult {
        request_doc_ids: result.request_doc_ids,
        request_ids: result.request_ids,
    })
}
