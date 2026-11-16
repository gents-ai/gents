use serde::Deserialize;

#[derive(Deserialize)]
pub struct CommandRow {
    #[serde(rename = "_docID")]
    pub doc_id: String,
    pub command_id: String,
    pub requester_did: String,
    pub requester_peer_id: Option<String>,
    pub node_did: String,
    #[serde(default)]
    pub agent_did: Option<String>,
    pub session_id: String,
    #[serde(default)]
    pub session_doc_id: Option<String>,
    pub intent_json: String,
    pub receipt_json: Option<String>,
}

pub const FIELDS: &str = "_docID command_id requester_did requester_peer_id node_did session_id intent_json receipt_json";
