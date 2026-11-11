use serde::Deserialize;

use super::LeanLifecycleHookRef;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanSupersedeReference {
    pub goal: String,
    pub root: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanSupersedeSelector {
    pub goal: String,
    pub root: String,
    pub successor: String,
    pub successor_doc: String,
    pub reason: String,
    pub next: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanSupersedeRow {
    pub doc: String,
    pub request: String,
    pub pending: bool,
    pub goal_continuation: bool,
    pub trigger: Option<String>,
    pub reference: Option<LeanSupersedeReference>,
    pub signed: bool,
    pub parent: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanSupersedeParent {
    pub doc: String,
    pub in_session: bool,
    pub signed: bool,
    pub goal: Option<String>,
    pub reference: Option<LeanSupersedeReference>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeanSupersedeLink {
    Unsigned,
    OtherGoal,
    NoParent,
    ParentAbsent,
    ParentOutsideSession,
    ParentUnsigned,
    Diverges,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LeanSupersedeAncestry {
    Ordinary,
    Broken { link: LeanSupersedeLink },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanSupersedeStale {
    pub doc: String,
    pub request: String,
    pub successor: String,
    pub successor_doc: String,
    pub reason: String,
    pub ancestry: LeanSupersedeAncestry,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookSupersedeCase {
    pub name: String,
    pub selector: LeanSupersedeSelector,
    pub rows: Vec<LeanSupersedeRow>,
    pub parents: Vec<LeanSupersedeParent>,
    pub stale: Vec<LeanSupersedeStale>,
}

/// An RFC 3339 text with the instant, in nanoseconds since the epoch, the
/// model compares; `None` where the text does not parse.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanGoalHaltStamp {
    pub text: String,
    pub instant: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LeanGoalHaltVerdict {
    Inactive,
    ChangedAfter,
    UnreadableHalt,
    Pause { status: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookGoalHaltCase {
    pub name: String,
    pub status: Option<String>,
    pub updated: Option<LeanGoalHaltStamp>,
    pub halt: LeanGoalHaltStamp,
    pub verdict: LeanGoalHaltVerdict,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanHistoryRead {
    pub collection: String,
    pub doc: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookHistoryCase {
    pub name: String,
    pub owns: Vec<String>,
    pub history: Vec<String>,
    pub named: Vec<LeanLifecycleHookRef>,
    pub read: LeanHistoryRead,
    pub permitted: bool,
}
