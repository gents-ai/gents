use serde::Deserialize;

pub type NodeId = String;

/// Executable actions serialized by the Lean R5 scenario owner. Conformance
/// assertions consume this model-derived shape directly.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModeledScenario {
    pub name: String,
    pub child_lease_secs: u64,
    pub cancel_ack_threshold_secs: u64,
    pub actions: Vec<ModeledAction>,
    pub recovery_checkpoints: Vec<ModeledRecoveryCheckpoint>,
    pub expected_notifications: usize,
    pub expected_wakes: usize,
    pub expected_rejected_invocations: usize,
    pub expected_cancel_ack_events: Vec<ModeledCancelAckEvent>,
    pub expected_a_bridges: Vec<ModeledBridgeFact>,
    pub expected_b_children: Vec<ModeledChildFact>,
    pub expected_a_generation: u64,
    pub expected_b_generation: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModeledRecoveryCheckpoint {
    pub after_action: usize,
    pub notification_children: Vec<String>,
    pub wake_sessions: Vec<String>,
    pub bridges: Vec<ModeledBridgeFact>,
    pub children: Vec<ModeledChildFact>,
}

#[derive(Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModeledCancelAckEvent {
    pub tool: String,
    pub outcome: ModeledCancelAckOutcome,
}

#[derive(Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModeledCancelAckOutcome {
    Pending,
    Stuck,
    Acked,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModeledBridgeFact {
    pub tool: String,
    pub child: String,
    pub call_doc: u64,
    pub parent_depth: u32,
    pub parent_workspace: Option<ModeledWorkspaceStamp>,
    pub state: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModeledChildFact {
    pub child: String,
    pub depth: u32,
    pub workspace: Option<ModeledWorkspaceStamp>,
    pub terminal: Option<String>,
    pub interrupt_requested: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModeledWorkspaceStamp {
    pub workspace_id: u64,
    pub workspace_owner_agent_did: u64,
    pub workspace_seal_hash: Option<u64>,
    pub workspace_authority: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModeledObservedWorkspace {
    pub workspace_id: u64,
    pub workspace_owner_agent_did: u64,
    pub workspace_seal_hash: Option<u64>,
    pub state: String,
    pub available: bool,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ModeledChildChoice {
    None,
    Inherit {
        workspace: ModeledObservedWorkspace,
    },
    Bind {
        workspace: ModeledObservedWorkspace,
        requested_authority: Option<String>,
    },
    Provision {
        observed_parent: ModeledObservedWorkspace,
        parent_path_exact: bool,
        created_child: Option<ModeledObservedWorkspace>,
    },
}

#[derive(Debug, Deserialize)]
#[serde(tag = "op", deny_unknown_fields)]
pub enum ModeledAction {
    PairPrincipals {
        node: NodeId,
        peer: NodeId,
    },
    PublishAcceptedBackgroundBridge {
        tool: String,
        child: String,
        session: String,
        parent_depth: u32,
        call_doc: u64,
        parent_workspace: Option<ModeledWorkspaceStamp>,
        accepted_arguments: Option<String>,
    },
    RejectSpawnInvocation {
        tool: String,
        child: String,
        session: String,
        parent_depth: u32,
    },
    ReplicateBridge {
        tool: String,
        #[serde(rename = "from")]
        source: NodeId,
        to: NodeId,
    },
    MaterializeChild {
        child: String,
        tool: String,
        payload: u64,
        choice: ModeledChildChoice,
    },
    BeginChild {
        child: String,
        generation: u64,
    },
    AwaitChildExpiry {
        child: String,
    },
    ReplicateChild {
        child: String,
        #[serde(rename = "from")]
        source: NodeId,
        to: NodeId,
    },
    PublishChildTerminal {
        child: String,
        terminal: String,
        has_message: bool,
    },
    ReplicateTerminalRequest {
        child: String,
        #[serde(rename = "from")]
        source: NodeId,
        to: NodeId,
    },
    ReplicateOutputSegments {
        child: String,
        #[serde(rename = "from")]
        source: NodeId,
        to: NodeId,
    },
    ReplicateMessageHeader {
        child: String,
        #[serde(rename = "from")]
        source: NodeId,
        to: NodeId,
    },
    ObserveCompletion,
    CancelBridge {
        tool: String,
    },
    ReplicateCancelIntent {
        tool: String,
        #[serde(rename = "from")]
        source: NodeId,
        to: NodeId,
    },
    MirrorCancel {
        tool: String,
    },
    ObserveCancelAck,
    RecoverBridges,
    RecoverChildRequests {
        expected_generation: u64,
        fresh_generation: u64,
    },
    CrashNode {
        node: NodeId,
        durable_reopen_premise: bool,
    },
    AdvanceClock {
        node: NodeId,
        seconds: u64,
    },
    Converge,
}

impl ModeledAction {
    /// Failure-evidence label. It carries no modeled prompt, argument or
    /// workspace bytes, so it is safe to stamp on every retained observation.
    pub fn op(&self) -> &'static str {
        match self {
            Self::PairPrincipals { .. } => "PairPrincipals",
            Self::PublishAcceptedBackgroundBridge { .. } => "PublishAcceptedBackgroundBridge",
            Self::RejectSpawnInvocation { .. } => "RejectSpawnInvocation",
            Self::ReplicateBridge { .. } => "ReplicateBridge",
            Self::MaterializeChild { .. } => "MaterializeChild",
            Self::BeginChild { .. } => "BeginChild",
            Self::AwaitChildExpiry { .. } => "AwaitChildExpiry",
            Self::ReplicateChild { .. } => "ReplicateChild",
            Self::PublishChildTerminal { .. } => "PublishChildTerminal",
            Self::ReplicateTerminalRequest { .. } => "ReplicateTerminalRequest",
            Self::ReplicateOutputSegments { .. } => "ReplicateOutputSegments",
            Self::ReplicateMessageHeader { .. } => "ReplicateMessageHeader",
            Self::ObserveCompletion => "ObserveCompletion",
            Self::CancelBridge { .. } => "CancelBridge",
            Self::ReplicateCancelIntent { .. } => "ReplicateCancelIntent",
            Self::MirrorCancel { .. } => "MirrorCancel",
            Self::ObserveCancelAck => "ObserveCancelAck",
            Self::RecoverBridges => "RecoverBridges",
            Self::RecoverChildRequests { .. } => "RecoverChildRequests",
            Self::CrashNode { .. } => "CrashNode",
            Self::AdvanceClock { .. } => "AdvanceClock",
            Self::Converge => "Converge",
        }
    }
}
