use serde::Deserialize;

pub type NodeId = String;

#[derive(Debug, Deserialize)]
pub struct Scenario {
    pub name: String,
    pub actions: Vec<Action>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "op")]
pub enum Action {
    OperatorWritePairing {
        node: NodeId,
        peer: NodeId,
        collections: Vec<String>,
    },
    WriteParentToolCall {
        node: NodeId,
        parent_request_id: String,
        parent_tool_call_id: String,
        child_request_id: String,
        behavior_id: String,
        unclaimed_deadline_at: Option<String>,
    },
    WriteAgentRequest {
        node: NodeId,
        request_id: String,
        agent_did: String,
        behavior_id: String,
        state: String,
        #[serde(default)]
        caused_by_parent_request_id: Option<String>,
        #[serde(default)]
        caused_by_parent_tool_call_id: Option<String>,
    },
    ReplicateDoc {
        from: NodeId,
        to: NodeId,
        collection: String,
        doc_id: String,
    },
    TerminalizeChildOnB {
        request_id: String,
        terminal: String,
        #[serde(default)]
        final_response: Option<String>,
    },
    CancelParentOnA {
        parent_request_id: String,
        parent_tool_call_id: String,
    },
    RunBackgroundCompletionObserverOnA,
    RunCancelMirrorObserverOnB,
    RunUnclaimedSpawnReconcilerOnA,
    RunCancelAckObserverOnA,
    RunRecoverySweepOn {
        node: NodeId,
    },
    Crash {
        node: NodeId,
    },
    AdvanceClockOn {
        node: NodeId,
        seconds: u64,
    },
    WaitForConvergence {
        timeout_secs: u64,
    },
}
