//! The compaction-summary rendering the loop needs when it resumes a
//! session with prior compaction checkpoints. `gents::prompt` owns full
//! preamble assembly (`LayeredPromptBuilder`, tied to the native `skills`
//! catalog); only this reminder-formatting slice is shared, so it lives
//! here and `gents::prompt::LayeredPromptBuilder::system_reminder` calls it.

use gents_protocol::message::{Message, Text, UserContent};

pub fn system_reminder(text: &str) -> Message {
    Message::User {
        content: vec![UserContent::Text(Text {
            text: format!("<system-reminder>\n{}\n</system-reminder>", text),
        })],
    }
}

pub fn continuation_checkpoint_reminder(checkpoints: &str) -> String {
    format!(
        "Continuation checkpoints from earlier conversation (oldest to newest):\n\n{checkpoints}\n\n\
Continue from these checkpoints and the retained conversation. Treat recorded results as \
evidence, not as a prohibition on verification. Re-check facts when state may have changed, \
the checkpoint is ambiguous, or correctness depends on them. Avoid repeating completed or \
expensive work without a concrete reason."
    )
}

pub fn join_compaction_summaries(compaction_summaries: &[String]) -> String {
    compaction_summaries.join("\n\n---\n\n")
}

/// Render durable compaction summaries exactly as they appear in provider input.
pub fn compaction_summary_message(compaction_summaries: &[String]) -> Option<Message> {
    if compaction_summaries.is_empty() {
        return None;
    }
    let summary_text = join_compaction_summaries(compaction_summaries);
    Some(system_reminder(&continuation_checkpoint_reminder(
        &summary_text,
    )))
}
