//! Exact native invocation-reply pairing for terminal tool accounting.
//!
//! This is the Rust boundary for Lean's `canonicalToolResultBound`: the
//! physical tool document anchors both the delivery publication and its native
//! result block.
use gents_protocol::output::{MessageBlock, MessagePublication, MessageRole, TranscriptMessage};

/// Whether this is the sole canonical invocation reply for an accepted call.
pub(crate) fn is_exact_invocation_reply(
    message: &TranscriptMessage,
    request_doc_id: &str,
    session_id: &str,
    tool_call_doc_id: &str,
    accepted_id: &str,
    accepted_call_id: &Option<String>,
) -> bool {
    message.request_doc_id.as_deref() == Some(request_doc_id)
        && message.session_id == session_id
        && message.role == MessageRole::User
        && matches!(
            &message.publication,
            MessagePublication::ToolDelivery {
                tool_call_doc_id: delivery_doc_id,
            } if delivery_doc_id == tool_call_doc_id
        )
        && matches!(
            message.blocks.as_slice(),
            [MessageBlock::ToolResult {
                tool_call_doc_id: result_doc_id,
                id,
                call_id,
                ..
            }] if result_doc_id == tool_call_doc_id
                && id == accepted_id
                && call_id == accepted_call_id
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents_protocol::output::{OutputOutcome, TranscriptMessage};

    fn reply() -> TranscriptMessage {
        TranscriptMessage {
            message_key: "reply-key".into(),
            session_id: "session".into(),
            agent_did: "did:test:agent".into(),
            requester_did: Some("did:test:requester".into()),
            request_doc_id: Some("request".into()),
            publication: MessagePublication::ToolDelivery {
                tool_call_doc_id: "tool-document".into(),
            },
            outcome: OutputOutcome::Complete,
            sequence: 2,
            role: MessageRole::User,
            native_id: Some("reply-native-id".into()),
            blocks: vec![MessageBlock::ToolResult {
                tool_call_doc_id: "tool-document".into(),
                id: "accepted-id".into(),
                call_id: Some("accepted-call-id".into()),
                parts: Vec::new(),
            }],
            created_at: "2026-09-21T00:00:00Z".into(),
        }
    }

    fn matches_accepted_reply(message: &TranscriptMessage) -> bool {
        is_exact_invocation_reply(
            message,
            "request",
            "session",
            "tool-document",
            "accepted-id",
            &Some("accepted-call-id".into()),
        )
    }

    #[test]
    fn invocation_reply_requires_exact_canonical_pairing() {
        let message = reply();
        assert!(matches_accepted_reply(&message));

        let mut wrong_request = message.clone();
        wrong_request.request_doc_id = Some("other-request".into());
        assert!(!matches_accepted_reply(&wrong_request));

        let mut wrong_session = message.clone();
        wrong_session.session_id = "other-session".into();
        assert!(!matches_accepted_reply(&wrong_session));

        let mut wrong_role = message.clone();
        wrong_role.role = MessageRole::Assistant;
        assert!(!matches_accepted_reply(&wrong_role));

        let mut wrong_delivery = message.clone();
        wrong_delivery.publication = MessagePublication::ToolDelivery {
            tool_call_doc_id: "other-tool-document".into(),
        };
        assert!(!matches_accepted_reply(&wrong_delivery));

        let mut wrong_result_document = message.clone();
        let MessageBlock::ToolResult {
            tool_call_doc_id, ..
        } = &mut wrong_result_document.blocks[0]
        else {
            unreachable!("reply fixture has one tool result");
        };
        *tool_call_doc_id = "other-tool-document".into();
        assert!(!matches_accepted_reply(&wrong_result_document));

        let mut wrong_id = message.clone();
        let MessageBlock::ToolResult { id, .. } = &mut wrong_id.blocks[0] else {
            unreachable!("reply fixture has one tool result");
        };
        *id = "other-id".into();
        assert!(!matches_accepted_reply(&wrong_id));

        let mut wrong_call_id = message.clone();
        let MessageBlock::ToolResult { call_id, .. } = &mut wrong_call_id.blocks[0] else {
            unreachable!("reply fixture has one tool result");
        };
        *call_id = Some("other-call-id".into());
        assert!(!matches_accepted_reply(&wrong_call_id));

        let mut extra_result = message;
        extra_result.blocks.push(MessageBlock::ToolResult {
            tool_call_doc_id: "tool-document".into(),
            id: "accepted-id".into(),
            call_id: Some("accepted-call-id".into()),
            parts: Vec::new(),
        });
        assert!(!matches_accepted_reply(&extra_result));
    }
}
