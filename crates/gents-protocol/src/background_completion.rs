pub const BACKGROUND_COMPLETION_WAKE_PROMPT: &str =
    "Review the new background completion results and continue the task if needed.";
const BACKGROUND_COMPLETION_NOTIFICATION_MESSAGE_PREFIX: &str =
    "background-completion-notification:";

pub fn background_completion_notification_message_key(stable_id: &str, kind: &str) -> String {
    format!("{BACKGROUND_COMPLETION_NOTIFICATION_MESSAGE_PREFIX}{stable_id}:{kind}")
}

pub fn is_background_completion_notification_message_key(message_key: &str) -> bool {
    message_key.starts_with(BACKGROUND_COMPLETION_NOTIFICATION_MESSAGE_PREFIX)
}
