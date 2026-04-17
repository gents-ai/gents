#[derive(Debug, Clone, Default)]
pub struct PeersState {
    pub selected_peer_id: Option<String>,
    pub show_add_form: bool,
    pub add_label: String,
    pub add_addr: String,
    pub add_agent_did: String,
    pub last_action_message: Option<String>,
}
