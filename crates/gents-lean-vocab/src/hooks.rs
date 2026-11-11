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
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookRef {
    pub collection: String,
    pub doc: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LeanLifecycleHookSubjectKind {
    Actor,
    Document {
        resource: String,
        collection: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookGrantable {
    pub collection: String,
    pub relation: String,
    pub subject: LeanLifecycleHookSubjectKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LeanLifecycleHookSubject {
    Actor { did: String },
    Document { collection: String, doc: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookGrant {
    pub target: LeanLifecycleHookRef,
    pub relation: String,
    pub subject: LeanLifecycleHookSubject,
}

/// `Hooks.grantsValid` on one result: the declared grantable relations, the
/// transaction's actor, what the event names and what core showed, the
/// result's effects and grants.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookGrantCase {
    pub name: String,
    pub declared: Vec<LeanLifecycleHookGrantable>,
    pub actor: String,
    pub named_actors: Vec<String>,
    pub named_docs: Vec<LeanLifecycleHookRef>,
    pub effects: Vec<LeanLifecycleHookEffect>,
    pub grants: Vec<LeanLifecycleHookGrant>,
    pub valid: bool,
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
pub struct LeanLifecycleHookOwedKey {
    pub point: String,
    pub plugin: String,
    pub subject: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeanLifecycleHookOwedSettlement {
    Delivered,
    Dropped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeanLifecycleHookOwedKind {
    Staging,
    OnBehalf,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookOwedRecord {
    pub key: LeanLifecycleHookOwedKey,
    pub kind: LeanLifecycleHookOwedKind,
    pub actor: Option<String>,
    pub runner: String,
    pub attempts: u64,
    pub last_error: Option<String>,
    pub settled: Option<LeanLifecycleHookOwedSettlement>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookOwedWrite {
    pub subject: String,
    pub doc: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LeanLifecycleHookOwedAttempt {
    Applied {
        writes: Vec<LeanLifecycleHookOwedWrite>,
    },
    Failed {
        error: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LeanLifecycleHookOwedStep {
    Owe {
        point: String,
        actor: Option<String>,
        owner: Option<String>,
        signable: Vec<String>,
        keys: Vec<LeanLifecycleHookOwedKey>,
        commit: bool,
    },
    Redrive {
        key: LeanLifecycleHookOwedKey,
        authorized: bool,
        current: bool,
        attempt: LeanLifecycleHookOwedAttempt,
        refused: Vec<LeanLifecycleHookOwedWrite>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookOwedTraceStep {
    pub step: LeanLifecycleHookOwedStep,
    pub runner: Option<String>,
    pub records: Vec<LeanLifecycleHookOwedRecord>,
    pub performed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookOwedTraceCase {
    pub name: String,
    pub trace: Vec<LeanLifecycleHookOwedTraceStep>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookHaltedSession {
    pub session: String,
    pub owner: Option<String>,
    pub latched_at: Option<String>,
    pub current: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookDerivedHalt {
    pub session: String,
    pub at: String,
    pub owner: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookOwedDeriveCase {
    pub name: String,
    pub bound: usize,
    pub sessions: Vec<LeanLifecycleHookHaltedSession>,
    pub derived: Vec<LeanLifecycleHookDerivedHalt>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookAgedOwedRecord {
    pub record: LeanLifecycleHookOwedRecord,
    pub age: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookOwedPruneCase {
    pub name: String,
    pub retention: u64,
    pub records: Vec<LeanLifecycleHookAgedOwedRecord>,
    pub kept: Vec<LeanLifecycleHookAgedOwedRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct LeanLifecycleHookOwedDeliveryCases {
    pub traces: Vec<LeanLifecycleHookOwedTraceCase>,
    pub derive: Vec<LeanLifecycleHookOwedDeriveCase>,
    pub prune: Vec<LeanLifecycleHookOwedPruneCase>,
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
