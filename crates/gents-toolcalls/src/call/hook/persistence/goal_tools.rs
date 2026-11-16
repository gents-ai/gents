//! The Goal tools as the tool-call hook dispatches them: the hook owns the
//! call's lifecycle (turn sequence, dispatch adoption, running, terminal
//! write) and the Goals plugin answers the call. The composition root hands
//! the plugin's answerer to the hook.

use std::sync::Arc;

use anyhow::anyhow;
use async_trait::async_trait;
use defra_node::Node as EmbeddedNode;
use gents_inference::llm::ToolCallHookAction;
use gents_loop::output_obligation::OutputObligationCheck;
use gents_loop::tool_call_lifecycle::FailureClass;
use gents_protocol::goal_names::{
    CREATE_GOAL_TOOL_NAME, GET_GOAL_TOOL_NAME, UPDATE_GOAL_TOOL_NAME,
};
use gents_store::config_client::ConfigAccess;

use crate::call::hook::DefraSessionHook;
use crate::call::lifecycle::ToolCallLifecycle;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GoalTool {
    Get,
    Update,
    Create,
}

impl GoalTool {
    pub fn name(self) -> &'static str {
        match self {
            Self::Get => GET_GOAL_TOOL_NAME,
            Self::Update => UPDATE_GOAL_TOOL_NAME,
            Self::Create => CREATE_GOAL_TOOL_NAME,
        }
    }
}

/// One running Goal tool call, as the Goals plugin answers it.
pub struct GoalToolCall<'a> {
    node: &'a Arc<EmbeddedNode>,
    node_did: &'a str,
    session_id: &'a str,
    request_id: &'a str,
    args: &'a str,
    lifecycle: &'a ToolCallLifecycle,
    output_obligation_gate: Option<&'a dyn OutputObligationCheck>,
}

impl<'a> GoalToolCall<'a> {
    pub fn node(&self) -> &'a Arc<EmbeddedNode> {
        self.node
    }

    pub fn node_did(&self) -> &'a str {
        self.node_did
    }

    pub fn session_id(&self) -> &'a str {
        self.session_id
    }

    pub fn request_id(&self) -> &'a str {
        self.request_id
    }

    pub fn args(&self) -> &'a str {
        self.args
    }

    pub fn request_doc_id(&self) -> Option<&'a str> {
        self.lifecycle.request_doc_id()
    }

    /// The call's actor reading through ACP, without its signer.
    pub fn actor_access(&self) -> ConfigAccess {
        ConfigAccess::local_as(self.node.clone(), self.lifecycle.actor().clone())
    }

    /// The call's actor with its signer, for writes.
    pub fn access(&self) -> anyhow::Result<ConfigAccess> {
        self.lifecycle.access()
    }

    pub fn output_obligation_gate(&self) -> Option<&'a dyn OutputObligationCheck> {
        self.output_obligation_gate
    }
}

/// The result a Goal tool call terminates with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GoalToolReply {
    Complete(String),
    Fail(String, FailureClass),
}

#[async_trait]
pub trait GoalTools: Send + Sync {
    async fn answer(
        &self,
        tool: GoalTool,
        call: &GoalToolCall<'_>,
    ) -> anyhow::Result<GoalToolReply>;
}

/// The Goal tools of a hook that has no Goals plugin attached: every call
/// fails.
#[derive(Debug, Default, Clone, Copy)]
pub struct NoGoalTools;

#[async_trait]
impl GoalTools for NoGoalTools {
    async fn answer(
        &self,
        _tool: GoalTool,
        _call: &GoalToolCall<'_>,
    ) -> anyhow::Result<GoalToolReply> {
        Err(anyhow!("no Goals plugin is attached"))
    }
}

impl DefraSessionHook {
    pub(super) async fn persist_goal_tool_call(
        &self,
        tool: GoalTool,
        tool_call_id: Option<String>,
        internal_call_id: &str,
        args: &str,
    ) -> anyhow::Result<ToolCallHookAction> {
        let (session_id, request_id, deadline_at, _sequence) =
            self.ensure_assistant_turn_sequence().await?;
        let mut lifecycle = self
            .adopt_accepted_tool_dispatch(
                internal_call_id,
                tool_call_id.as_deref(),
                &request_id,
                &session_id,
                tool.name(),
                args,
                deadline_at,
                gents_protocol::tool_call::AwaitMode::Foreground,
            )
            .await?;
        lifecycle.start_running().await?;
        let reply = self
            .goal_tools
            .answer(
                tool,
                &GoalToolCall {
                    node: &self.node,
                    node_did: &self.node_did,
                    session_id: &session_id,
                    request_id: &request_id,
                    args,
                    lifecycle: &lifecycle,
                    output_obligation_gate: self.output_obligation_gate.as_deref(),
                },
            )
            .await?;
        match reply {
            GoalToolReply::Complete(result) => {
                self.complete_control_tool_call(&mut lifecycle, tool.name(), result)
                    .await
            }
            GoalToolReply::Fail(result, failure) => {
                self.fail_control_tool_call(&mut lifecycle, tool.name(), result, failure)
                    .await
            }
        }
    }
}
