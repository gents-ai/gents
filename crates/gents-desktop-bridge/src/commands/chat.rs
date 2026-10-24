use anyhow::{bail, Context, Result};
use gents_desktop_core::client::{ClientCore, SubmitRequestOptions};
use uuid::Uuid;

use super::super::types::{ChatSendRequest, ChatSendResult, SessionRenameRequest};

pub async fn tool_file_changes(
    core: &ClientCore,
    node_did: String,
    session_id: String,
    request_id: String,
    tool_call_doc_id: String,
    retry_id: Option<String>,
) -> Result<Option<gents_protocol::file_changes::ToolFileChanges>> {
    core.tool_file_changes(
        &node_did,
        &session_id,
        &request_id,
        &tool_call_doc_id,
        retry_id.as_deref(),
    )
    .await
}

pub async fn request_output(
    core: &ClientCore,
    node_did: String,
    session_id: String,
    request_id: String,
) -> Result<crate::types::RequestOutputView> {
    let output = request_output_state(
        core.request_output(&node_did, &session_id, &request_id)
            .await?,
    );
    Ok(crate::types::RequestOutputView {
        node_did,
        session_id,
        request_id,
        output,
    })
}

fn request_output_state(
    output: Option<gents::session::CanonicalRequestOutput>,
) -> crate::types::RequestOutputStateView {
    use crate::types::RequestOutputStateView as View;
    use gents::session::CanonicalRequestOutput as Output;
    match output {
        Some(Output::TerminalMessage { presentation, .. }) => View::TerminalMessage { content: presentation.body_markdown },
        Some(Output::TerminalNoMessage) => View::TerminalNoMessage,
        Some(Output::Denied) => View::Unavailable { reason: "Access to this request's canonical answer was denied.".into() },
        Some(Output::RetainedPartial(_)) => View::Unavailable { reason: "This turn ended without a complete reply. Read its transcript before starting another voice turn.".into() },
        Some(Output::Conflicted | Output::Invalid | Output::Retracted) => View::Unavailable { reason: "This request's canonical answer cannot be presented. Check its transcript and runtime status.".into() },
        None | Some(Output::Absent | Output::Live(_) | Output::Settling(_) | Output::Loading | Output::Published { .. }) => View::Loading,
    }
}

pub async fn send_chat_message(
    core: &ClientCore,
    request: ChatSendRequest,
) -> Result<ChatSendResult> {
    let node_did = request.node_did.trim().to_string();
    if node_did.is_empty() {
        bail!("node_did is required");
    }
    let caller_intent = super::chat_submission::intent(core, &request)?;
    if let Some(intent) = caller_intent.as_ref() {
        match core.recover_caller_submission(intent).await {
            Ok(None) => {}
            recovered => {
                return super::chat_submission::sent(
                    core,
                    intent,
                    recovered.and_then(|receipt| {
                        receipt.context("retained submission reply omitted its result")
                    }),
                )
                .await;
            }
        }
    }
    if let Some(request_id) = super::chat_resolve::resolve_target(&request) {
        return super::chat_resolve::resolve_chat_submission(core, &node_did, &request, request_id)
            .await;
    }

    let captured_context = request.captured_context.unwrap_or_default();
    let attachments = request.attachments.unwrap_or_default();
    let content = match &request.answer {
        Some(answer) => {
            if !request.content.trim().is_empty()
                || !captured_context.is_empty()
                || !attachments.is_empty()
            {
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
    if content.is_empty() && captured_context.is_empty() && attachments.is_empty() {
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
                captured_context,
                attachments,
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
                caller_intent: caller_intent.clone(),
                caused_by_source_doc_id: request.caused_by_source_doc_id,
                input,
                ..SubmitRequestOptions::default()
            },
        )
        .await;

    match caller_intent.as_ref() {
        Some(intent) => super::chat_submission::sent(core, intent, submitted).await,
        None => super::chat_resolve::sent(submitted),
    }
}

pub async fn retry_request(
    core: &ClientCore,
    request_id: &str,
    node_did: Option<&str>,
) -> Result<ChatSendResult> {
    let parent = core.request_in_scope(request_id, node_did)?;
    Ok(core.retry_request(&parent).await?.into())
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

pub async fn fork_session_before_message(
    core: &ClientCore,
    request: crate::types::SessionForkBeforeRequest,
) -> Result<crate::types::SessionForkResult> {
    let outcome = core
        .fork_session_before_message(&request.node_did, &request.session_id, &request.message_key)
        .await?;
    Ok(crate::types::SessionForkResult {
        node_did: request.node_did.trim().to_owned(),
        session_id: outcome.session_id,
    })
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

pub async fn steer_pending_message(
    core: &ClientCore,
    request: super::super::types::PendingMessageSteerRequest,
) -> Result<super::super::types::PendingQueueEditResult> {
    let result = core
        .steer_pending_message(
            &request.node_did,
            &request.session_id,
            request.expected_request_doc_ids,
            &request.request_doc_id,
        )
        .await?;
    Ok(super::super::types::PendingQueueEditResult {
        request_doc_ids: result.request_doc_ids,
        request_ids: result.request_ids,
    })
}

#[cfg(test)]
mod tests {
    use super::request_output_state;

    #[test]
    fn terminal_diagnostic_history_cannot_keep_voice_waiting_for_a_complete_answer() {
        let state = request_output_state(Some(
            gents::session::CanonicalRequestOutput::RetainedPartial(Vec::new()),
        ));
        assert!(matches!(
            state,
            crate::types::RequestOutputStateView::Unavailable { .. }
        ));
        assert!(matches!(
            request_output_state(None),
            crate::types::RequestOutputStateView::Loading
        ));
    }
}
