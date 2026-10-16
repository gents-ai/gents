use super::*;

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

#[derive(Debug, Clone)]
pub struct SessionOwnerRow {
    pub doc_id: String,
    pub session: gents_protocol::session::AgentSession,
}

pub(crate) fn dedupe_paths(paths: &mut Vec<String>) {
    paths.sort();
    paths.dedup();
}
