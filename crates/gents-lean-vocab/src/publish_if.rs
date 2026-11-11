use serde::Deserialize;

use super::{LeanApplyDesiredRow, LeanApplyDocRef};

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanPublishIfExpectation {
    #[serde(rename = "ref")]
    pub target: LeanApplyDocRef,
    pub content: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LeanPublishIfCase {
    pub name: String,
    pub expected: Vec<LeanPublishIfExpectation>,
    pub pre_desired: Vec<LeanApplyDesiredRow>,
    pub candidate: Vec<LeanApplyDesiredRow>,
    pub applied: bool,
    pub expected_after_desired: Vec<LeanApplyDesiredRow>,
}
