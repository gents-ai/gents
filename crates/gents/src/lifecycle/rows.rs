#[derive(Debug, Clone)]
pub(super) struct DedupPlan {
    pub(super) is_earliest: bool,
    pub(super) blocking_request_id: Option<String>,
}
