use serde::Deserialize;

pub(crate) use gents_protocol::request_lifecycle::RequestLifecycleState as LeanRequestExecutionRequestPhase;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LeanRequestExecutionResponsePhase {
    Absent,
    Streaming,
    Complete,
    Error,
    Interrupted,
}

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
pub(crate) enum LeanRequestExecutionProgressKind {
    Response,
    Tool,
    Transcript,
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
pub(crate) struct LeanRequestExecutionLease {
    pub(crate) status: LeanRequestExecutionLeaseStatus,
    pub(crate) generation: Option<u64>,
    pub(crate) deadline: Option<u64>,
    pub(crate) outcome: Option<LeanRequestExecutionOutcome>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanRequestExecutionWorld {
    pub(crate) request: LeanRequestExecutionRequestPhase,
    pub(crate) response: LeanRequestExecutionResponsePhase,
    pub(crate) lease: LeanRequestExecutionLease,
    pub(crate) used_generations: Vec<u64>,
    pub(crate) now: u64,
    pub(crate) progress_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum LeanRequestExecutionAction {
    Claim {
        generation: u64,
        deadline: u64,
    },
    Begin {
        generation: u64,
    },
    PersistProgress {
        generation: u64,
        progress_kind: LeanRequestExecutionProgressKind,
        deadline: u64,
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
        generation: u64,
    },
    Expire {
        generation: u64,
    },
    Recover {
        expected_generation: u64,
        fresh_generation: u64,
        deadline: u64,
    },
    Finalize {
        generation: u64,
        outcome: LeanRequestExecutionOutcome,
    },
    Revoke {
        expected_generation: u64,
        expected_deadline: u64,
        expected_progress: u64,
        fresh_generation: u64,
        outcome: LeanRequestExecutionOutcome,
    },
    RecoverAndFail {
        expected_generation: u64,
        fresh_generation: u64,
    },
}

impl LeanRequestExecutionAction {
    /// The snake_case action kind, matching the Lean JSON tag. Used to pin
    /// which generated kinds are deliberately not driven through the Rust
    /// authorization seam.
    pub(crate) fn kind(&self) -> &'static str {
        match self {
            Self::Claim { .. } => "claim",
            Self::Begin { .. } => "begin",
            Self::PersistProgress { .. } => "persist_progress",
            Self::SocketTraffic { .. } => "socket_traffic",
            Self::NoOp { .. } => "no_op",
            Self::AdvanceTime { .. } => "advance_time",
            Self::Drop { .. } => "drop",
            Self::Expire { .. } => "expire",
            Self::Recover { .. } => "recover",
            Self::Finalize { .. } => "finalize",
            Self::Revoke { .. } => "revoke",
            Self::RecoverAndFail { .. } => "recover_and_fail",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanRequestExecutionLeaseCase {
    pub(crate) name: String,
    pub(crate) pre: LeanRequestExecutionWorld,
    pub(crate) action: LeanRequestExecutionAction,
    pub(crate) expected: Option<LeanRequestExecutionWorld>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanRequestExecutionLeaseTraceCase {
    pub(crate) name: String,
    pub(crate) pre: LeanRequestExecutionWorld,
    pub(crate) actions: Vec<LeanRequestExecutionAction>,
    pub(crate) expected: Option<LeanRequestExecutionWorld>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub(crate) struct LeanProviderEofCase {
    pub(crate) saw_explicit_final: bool,
    pub(crate) expected_failure: bool,
}
