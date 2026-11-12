use crate::generated::*;
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCanonicalHeader {
    pub id: u64,
    pub session: u64,
    pub request: Option<u64>,
    pub origin: Option<u64>,
    pub refs: Vec<LeanPayloadRef>,
    pub outcome: LeanOutcome,
    pub role: LeanMessageRole,
    pub publication: LeanMessagePublication,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanMessagePublication {
    RequestExecution { generation: u64 },
    RequestRecovery { generation: u64 },
    ToolDelivery { call: u64 },
    Fork { origin: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanCanonicalMessage<T> {
    pub header: LeanCanonicalHeader,
    pub key: String,
    pub sequence: u64,
    pub native_id: Option<String>,
    pub native_request_id: Option<String>,
    pub blocks: Vec<LeanMessageBlock<T>>,
    pub created_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanMessageBlock<T> {
    Text {
        payload: T,
    },
    Reasoning {
        id: Option<String>,
        parts: Vec<LeanReasoningPart<T>>,
    },
    ToolCall {
        doc_id: u64,
        id: String,
        call_id: Option<String>,
        name: String,
        arguments: T,
        signature: Option<String>,
        additional_params: Option<String>,
    },
    ToolResult {
        doc_id: u64,
        id: String,
        call_id: Option<String>,
        parts: Vec<LeanResultPart<T>>,
    },
    Media {
        value: LeanMedia<T>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanReasoningPart<T> {
    Text {
        payload: T,
        signature: Option<String>,
    },
    Encrypted {
        payload: T,
    },
    Redacted {
        payload: T,
    },
    Summary {
        payload: T,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanResultPart<T> {
    Text { payload: T },
    Media { value: LeanMedia<T> },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanMedia<T> {
    pub kind: LeanMediaKind,
    pub data: LeanMediaData<T>,
    pub media_type: Option<String>,
    pub detail: Option<String>,
    pub additional_params: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanMediaData<T> {
    Url { url: String },
    Base64 { payload: T },
    Raw { payload: T },
    String { payload: T },
    Unknown,
}
