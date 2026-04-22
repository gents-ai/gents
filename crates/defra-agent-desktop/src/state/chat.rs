use std::collections::BTreeSet;

use crate::chat::domain::submission::ChatWorkflowState;

#[derive(Debug, Clone, Default)]
pub struct ChatShellState {
    pub selected_peer_id: Option<String>,
    pub selected_agent_did: Option<String>,
    pub selected_session_id: Option<String>,
    pub sidebar_collapsed: bool,
    pub sidebar_width: Option<f32>,
    pub sidebar_drag_origin_width: Option<f32>,
    pub workflow: ChatWorkflowState,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolDetailModalState {
    pub card_id: String,
    pub title: String,
    pub body: String,
    pub language: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct ChatEditorState {
    pub composer_text: String,
    pub composer_expanded: bool,
    pub composer_panel_height: Option<f32>,
    pub composer_drag_origin_height: Option<f32>,
    pub selected_behavior_override: Option<String>,
    pub expanded_tool_cards: BTreeSet<String>,
    pub expanded_reasoning_cards: BTreeSet<String>,
    pub transcript_stick_to_bottom: bool,
    pub last_submission_error: Option<String>,
    pub last_action_message: Option<String>,
    pub last_export_payload: Option<String>,
    pub tool_detail_modal: Option<ToolDetailModalState>,
}

#[derive(Debug, Clone, Default)]
pub struct ChatState {
    pub shell: ChatShellState,
    pub editor: ChatEditorState,
}
