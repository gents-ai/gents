use super::*;

#[derive(Deserialize)]
pub(super) struct AgentMessageRow {
    pub(super) role: String,
    pub(super) content: String,
}

#[derive(Deserialize)]
pub(super) struct CompactionEntryRow {
    pub(super) session_id: String,
    pub(super) sequence: u32,
    pub(super) summary: String,
    pub(super) files_read: String,
    pub(super) files_modified: String,
    pub(super) messages_compacted: u32,
    pub(super) original_tokens: usize,
    pub(super) compacted_tokens: usize,
    pub(super) created_at: String,
}

#[derive(Deserialize)]
pub(super) struct ToolCallDocument {
    #[serde(rename = "_docID")]
    pub(super) doc_id: String,
    pub(super) tool_name: String,
    pub(super) args: String,
    pub(super) started_at: String,
    #[serde(default)]
    pub(super) completed_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct SessionDocument {
    #[serde(rename = "_docID")]
    pub(super) doc_id: String,
    pub(super) behavior_id: Option<String>,
    pub(super) started: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct ConversationDocument {
    #[serde(rename = "_docID")]
    #[allow(dead_code)]
    #[serde(default)]
    pub(super) doc_id: String,
    #[serde(default)]
    pub(super) title: String,
    #[serde(default)]
    pub(super) title_source: Option<String>,
    #[serde(default)]
    pub(super) preview_text: String,
    #[serde(default)]
    pub(super) status: String,
    #[serde(default)]
    pub(super) latest_request_id: String,
    #[serde(default)]
    pub(super) behavior_id: Option<String>,
    #[serde(default)]
    pub(super) created_at: String,
    #[serde(default)]
    pub(super) agent_did: Option<String>,
    #[serde(default)]
    pub(super) agent_name: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub(super) forked_from_session_id: Option<String>,
    #[serde(default)]
    #[allow(dead_code)]
    pub(super) fork_at_user_turn: Option<i64>,
    #[serde(default)]
    #[allow(dead_code)]
    pub(super) forked_at: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct ToolCallResultRow {
    pub(super) result: String,
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
            original_tokens: row.original_tokens,
            compacted_tokens: row.compacted_tokens,
            created_at: row.created_at,
        })
    }
}

pub(super) fn dedupe_paths(paths: &mut Vec<String>) {
    paths.sort();
    paths.dedup();
}
