use gents_protocol::document_query::{fields, Direction, Filter, Query, Selection};
use gents_protocol::views::View;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct SettingsRow<T> {
    #[serde(rename = "_docID")]
    pub doc_id: String,
    #[serde(flatten)]
    pub settings: T,
}

macro_rules! settings_view {
    ($name:ident, $module:ident, $row:ty, $id:ident, [$($field:ident),*]) => {
        #[derive(Debug, Clone)]
        pub struct $name {
            node_did: String,
            id: Option<String>,
            after: Option<String>,
            doc_ids: Option<Vec<String>>,
            limit: Option<u64>,
        }

        impl $name {
            pub fn new(node_did: impl Into<String>) -> Self {
                Self { node_did: node_did.into(), id: None, after: None, doc_ids: None, limit: None }
            }

            pub fn id(mut self, id: impl Into<String>) -> Self {
                self.id = Some(id.into());
                self
            }

            pub fn after(mut self, after: impl Into<String>) -> Self {
                self.after = Some(after.into());
                self
            }

            pub fn doc_ids(mut self, doc_ids: Vec<String>) -> Self {
                self.doc_ids = Some(doc_ids);
                self
            }

            pub fn limit(mut self, limit: u64) -> Self {
                self.limit = Some(limit);
                self
            }
        }

        impl View for $name {
            type Collection = fields::$module::Doc;
            type Row = SettingsRow<$row>;

            fn query(&self, _reader_did: &str) -> Query<Self::Collection> {
                let selection = Selection::new()
                    .field(fields::$module::DOC_ID)
                    .field(fields::$module::NODE_DID)
                    .field(fields::$module::$id)
                    $(.field(fields::$module::$field))*;
                let mut query = Query::select(selection)
                    .filter(Filter::eq(fields::$module::NODE_DID, &self.node_did))
                    .order_by(fields::$module::$id, Direction::Asc);
                if let Some(id) = &self.id {
                    query = query.filter(Filter::eq(fields::$module::$id, id));
                }
                if let Some(after) = &self.after {
                    query = query.filter(Filter::gt(fields::$module::$id, after));
                }
                if let Some(doc_ids) = &self.doc_ids {
                    query = query.filter(Filter::any_of(fields::$module::DOC_ID, doc_ids.iter()));
                }
                if let Some(limit) = self.limit {
                    query = query.limit(limit);
                }
                query
            }
        }
    };
}

settings_view!(
    SamplingList,
    inference_sampling,
    gents_documents::InferenceSampling,
    SAMPLING_ID,
    [
        DISPLAY_NAME,
        TEMPERATURE,
        TOP_P,
        TOP_K,
        SEED,
        MIN_P,
        FREQUENCY_PENALTY,
        PRESENCE_PENALTY,
        REPETITION_PENALTY,
        TAGS
    ]
);
settings_view!(
    ExecutionList,
    inference_execution,
    gents_documents::InferenceExecution,
    EXECUTION_ID,
    [
        DISPLAY_NAME,
        MAX_TURNS,
        MAX_TOTAL_TOKENS,
        STREAM_BATCH_MS,
        STREAM_LIVENESS_TIMEOUT_SECS,
        PROVIDER_IDLE_TIMEOUT_SECS,
        DEADLINE_DURATION_SECS,
        RETRY_POLICY_ID,
        TAGS
    ]
);
settings_view!(
    RetryPolicyList,
    inference_retry_policy,
    gents_documents::InferenceRetryPolicy,
    RETRY_POLICY_ID,
    [
        DISPLAY_NAME,
        MAX_TRANSPORT_RETRIES,
        BACKOFF_MS,
        MAX_RESAMPLE_RETRIES,
        ALLOW_REPAIR,
        INTERACTIVE_MAX_RETRIES,
        TAGS
    ]
);
