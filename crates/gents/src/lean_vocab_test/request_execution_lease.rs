use serde::Deserialize;

pub(crate) use gents_protocol::request_lifecycle::RequestLifecycleState as LeanRequestExecutionRequestPhase;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LeanRequestExecutionOutcome {
    Completed,
    Failed,
    Interrupted,
    Dead,
    Superseded,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LeanRequestExecutionBoundary {
    MutationWriteGate,
    ObservingReplica,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LeanRequestExecutionProducerDecision {
    CloseOrRetract,
    AcceptAndPublish,
    Dispatch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LeanRequestExecutionLeaseStatus {
    Vacant,
    Active,
    Recoverable,
    Terminal,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanRequestExecutionLease {
    pub(crate) status: LeanRequestExecutionLeaseStatus,
    pub(crate) generation: Option<u64>,
    pub(crate) duration: Option<u64>,
    pub(crate) explicit_deadline: Option<u64>,
    pub(crate) outcome: Option<LeanRequestExecutionOutcome>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanRequestExecutionWorld {
    pub(crate) request: LeanRequestExecutionRequestPhase,
    pub(crate) lease: LeanRequestExecutionLease,
    pub(crate) used_generations: Vec<u64>,
    pub(crate) now: u64,
    pub(crate) effective_expiry: u64,
    pub(crate) continuation_required: bool,
    pub(crate) token_charge_required: bool,
    pub(crate) continuation_count: u64,
    pub(crate) token_charge_count: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub(crate) enum LeanRequestExecutionAction {
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
    RecoverDroppedAndFail {
        boundary: LeanRequestExecutionBoundary,
        expected_generation: u64,
        fresh_generation: u64,
    },
}

impl LeanRequestExecutionAction {
    pub(crate) fn kind(&self) -> &'static str {
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
            Self::RecoverDroppedAndFail { .. } => "recover_dropped_and_fail",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanRequestExecutionLeaseCase {
    pub(crate) name: String,
    pub(crate) pre: LeanRequestExecutionWorld,
    pub(crate) action: LeanRequestExecutionAction,
    pub(crate) expected: Option<LeanRequestExecutionWorld>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanRequestExecutionLeaseTraceCase {
    pub(crate) name: String,
    pub(crate) pre: LeanRequestExecutionWorld,
    pub(crate) actions: Vec<LeanRequestExecutionAction>,
    pub(crate) expected: Option<LeanRequestExecutionWorld>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct LeanProviderEofCase {
    pub(crate) saw_explicit_final: bool,
    pub(crate) expected_failure: bool,
}

pub(crate) fn assert_request_execution_lease_cases<E, F>(
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

pub(crate) fn assert_request_execution_lease_trace_cases<E, F>(
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
