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

#[derive(Debug, Clone, Deserialize)]
pub(super) struct SessionDocument {
    pub(super) behavior_id: Option<String>,
    #[cfg_attr(not(test), allow(dead_code))]
    pub(super) started: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct ConversationDocument {
    #[serde(default)]
    pub(super) title: String,
    #[serde(default)]
    pub(super) title_source: Option<String>,
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
            created_at: canonical_compaction_created_at(&row.created_at)?,
        })
    }
}

pub(super) fn dedupe_paths(paths: &mut Vec<String>) {
    paths.sort();
    paths.dedup();
}
