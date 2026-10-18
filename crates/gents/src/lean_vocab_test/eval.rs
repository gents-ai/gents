use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanEvalOutcomeCase {
    pub(crate) kind: String,
    pub(crate) provider_reason: Option<String>,
    pub(crate) class: String,
    pub(crate) subject_causable: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct LeanEvalDocumentCaptureCase {
    pub(crate) collection: String,
    pub(crate) fields: Vec<String>,
    pub(crate) filter_fields: Vec<String>,
    pub(crate) filter: serde_json::Value,
    pub(crate) allowed: bool,
}
