use crate::generated::*;

pub use gents_protocol::request_lifecycle::RequestLifecycleState as LeanRequestExecutionRequestPhase;

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
