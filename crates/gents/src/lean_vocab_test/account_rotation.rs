use serde::Deserialize;

use super::lean_contract_snapshot;

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanAccountRotationLimit {
    pub(crate) claim: String,
    pub(crate) severity: String,
    pub(crate) reset_at: u64,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanAccountRotationAccount {
    pub(crate) enabled: bool,
    pub(crate) limits: Vec<LeanAccountRotationLimit>,
    pub(crate) overage: String,
    pub(crate) blocked_until: u64,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanAccountRotationSelectCase {
    pub(crate) name: String,
    pub(crate) allow_overage: bool,
    pub(crate) now: u64,
    pub(crate) family: String,
    pub(crate) pool: Vec<LeanAccountRotationAccount>,
    pub(crate) current: Option<usize>,
    pub(crate) cursor: usize,
    pub(crate) expected: Option<usize>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanAccountRotationRejection {
    pub(crate) account: usize,
    pub(crate) reset_at: u64,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanAccountRotationAttemptCase {
    pub(crate) name: String,
    pub(crate) allow_overage: bool,
    pub(crate) now: u64,
    pub(crate) family: String,
    pub(crate) pool: Vec<LeanAccountRotationAccount>,
    pub(crate) current: Option<usize>,
    pub(crate) cursor: usize,
    pub(crate) rejects: Vec<LeanAccountRotationRejection>,
    pub(crate) expected: Vec<usize>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanAccountRotationObserveCase {
    pub(crate) name: String,
    pub(crate) family: String,
    pub(crate) account: LeanAccountRotationAccount,
    pub(crate) reported: Vec<LeanAccountRotationLimit>,
    pub(crate) overage: String,
    pub(crate) expected: LeanAccountRotationAccount,
}

pub(crate) fn lean_account_rotation_select_cases() -> &'static [LeanAccountRotationSelectCase] {
    &lean_contract_snapshot().account_rotation_select_cases
}

pub(crate) fn lean_account_rotation_attempt_cases() -> &'static [LeanAccountRotationAttemptCase] {
    &lean_contract_snapshot().account_rotation_attempt_cases
}

pub(crate) fn lean_account_rotation_observe_cases() -> &'static [LeanAccountRotationObserveCase] {
    &lean_contract_snapshot().account_rotation_observe_cases
}
