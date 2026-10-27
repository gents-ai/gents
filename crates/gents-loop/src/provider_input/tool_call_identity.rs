use super::ProviderInputProfile;
use rig::message::{AssistantContent, Message, UserContent};

pub(crate) fn normalize_tool_call_ids(profile: ProviderInputProfile, message: &mut Message) {
    if !matches!(
        profile,
        ProviderInputProfile::OpenAiResponsesNormalized
            | ProviderInputProfile::ChatGptCodexResponses
            | ProviderInputProfile::XaiResponses
    ) {
        return;
    }
    match message {
        Message::Assistant { content, .. } => {
            for item in content.iter_mut() {
                if let AssistantContent::ToolCall(call) = item {
                    call.call_id.get_or_insert_with(|| call.id.clone());
                }
            }
        }
        Message::User { content } => {
            for item in content.iter_mut() {
                if let UserContent::ToolResult(result) = item {
                    result.call_id.get_or_insert_with(|| result.id.clone());
                }
            }
        }
        Message::System { .. } => {}
    }
}
