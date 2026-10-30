use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

use super::{DataQuery, JobState};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct ChildOutputRequest {
    pub slots: Vec<String>,
    pub offset: u64,
    pub limit: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct ChildOutputResponse {
    pub slots: BTreeMap<String, ChildOutputSlice>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct JobResultRequest {
    pub row_doc_id: String,
    pub offset: u64,
    pub limit: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct JobResultResponse {
    pub row_doc_id: String,
    pub state: JobState,
    pub result: String,
    pub offset: u64,
    pub total_bytes: u64,
    pub truncated: bool,
}

/// One `data_calls` request; its answer is `data_results[id]`, a
/// `RoundResult<DataQueryResponse>`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct DataCall {
    pub id: String,
    pub query: DataQuery,
}

/// The rows a `data_calls.query` returns, each holding the requested `fields`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct DataQueryResponse {
    #[cfg_attr(feature = "typescript", ts(type = "Array<Record<string, unknown>>"))]
    pub rows: Vec<serde_json::Map<String, serde_json::Value>>,
}

/// One `job_calls` request; its answer is `job_results[id]`, a `RoundResult`
/// carrying a `ChildOutputResponse` or a `JobResultResponse`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged, deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum JobCall {
    ChildOutput {
        id: String,
        child_output: ChildOutputRequest,
    },
    Result {
        id: String,
        result: JobResultRequest,
    },
}

/// The answer to one round request, keyed by the request's `id`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(untagged, deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub enum RoundResult<T> {
    Result { result: T },
    Error { error: String },
}

/// One requested slot's terminal text from `offset`, capped at `limit` and at
/// `result_bytes`; `truncated` when the text continues past this slice.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
pub struct ChildOutputSlice {
    pub text: String,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[cfg_attr(feature = "typescript", ts(as = "Option<bool>", optional))]
    pub truncated: bool,
}
