use super::*;

#[derive(Deserialize)]
pub(super) struct AgentMessageRow {
    pub(super) sequence: u32,
    pub(super) role: String,
    pub(super) content: String,
    #[serde(default)]
    pub(super) request_id: Option<String>,
    #[serde(default)]
    pub(super) message_key: String,
}

#[derive(Deserialize)]
pub(super) struct CompactionEntryRow {
    pub(super) session_id: String,
    pub(super) sequence: u32,
    pub(super) summary: String,
    pub(super) files_read: String,
    pub(super) files_modified: String,
    pub(super) messages_compacted: u32,
    #[serde(default)]
    pub(super) compacted_through_sequence: Option<u32>,
    pub(super) original_tokens: usize,
    pub(super) compacted_tokens: usize,
    pub(super) created_at: String,
}

/// Reader envelope for the single durable session document: the canonical
/// `AgentSession` plus the physical `_docID` the owner uses to address the
/// exact row in a patch transaction. This is not a second writable
/// representation; writers patch through `AgentSession` fields only.
#[derive(Debug, Clone)]
pub struct SessionOwnerRow {
    pub doc_id: String,
    pub session: gents_protocol::session::AgentSession,
}

impl TryFrom<CompactionEntryRow> for CompactionEntry {
    type Error = anyhow::Error;

    fn try_from(row: CompactionEntryRow) -> Result<Self> {
        Ok(Self {
            session_id: row.session_id,
            sequence: row.sequence,
            summary: row.summary,
            files_read: serde_json::from_str(&row.files_read)?,
            files_modified: serde_json::from_str(&row.files_modified)?,
            messages_compacted: row.messages_compacted,
            compacted_through_sequence: row.compacted_through_sequence,
            original_tokens: row.original_tokens,
            compacted_tokens: row.compacted_tokens,
            created_at: row.created_at,
        })
    }
}

pub(crate) fn dedupe_paths(paths: &mut Vec<String>) {
    paths.sort();
    paths.dedup();
}
