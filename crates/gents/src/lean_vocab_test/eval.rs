use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanEvalOutcomeCase {
    pub(crate) kind: String,
    pub(crate) provider_reason: Option<String>,
    pub(crate) class: String,
    pub(crate) subject_causable: bool,
}
