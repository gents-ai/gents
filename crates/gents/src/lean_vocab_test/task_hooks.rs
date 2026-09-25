use serde::Deserialize;

/// `effective_timeout_secs` is resolved by the model, not authored. A host
/// executor must consume it rather than re-deriving the default.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanTaskHook {
    pub(crate) hook_id: String,
    pub(crate) phase: String,
    pub(crate) command: Vec<String>,
    pub(crate) timeout_secs: Option<i64>,
    pub(crate) effective_timeout_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum LeanCommandResult {
    Exited { code: i64 },
    LaunchFailed,
    TimedOut,
    Interrupted,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanHookAttempt {
    pub(crate) hook_id: String,
    pub(crate) result: LeanCommandResult,
    pub(crate) succeeded: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanScriptedHookResult {
    pub(crate) hook_id: String,
    pub(crate) result: LeanCommandResult,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum LeanTaskPrimaryError {
    Hook { hook_id: String },
    Agent,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum LeanTaskOutcome {
    Success,
    Failure { error: LeanTaskPrimaryError },
    Interrupted,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanTaskHookAdmissionCase {
    pub(crate) name: String,
    pub(crate) hooks: Vec<LeanTaskHook>,
    pub(crate) expected_admitted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanTaskHookRunCase {
    pub(crate) name: String,
    pub(crate) hooks: Vec<LeanTaskHook>,
    pub(crate) script: Vec<LeanScriptedHookResult>,
    pub(crate) agent: String,
    pub(crate) expected_agent_ran: bool,
    pub(crate) before_attempted: Vec<LeanHookAttempt>,
    pub(crate) after_success_attempted: Vec<LeanHookAttempt>,
    pub(crate) after_failure_attempted: Vec<LeanHookAttempt>,
    pub(crate) finally_attempted: Vec<LeanHookAttempt>,
    pub(crate) cleanup_errors: Vec<String>,
    pub(crate) expected_outcome: LeanTaskOutcome,
    pub(crate) expected_final_outcome: LeanTaskOutcome,
    pub(crate) expected_request_state: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanTaskHookRecoveryCase {
    pub(crate) name: String,
    pub(crate) started: bool,
    pub(crate) hooks: Vec<LeanTaskHook>,
    pub(crate) observed: Vec<LeanHookAttempt>,
    pub(crate) script: Vec<LeanScriptedHookResult>,
    pub(crate) expected_remaining: Vec<String>,
    pub(crate) attempted: Vec<LeanHookAttempt>,
    pub(crate) expected_request_state: String,
}
