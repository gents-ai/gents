use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanTaskHook {
    pub hook_id: String,
    pub phase: String,
    pub command: Vec<String>,
    pub timeout_secs: Option<i64>,
    pub skip_exit_codes: Vec<i64>,
    pub effective_timeout_secs: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LeanCommandResult {
    Exited { code: i64 },
    LaunchFailed,
    TimedOut,
    Interrupted,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanHookAttempt {
    pub hook_id: String,
    pub result: LeanCommandResult,
    pub succeeded: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanScriptedHookResult {
    pub hook_id: String,
    pub result: LeanCommandResult,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LeanTaskPrimaryError {
    Hook { hook_id: String },
    Agent,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LeanTaskOutcome {
    Parked,
    Success,
    Failure { error: LeanTaskPrimaryError },
    Interrupted,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LeanHookInvocation {
    Hook { hook_id: String },
    Work,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanTaskHookAdmissionCase {
    pub name: String,
    pub hooks: Vec<LeanTaskHook>,
    pub expected_admitted: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanTaskHookRunCase {
    pub name: String,
    pub hooks: Vec<LeanTaskHook>,
    pub script: Vec<LeanScriptedHookResult>,
    pub revoked: Vec<String>,
    pub refused_before_launch: Vec<String>,
    pub agent: String,
    pub expected_agent_ran: bool,
    pub before_attempted: Vec<LeanHookAttempt>,
    pub after_success_attempted: Vec<LeanHookAttempt>,
    pub after_failure_attempted: Vec<LeanHookAttempt>,
    pub finally_attempted: Vec<LeanHookAttempt>,
    pub invocation_trace: Vec<LeanHookInvocation>,
    pub cleanup_errors: Vec<String>,
    pub expected_outcome: LeanTaskOutcome,
    pub expected_final_outcome: LeanTaskOutcome,
    pub expected_request_state: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanTaskHookRecoveryCase {
    pub name: String,
    pub interrupt_requested: bool,
    pub started: bool,
    pub hooks: Vec<LeanTaskHook>,
    pub observed: Vec<LeanHookAttempt>,
    pub script: Vec<LeanScriptedHookResult>,
    pub expected_remaining: Vec<String>,
    pub attempted: Vec<LeanHookAttempt>,
    pub expected_request_state: String,
}
