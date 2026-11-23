use gents_protocol::document_query::{fields::inference_call, Direction, Filter, Query, Selection};
use gents_protocol::provider_limit::ProviderLimitVerdict;
use gents_protocol::views::View;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct LatestCallRow {
    pub call_id: String,
    pub agent_did: Option<String>,
    pub queued_at: Option<String>,
    pub provider_limit_resets_at: Option<String>,
    pub provider_limit_window_minutes: Option<u64>,
    pub provider_limit_tokens_used: Option<u64>,
    pub provider_limit_tokens_limit: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LatestCallKind {
    UsageLimited,
    Completed,
}

/// The latest call by `queued_at` on one backend and model, among those the
/// reader may read: the latest usage-limited failure, or the latest completion.
#[derive(Debug, Clone)]
pub struct LatestCall {
    pub node_did: String,
    pub backend_id: String,
    pub model_name: String,
    pub kind: LatestCallKind,
}

impl View for LatestCall {
    type Collection = inference_call::Doc;
    type Row = LatestCallRow;

    fn query(&self, _reader_did: &str) -> Query<Self::Collection> {
        let selection = Selection::new()
            .field(inference_call::CALL_ID)
            .field(inference_call::AGENT_DID)
            .field(inference_call::QUEUED_AT)
            .field(inference_call::PROVIDER_LIMIT_RESETS_AT)
            .field(inference_call::PROVIDER_LIMIT_WINDOW_MINUTES)
            .field(inference_call::PROVIDER_LIMIT_TOKENS_USED)
            .field(inference_call::PROVIDER_LIMIT_TOKENS_LIMIT);
        let query = Query::select(selection)
            .filter(Filter::eq(inference_call::NODE_DID, &self.node_did))
            .filter(Filter::eq(inference_call::BACKEND_ID, &self.backend_id))
            .filter(Filter::eq(inference_call::MODEL_NAME, &self.model_name))
            .filter(Filter::not_null(inference_call::QUEUED_AT));
        let query = match self.kind {
            LatestCallKind::UsageLimited => query
                .filter(Filter::eq(inference_call::CALL_STATE, "failed"))
                .filter(Filter::eq(
                    inference_call::PROVIDER_LIMIT,
                    ProviderLimitVerdict::UsageLimited.as_str(),
                )),
            LatestCallKind::Completed => {
                query.filter(Filter::eq(inference_call::CALL_STATE, "completed"))
            }
        };
        query
            .order_by(inference_call::QUEUED_AT, Direction::Desc)
            .order_by(inference_call::CALL_ID, Direction::Desc)
            .limit(1)
    }
}
