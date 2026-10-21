use serde::Deserialize;

#[derive(Debug, Deserialize, Clone)]
pub(crate) struct LeanRenderedCaptureCase {
    pub(crate) name: String,
    pub(crate) node_did: u64,
    pub(crate) session_id: u64,
    pub(crate) request_id: u64,
    pub(crate) turn_index: usize,
    pub(crate) attempt: u32,
    pub(crate) request: u64,
    pub(crate) prior_binding: Option<u64>,
    pub(crate) capture_outcome: String,
    pub(crate) capture_durable: bool,
    pub(crate) post_stage: String,
    pub(crate) send_permitted: bool,
    pub(crate) provider_requests_observed: usize,
    pub(crate) durable_after: Option<u64>,
    pub(crate) final_stage: String,
}

#[derive(Debug, Deserialize, Clone)]
pub(crate) struct LeanRenderedCaptureStorageCase {
    pub(crate) name: String,
    pub(crate) encoding: String,
    pub(crate) request: u64,
    pub(crate) base_witness: Option<u64>,
    pub(crate) base_depth: usize,
    pub(crate) max_depth: usize,
    pub(crate) base_verified: bool,
    pub(crate) manifest_blocks: Vec<LeanRenderedCaptureManifestBlock>,
    pub(crate) manifest_root: Option<LeanRenderedCaptureManifestBlock>,
    pub(crate) decoded_request: Option<u64>,
    pub(crate) send_permitted: bool,
}

#[derive(Debug, Deserialize, Clone)]
pub(crate) struct LeanRenderedCaptureManifestBlock {
    pub(crate) r#ref: u64,
    pub(crate) pinned_witness: u64,
    pub(crate) stored_witness: Option<u64>,
}

#[derive(Debug, Deserialize, Clone)]
pub(crate) struct LeanRenderedCaptureKeyCase {
    pub(crate) name: String,
    pub(crate) left_node_did: u64,
    pub(crate) left_session_id: u64,
    pub(crate) left_request_id: u64,
    pub(crate) left_turn_index: usize,
    pub(crate) left_attempt: u32,
    pub(crate) right_node_did: u64,
    pub(crate) right_session_id: u64,
    pub(crate) right_request_id: u64,
    pub(crate) right_turn_index: usize,
    pub(crate) right_attempt: u32,
    pub(crate) same_fact: bool,
}

#[derive(Debug, Deserialize, Clone)]
pub(crate) struct LeanCaptureScopeCase {
    pub(crate) label: String,
    pub(crate) kind: String,
    pub(crate) seq: u64,
    pub(crate) valid: bool,
}

#[derive(Debug, Deserialize, Clone)]
pub(crate) struct LeanCaptureOrderCase {
    pub(crate) name: String,
    pub(crate) left_label: String,
    pub(crate) left_turn: i64,
    pub(crate) left_attempt: i64,
    pub(crate) right_label: String,
    pub(crate) right_turn: i64,
    pub(crate) right_attempt: i64,
    pub(crate) left_before_right: bool,
}
