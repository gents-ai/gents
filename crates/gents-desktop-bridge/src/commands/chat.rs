use anyhow::{bail, Result};
use gents_desktop_core::client::{ClientCore, SubmitRequestOptions};
use uuid::Uuid;

use super::super::types::{
    turn_state_label, ChatSendRequest, ChatSendResult, SessionRenameRequest,
};

pub async fn send_chat_message(
    core: &ClientCore,
    request: ChatSendRequest,
) -> Result<ChatSendResult> {
    let agent_did = request.agent_did.trim().to_string();
    if agent_did.is_empty() {
        bail!("agent_did is required");
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
                &agent_did,
                answer,
            )?
        }
        None => request.content.trim().to_string(),
    };
    if content.is_empty() {
        bail!("content is required");
    }

    let behavior_id = request
        .behavior_id
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

    let store = core.store().snapshot();
    let mut input = gents_protocol::request_input::RequestInput {
        cwd: chat_folder(request.cwd.as_deref())?,
        ..Default::default()
    };
    if let Some(turn_state) = store.derive_turn_for_agent(&session_id, &agent_did) {
        if !turn_state.is_terminal() {
            // An answer never waits for the asking turn: like a queued user
            // turn, it is ordered behind the active request, which the
            // runtime claims first; the reply claim then consumes the item.
            let active = store
                .latest_request_id_for_session_for_agent(&session_id, &agent_did)
                .filter(|_| request.answer.is_some());
            let Some(active) = active else {
                bail!(
                    "cannot send while current turn is {}",
                    turn_state_label(turn_state)
                );
            };
            input.queue = Some(queued_user_turn(active));
        }
    }

    let submitted = core
        .submit_request_with_options(
            &session_id,
            &agent_did,
            &content,
            behavior_id.as_deref(),
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
        agent_did: submitted.agent_did,
        behavior_id: submitted.behavior_id,
    })
}

/// The chat folder as a request cwd: absent when blank, else an existing
/// absolute directory. The runtime still holds it under the agent's tool
/// root ceiling at admission.
fn chat_folder(raw: Option<&str>) -> Result<Option<String>> {
    let Some(raw) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    let path = std::path::Path::new(raw);
    if !path.is_absolute() || !path.is_dir() {
        bail!("The chat folder must be an existing folder on this computer.");
    }
    Ok(Some(raw.to_string()))
}

fn queued_user_turn(active_request_id: String) -> gents_protocol::request_input::RequestQueue {
    use gents_protocol::request_input::{QueuePolicy, QueueSource, RequestQueue};
    RequestQueue {
        source: QueueSource::User,
        policy: QueuePolicy::Append,
        key: None,
        queued_after_request_id: Some(active_request_id),
        interrupted_request_id: None,
        background_completion_wake_version: None,
    }
}

pub async fn rename_session(core: &ClientCore, request: SessionRenameRequest) -> Result<()> {
    let agent_did = request.agent_did.trim().to_string();
    if agent_did.is_empty() {
        bail!("agent_did is required");
    }
    let session_id = request.session_id.trim().to_string();
    if session_id.is_empty() {
        bail!("session_id is required");
    }
    let title = request.title.trim().to_string();
    if title.is_empty() {
        bail!("title is required");
    }
    core.rename_session(&agent_did, &session_id, &title).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::chat_folder;

    #[test]
    fn blank_folder_is_absent() {
        assert_eq!(chat_folder(None).unwrap(), None);
        assert_eq!(chat_folder(Some("  ")).unwrap(), None);
    }

    #[test]
    fn existing_absolute_folder_is_kept_trimmed() {
        let dir = tempfile::tempdir().unwrap();
        let raw = dir.path().to_str().unwrap();
        assert_eq!(
            chat_folder(Some(&format!(" {raw} "))).unwrap().as_deref(),
            Some(raw)
        );
    }

    #[test]
    fn relative_missing_or_file_folder_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("f.txt");
        std::fs::write(&file, "x").unwrap();
        for bad in [
            "relative/dir".to_string(),
            dir.path().join("missing").display().to_string(),
            file.display().to_string(),
        ] {
            assert!(chat_folder(Some(&bad)).is_err(), "{bad}");
        }
    }
}
