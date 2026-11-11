use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanEvalOutcomeCase {
    pub kind: String,
    pub provider_reason: Option<String>,
    pub class: String,
    pub subject_causable: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct LeanEvalDocumentCaptureCase {
    pub collection: String,
    pub fields: Vec<String>,
    pub filter_fields: Vec<String>,
    pub filter: serde_json::Value,
    pub allowed: bool,
}
