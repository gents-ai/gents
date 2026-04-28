#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerMutationResult {
    pub peer_id: String,
    pub label: String,
    pub addr: String,
    pub connected: bool,
    pub warning: Option<String>,
}
