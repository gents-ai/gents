use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookRow {
    pub collection: String,
    pub doc: String,
    pub revision: u64,
    pub value: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeanLifecycleHookAccess {
    Create,
    Update,
    Delete,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LeanLifecycleHookEffect {
    Create {
        collection: String,
        doc: String,
        value: String,
    },
    Update {
        collection: String,
        doc: String,
        expected: u64,
        value: String,
    },
    Delete {
        collection: String,
        doc: String,
        expected: u64,
    },
    Core {
        op: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookDenial {
    pub access: LeanLifecycleHookAccess,
    pub collection: String,
    pub doc: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookWorld {
    pub rows: Vec<LeanLifecycleHookRow>,
    pub core: Vec<String>,
    pub denied: Vec<LeanLifecycleHookDenial>,
    pub failing_ops: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookState {
    pub core: Vec<String>,
    pub rows: Vec<LeanLifecycleHookRow>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookManifest {
    pub name: String,
    pub owns: Vec<String>,
    pub bound: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LeanLifecycleHookRefusal {
    Core,
    Refused { plugin: String, reason: String },
    Invalid { plugin: String },
    Conflict { plugin: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookChoice<V> {
    pub value: V,
    pub effects: Vec<LeanLifecycleHookEffect>,
}

/// A scripted plugin decision; `by_shown` picks `matching` when the plugin was
/// shown exactly `count` rows across its declared reads.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LeanLifecycleHookDecision<V> {
    Refuse {
        reason: String,
    },
    Proceed {
        value: V,
        effects: Vec<LeanLifecycleHookEffect>,
    },
    ByShown {
        count: usize,
        matching: LeanLifecycleHookChoice<V>,
        otherwise: LeanLifecycleHookChoice<V>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookScript<V> {
    pub name: String,
    pub owns: Vec<String>,
    pub bound: usize,
    pub reads: Vec<String>,
    pub decision: LeanLifecycleHookDecision<V>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LeanLifecycleHookVerdict {
    Allow,
    LeaveQueued,
    Retire { reason: String },
    Reject { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookEffectCase {
    pub name: String,
    pub world: LeanLifecycleHookWorld,
    pub manifest: LeanLifecycleHookManifest,
    pub effects: Vec<LeanLifecycleHookEffect>,
    pub valid: bool,
    pub committed: Option<LeanLifecycleHookState>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LeanLifecycleHookStageResult {
    Ok {
        state: LeanLifecycleHookState,
        values: Vec<String>,
    },
    Error {
        refusal: LeanLifecycleHookRefusal,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookStageCase {
    pub name: String,
    pub world: LeanLifecycleHookWorld,
    pub plugins: Vec<LeanLifecycleHookScript<String>>,
    pub result: LeanLifecycleHookStageResult,
    pub settled: LeanLifecycleHookState,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookMustCommitted {
    pub state: LeanLifecycleHookState,
    pub dropped: Vec<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookMustCommitCase {
    pub name: String,
    pub world: LeanLifecycleHookWorld,
    pub plugins: Vec<LeanLifecycleHookScript<String>>,
    pub transition: String,
    pub transition_refused: bool,
    pub read_failed: Vec<String>,
    pub committed: Option<LeanLifecycleHookMustCommitted>,
    pub owed: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookCombineCase {
    pub name: String,
    pub verdicts: Vec<LeanLifecycleHookVerdict>,
    pub combined: LeanLifecycleHookVerdict,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LeanLifecycleHookGateResult {
    Ok {
        verdicts: Vec<LeanLifecycleHookVerdict>,
        combined: LeanLifecycleHookVerdict,
    },
    Error {
        refusal: LeanLifecycleHookRefusal,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookGateCase {
    pub name: String,
    pub world: LeanLifecycleHookWorld,
    pub plugins: Vec<LeanLifecycleHookScript<LeanLifecycleHookVerdict>>,
    pub result: LeanLifecycleHookGateResult,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LeanLifecycleHookScopeAnswer {
    Unanswered,
    Keyed { key: String },
    Failed { reason: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookScopeKey {
    pub plugin: String,
    pub key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LeanLifecycleHookPartition {
    Ok {
        partition: Vec<LeanLifecycleHookScopeKey>,
    },
    Error {
        refusal: LeanLifecycleHookRefusal,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LeanLifecycleHookQueueScopeResult {
    Ok {
        scopes: Vec<LeanLifecycleHookPartition>,
    },
    Error {
        refusal: LeanLifecycleHookRefusal,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookQueueScopeCase {
    pub name: String,
    pub world: LeanLifecycleHookWorld,
    pub plugins: Vec<LeanLifecycleHookScript<Vec<LeanLifecycleHookScopeAnswer>>>,
    pub asked: Vec<String>,
    pub result: LeanLifecycleHookQueueScopeResult,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookDeliveryRecord {
    pub terminal: Option<String>,
    pub owed: Vec<String>,
    pub delivered: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LeanLifecycleHookDeliveryStep {
    Publish {
        outcome: String,
        keys: Vec<String>,
        commit: bool,
    },
    Deliver {
        key: String,
        applied: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookDeliveryTraceStep {
    pub step: LeanLifecycleHookDeliveryStep,
    pub state: LeanLifecycleHookDeliveryRecord,
    pub pending: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookDeliveryCase {
    pub name: String,
    pub initial: LeanLifecycleHookDeliveryRecord,
    pub trace: Vec<LeanLifecycleHookDeliveryTraceStep>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookBinding {
    pub actor: String,
    pub point: String,
    pub subject: String,
    pub inputs: String,
    pub configuration: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LeanLifecycleHookPreparedVerdict {
    Allow { payload: String },
    Deny { reason: String },
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookPrepared {
    pub binding: LeanLifecycleHookBinding,
    pub verdict: LeanLifecycleHookPreparedVerdict,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LeanLifecycleHookStale {
    Unauthorized,
    Cancelled,
    Missing,
    Stale,
    Denied { reason: String },
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LeanLifecycleHookAdmitResult {
    Ok { payload: String },
    Error { stale: LeanLifecycleHookStale },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookAdmitCase {
    pub name: String,
    pub authorized: bool,
    pub cancelled: bool,
    pub current: LeanLifecycleHookBinding,
    pub prepared: Option<LeanLifecycleHookPrepared>,
    pub result: LeanLifecycleHookAdmitResult,
}
