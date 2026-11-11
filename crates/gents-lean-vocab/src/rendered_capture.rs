use serde::Deserialize;

#[derive(Debug, Deserialize, Clone)]
pub struct LeanRenderedCaptureCase {
    pub name: String,
    pub node_did: u64,
    pub session_id: u64,
    pub request_id: u64,
    pub turn_index: usize,
    pub attempt: u32,
    pub request: u64,
    pub prior_binding: Option<u64>,
    pub capture_outcome: String,
    pub capture_durable: bool,
    pub post_stage: String,
    pub send_permitted: bool,
    pub provider_requests_observed: usize,
    pub durable_after: Option<u64>,
    pub final_stage: String,
}

#[derive(Debug, Deserialize, Clone)]
pub struct LeanRenderedCaptureStorageCase {
    pub name: String,
    pub encoding: String,
    pub request: u64,
    pub base_witness: Option<u64>,
    pub base_depth: usize,
    pub max_depth: usize,
    pub base_verified: bool,
    pub manifest_blocks: Vec<LeanRenderedCaptureManifestBlock>,
    pub manifest_root: Option<LeanRenderedCaptureManifestBlock>,
    pub decoded_request: Option<u64>,
    pub send_permitted: bool,
}

#[derive(Debug, Deserialize, Clone)]
pub struct LeanRenderedCaptureManifestBlock {
    pub r#ref: u64,
    pub pinned_witness: u64,
    pub stored_witness: Option<u64>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct LeanRenderedCaptureKeyCase {
    pub name: String,
    pub left_node_did: u64,
    pub left_session_id: u64,
    pub left_request_id: u64,
    pub left_turn_index: usize,
    pub left_attempt: u32,
    pub right_node_did: u64,
    pub right_session_id: u64,
    pub right_request_id: u64,
    pub right_turn_index: usize,
    pub right_attempt: u32,
    pub same_fact: bool,
}

#[derive(Debug, Deserialize, Clone)]
pub struct LeanCaptureScopeCase {
    pub label: String,
    pub kind: String,
    pub seq: u64,
    pub valid: bool,
}

#[derive(Debug, Deserialize, Clone)]
pub struct LeanCaptureOrderCase {
    pub name: String,
    pub left_label: String,
    pub left_turn: i64,
    pub left_attempt: i64,
    pub right_label: String,
    pub right_turn: i64,
    pub right_attempt: i64,
    pub left_before_right: bool,
}
