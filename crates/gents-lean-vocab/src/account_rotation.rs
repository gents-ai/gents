use serde::Deserialize;

use super::lean_contract_snapshot;

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LeanAccountRotationLimit {
    pub claim: String,
    pub severity: String,
    pub reset_at: u64,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LeanAccountRotationAccount {
    pub enabled: bool,
    pub limits: Vec<LeanAccountRotationLimit>,
    pub overage: String,
    pub blocked_until: u64,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LeanAccountRotationSelectCase {
    pub name: String,
    pub allow_overage: bool,
    pub now: u64,
    pub family: String,
    pub pool: Vec<LeanAccountRotationAccount>,
    pub current: Option<usize>,
    pub cursor: usize,
    pub expected: Option<usize>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LeanAccountRotationRejection {
    pub account: usize,
    pub reset_at: u64,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LeanAccountRotationAttemptCase {
    pub name: String,
    pub allow_overage: bool,
    pub now: u64,
    pub family: String,
    pub pool: Vec<LeanAccountRotationAccount>,
    pub current: Option<usize>,
    pub cursor: usize,
    pub rejects: Vec<LeanAccountRotationRejection>,
    pub expected: Vec<usize>,
}

#[derive(Debug, Deserialize, Clone, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LeanAccountRotationObserveCase {
    pub name: String,
    pub family: String,
    pub account: LeanAccountRotationAccount,
    pub reported: Vec<LeanAccountRotationLimit>,
    pub overage: String,
    pub expected: LeanAccountRotationAccount,
}

pub fn lean_account_rotation_select_cases() -> &'static [LeanAccountRotationSelectCase] {
    &lean_contract_snapshot().account_rotation_select_cases
}

pub fn lean_account_rotation_attempt_cases() -> &'static [LeanAccountRotationAttemptCase] {
    &lean_contract_snapshot().account_rotation_attempt_cases
}

pub fn lean_account_rotation_observe_cases() -> &'static [LeanAccountRotationObserveCase] {
    &lean_contract_snapshot().account_rotation_observe_cases
}
