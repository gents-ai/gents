use serde::Deserialize;

pub use gents_protocol::request_lifecycle::RequestLifecycleState as LeanRequestExecutionRequestPhase;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeanRequestExecutionOutcome {
    Completed,
    Failed,
    Interrupted,
    Dead,
    Superseded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeanRequestExecutionBoundary {
    MutationWriteGate,
    ValidatedTransaction,
    ObservingReplica,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeanRequestExecutionProducerDecision {
    CloseOrRetract,
    AcceptAndPublish,
    Dispatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LeanRequestExecutionLeaseStatus {
    Vacant,
    Active,
    Recoverable,
    Terminal,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanRequestExecutionLease {
    pub status: LeanRequestExecutionLeaseStatus,
    pub generation: Option<u64>,
    pub duration: Option<u64>,
    pub explicit_deadline: Option<u64>,
    pub outcome: Option<LeanRequestExecutionOutcome>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanRequestExecutionWorld {
    pub request: LeanRequestExecutionRequestPhase,
    pub lease: LeanRequestExecutionLease,
    pub used_generations: Vec<u64>,
    pub now: u64,
    pub effective_expiry: u64,
    pub continuation_required: bool,
    pub token_charge_required: bool,
    pub continuation_count: u64,
    pub token_charge_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum LeanRequestExecutionAction {
    Claim {
        boundary: LeanRequestExecutionBoundary,
        generation: u64,
        duration: u64,
        explicit_deadline: u64,
    },
    Begin {
        boundary: LeanRequestExecutionBoundary,
        generation: u64,
    },
    AppendOutput {
        boundary: LeanRequestExecutionBoundary,
        generation: u64,
    },
    Renew {
        boundary: LeanRequestExecutionBoundary,
        generation: u64,
        expected_deadline: u64,
    },
    AuthorizeProducerDecision {
        boundary: LeanRequestExecutionBoundary,
        generation: u64,
        decision: LeanRequestExecutionProducerDecision,
    },
    SocketTraffic {
        generation: u64,
    },
    NoOp {
        generation: u64,
    },
    AdvanceTime {
        now: u64,
    },
    Drop {
        boundary: LeanRequestExecutionBoundary,
        generation: u64,
    },
    RecoverExpired {
        boundary: LeanRequestExecutionBoundary,
        expected_generation: u64,
        fresh_generation: u64,
        duration: u64,
        explicit_deadline: u64,
    },
    RecoverDropped {
        boundary: LeanRequestExecutionBoundary,
        expected_generation: u64,
        fresh_generation: u64,
        duration: u64,
        explicit_deadline: u64,
    },
    Finalize {
        boundary: LeanRequestExecutionBoundary,
        generation: u64,
        outcome: LeanRequestExecutionOutcome,
    },
    PolicyRevoke {
        boundary: LeanRequestExecutionBoundary,
        expected_generation: u64,
        fresh_generation: u64,
        outcome: LeanRequestExecutionOutcome,
    },
    RecoverExpiredAndFail {
        boundary: LeanRequestExecutionBoundary,
        expected_generation: u64,
        fresh_generation: u64,
    },
    RecoverExpiredTerminal {
        boundary: LeanRequestExecutionBoundary,
        expected_generation: u64,
        fresh_generation: u64,
        outcome: LeanRequestExecutionOutcome,
    },
    RecoverDroppedAndFail {
        boundary: LeanRequestExecutionBoundary,
        expected_generation: u64,
        fresh_generation: u64,
    },
}

impl LeanRequestExecutionAction {
    pub fn kind(&self) -> &'static str {
        match self {
            Self::Claim { .. } => "claim",
            Self::Begin { .. } => "begin",
            Self::AppendOutput { .. } => "append_output",
            Self::Renew { .. } => "renew",
            Self::AuthorizeProducerDecision { .. } => "authorize_producer_decision",
            Self::SocketTraffic { .. } => "socket_traffic",
            Self::NoOp { .. } => "no_op",
            Self::AdvanceTime { .. } => "advance_time",
            Self::Drop { .. } => "drop",
            Self::RecoverExpired { .. } => "recover_expired",
            Self::RecoverDropped { .. } => "recover_dropped",
            Self::Finalize { .. } => "finalize",
            Self::PolicyRevoke { .. } => "policy_revoke",
            Self::RecoverExpiredAndFail { .. } => "recover_expired_and_fail",
            Self::RecoverExpiredTerminal { .. } => "recover_expired_terminal",
            Self::RecoverDroppedAndFail { .. } => "recover_dropped_and_fail",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanRequestExecutionLeaseCase {
    pub name: String,
    pub pre: LeanRequestExecutionWorld,
    pub action: LeanRequestExecutionAction,
    pub expected: Option<LeanRequestExecutionWorld>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanRequestExecutionObservedCase {
    pub name: String,
    pub pre: LeanRequestExecutionWorld,
    pub action: LeanRequestExecutionAction,
    pub intervening: Vec<LeanRequestExecutionAction>,
    pub expected: Option<LeanRequestExecutionWorld>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanRequestExecutionLeaseTraceCase {
    pub name: String,
    pub pre: LeanRequestExecutionWorld,
    pub actions: Vec<LeanRequestExecutionAction>,
    pub expected: Option<LeanRequestExecutionWorld>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LeanProviderEofCase {
    pub saw_explicit_final: bool,
    pub expected_failure: bool,
}

pub fn assert_request_execution_lease_cases<E, F>(
    cases: &[LeanRequestExecutionLeaseCase],
    mut run: F,
) -> Result<(), E>
where
    F: FnMut(
        &LeanRequestExecutionWorld,
        &LeanRequestExecutionAction,
    ) -> Result<Option<LeanRequestExecutionWorld>, E>,
{
    assert!(
        !cases.is_empty(),
        "lease conformance cases must not be empty"
    );
    for case in cases {
        let actual = run(&case.pre, &case.action)?;
        assert_eq!(actual, case.expected, "{}", case.name);
    }
    Ok(())
}

pub fn assert_request_execution_lease_trace_cases<E, F>(
    cases: &[LeanRequestExecutionLeaseTraceCase],
    mut run: F,
) -> Result<(), E>
where
    F: FnMut(
        &LeanRequestExecutionWorld,
        &[LeanRequestExecutionAction],
    ) -> Result<Option<LeanRequestExecutionWorld>, E>,
{
    assert!(
        !cases.is_empty(),
        "lease conformance traces must not be empty"
    );
    for case in cases {
        assert!(!case.actions.is_empty(), "{}: empty lease trace", case.name);
        let actual = run(&case.pre, &case.actions)?;
        assert_eq!(actual, case.expected, "{}", case.name);
    }
    Ok(())
}
