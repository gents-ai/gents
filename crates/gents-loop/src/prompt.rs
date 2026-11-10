use gents_protocol::message::{Message, Text, UserContent};

const REMINDER_PREFIX: &str = "<system-reminder>\n";
const REMINDER_SUFFIX: &str = "\n</system-reminder>";

pub fn system_reminder(text: &str) -> Message {
    Message::User {
        content: vec![UserContent::Text(Text {
            text: format!("{REMINDER_PREFIX}{text}{REMINDER_SUFFIX}"),
        })],
    }
}

/// Recognizes the wire envelope for display classification, not authorship or authorization.
pub fn is_system_reminder(text: &str) -> bool {
    text.strip_prefix(REMINDER_PREFIX)
        .is_some_and(|body| body.ends_with(REMINDER_SUFFIX))
}

const CHECKPOINT_REMINDER_HEAD: &str = "Continuation checkpoints from earlier conversation";

/// Whether a message text is the compaction summary a reduction leaves in the history.
pub fn is_compaction_summary(text: &str) -> bool {
    is_system_reminder(text)
        && text
            .strip_prefix(REMINDER_PREFIX)
            .is_some_and(|body| body.starts_with(CHECKPOINT_REMINDER_HEAD))
}

pub fn continuation_checkpoint_reminder(checkpoints: &str) -> String {
    format!(
        "{CHECKPOINT_REMINDER_HEAD} (oldest to newest):\n\n{checkpoints}\n\n\
Continue from these checkpoints and the retained conversation. Treat recorded results as \
evidence, not as a prohibition on verification. Re-check facts when state may have changed, \
the checkpoint is ambiguous, or correctness depends on them. Avoid repeating completed or \
expensive work without a concrete reason."
    )
}

pub fn join_compaction_summaries(compaction_summaries: &[String]) -> String {
    compaction_summaries.join("\n\n---\n\n")
}

pub fn compaction_summary_message(compaction_summaries: &[String]) -> Option<Message> {
    if compaction_summaries.is_empty() {
        return None;
    }
    let summary_text = join_compaction_summaries(compaction_summaries);
    Some(system_reminder(&continuation_checkpoint_reminder(
        &summary_text,
    )))
}
