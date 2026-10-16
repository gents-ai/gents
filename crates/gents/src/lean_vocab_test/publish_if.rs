use serde::Deserialize;

use super::{LeanApplyDesiredRow, LeanApplyDocRef};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanPublishIfExpectation {
    #[serde(rename = "ref")]
    pub(crate) target: LeanApplyDocRef,
    pub(crate) content: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanPublishIfCase {
    pub(crate) name: String,
    pub(crate) expected: Vec<LeanPublishIfExpectation>,
    pub(crate) pre_desired: Vec<LeanApplyDesiredRow>,
    pub(crate) candidate: Vec<LeanApplyDesiredRow>,
    pub(crate) applied: bool,
    pub(crate) expected_after_desired: Vec<LeanApplyDesiredRow>,
}
