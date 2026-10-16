use super::shared::wire_enum;
use super::Turn;
use serde::Deserialize;
use serde::Serialize;

wire_enum!(
    pub enum ReviewDelivery {
        Inline,
        Detached,
    }
);

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ReviewStartParams {
    pub thread_id: String,
    pub target: ReviewTarget,

    #[serde(default)]
    pub delivery: Option<ReviewDelivery>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct ReviewStartResponse {
    pub turn: Turn,
    pub review_thread_id: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum ReviewTarget {
    UncommittedChanges,

    #[serde(rename_all = "camelCase")]
    BaseBranch {
        branch: String,
    },

    #[serde(rename_all = "camelCase")]
    Commit {
        sha: String,
        title: Option<String>,
    },

    #[serde(rename_all = "camelCase")]
    Custom {
        instructions: String,
    },
}
