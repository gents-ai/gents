use std::path::PathBuf;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::Instant;

use anyhow::Result;
use gents_loop::tool::{BoxFuture, Tool, ToolCallEnvelope, ToolDefinition, ToolDyn, ToolError};
use gents_loop::tool_call_lifecycle::ToolOutcome;
use gents_loop::tool_effects::{
    EffectResult, ToolEffectCall, ToolEffectDispatcher, ToolEffectError,
};
use gents_pack::plugin::model_binding::{ModelBinding, ModelEndpoint, ModelResolver};
use gents_protocol::goal::GoalLimits;
use gents_protocol::request_admission::AgentRequestCreate;
use gents_protocol::request_input::RequestInput;
use gents_protocol::request_lifecycle::EnqueuedAgentRequest;
use gents_protocol::row::AgentRequestRow;
use gents_session::session::message::{RequestGoalAssignment, SessionGoal, SessionMessageGoals};
use gents_store::config_client::{ConfigAccess, ConfigApplyTxn};
use gents_tools::native::file_tools::read_tools::ReadFileTool;
use serde_json::{json, Value};

pub struct StaticTool {
    pub name: String,
    pub result: String,
}

impl StaticTool {
    pub fn new(name: impl Into<String>, result: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            result: result.into(),
        }
    }
}

impl ToolDyn for StaticTool {
    fn hidden(&self) -> bool {
        false
    }

    fn name(&self) -> String {
        self.name.clone()
    }

    fn definition<'a>(&'a self, _prompt: String) -> BoxFuture<'a, ToolDefinition> {
        Box::pin(async move {
            ToolDefinition {
                name: self.name.clone(),
                description: "test tool".to_owned(),
                parameters: json!({"type": "object"}),
            }
        })
    }

    fn call<'a>(
        &'a self,
        _args: String,
        _context: &'a ToolCallEnvelope,
    ) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(async move { Ok(self.result.clone()) })
    }
}

pub struct PendingTool {
    pub name: String,
    pub description: String,
    pub parameters: Value,
}

impl PendingTool {
    pub fn new(name: impl Into<String>, description: impl Into<String>, parameters: Value) -> Self {
        Self {
            name: name.into(),
            description: description.into(),
            parameters,
        }
    }

    pub fn slow() -> Self {
        Self::new("slow_tool", "test tool", json!({"type": "object"}))
    }

    pub fn named(name: impl Into<String>) -> Self {
        Self::new(name, "Held background work", json!({"type": "object"}))
    }
}

impl ToolDyn for PendingTool {
    fn hidden(&self) -> bool {
        false
    }

    fn name(&self) -> String {
        self.name.clone()
    }

    fn definition<'a>(&'a self, _prompt: String) -> BoxFuture<'a, ToolDefinition> {
        Box::pin(async move {
            ToolDefinition {
                name: self.name.clone(),
                description: self.description.clone(),
                parameters: self.parameters.clone(),
            }
        })
    }

    fn call<'a>(
        &'a self,
        _args: String,
        _context: &'a ToolCallEnvelope,
    ) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(std::future::pending())
    }
}

pub struct GatedTool(pub Arc<tokio::sync::Semaphore>);

impl ToolDyn for GatedTool {
    fn hidden(&self) -> bool {
        false
    }

    fn name(&self) -> String {
        "gated_tool".to_owned()
    }

    fn definition<'a>(&'a self, _prompt: String) -> BoxFuture<'a, ToolDefinition> {
        Box::pin(async {
            ToolDefinition {
                name: "gated_tool".to_owned(),
                description: "gated tool".to_owned(),
                parameters: json!({"type": "object"}),
            }
        })
    }

    fn call<'a>(
        &'a self,
        _args: String,
        _context: &'a ToolCallEnvelope,
    ) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(async {
            let _permit = self.0.acquire().await.unwrap();
            Ok("wake-completed".to_owned())
        })
    }
}

pub struct PanickingTool;

impl ToolDyn for PanickingTool {
    fn hidden(&self) -> bool {
        false
    }

    fn name(&self) -> String {
        "panicking_tool".to_owned()
    }

    fn definition<'a>(&'a self, _prompt: String) -> BoxFuture<'a, ToolDefinition> {
        Box::pin(async {
            ToolDefinition {
                name: "panicking_tool".to_owned(),
                description: "Panic regression at the background execution owner".to_owned(),
                parameters: json!({"type": "object"}),
            }
        })
    }

    fn call<'a>(
        &'a self,
        _args: String,
        _context: &'a ToolCallEnvelope,
    ) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(async { panic!("intentional background tool panic") })
    }
}

pub struct SilentChanges {
    pub waiting: Arc<tokio::sync::Notify>,
    pub closed: bool,
}

impl futures::Stream for SilentChanges {
    type Item = events::DocumentChangeBatch;

    fn poll_next(self: Pin<&mut Self>, _context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        self.waiting.notify_one();
        if self.closed {
            Poll::Ready(None)
        } else {
            Poll::Pending
        }
    }
}

/// Remembers the parent envelope each child call is dispatched under.
#[derive(Default)]
pub struct CapturedEffects(pub Mutex<Option<ToolCallEnvelope>>);

impl ToolEffectDispatcher for CapturedEffects {
    fn definitions<'a>(&'a self) -> BoxFuture<'a, Vec<ToolDefinition>> {
        Box::pin(async {
            vec![ToolDefinition {
                name: "read_file".into(),
                description: String::new(),
                parameters: json!({"type": "object"}),
            }]
        })
    }

    fn call<'a>(
        &'a self,
        parent: &'a ToolCallEnvelope,
        _ordinal: u32,
        _name: &'a str,
        _arguments: &'a str,
    ) -> BoxFuture<'a, Result<ToolOutcome, ToolEffectError>> {
        Box::pin(async move {
            *self.0.lock().unwrap() = Some(parent.clone());
            Ok(ToolOutcome::Completed("{}".to_owned()))
        })
    }
}

pub struct FileReadEffects(pub ReadFileTool, pub AtomicU32);

impl ToolEffectDispatcher for FileReadEffects {
    fn definitions<'a>(&'a self) -> BoxFuture<'a, Vec<ToolDefinition>> {
        Box::pin(async { vec![Tool::definition(&self.0, String::new()).await] })
    }

    fn call<'a>(
        &'a self,
        parent: &'a ToolCallEnvelope,
        ordinal: u32,
        name: &'a str,
        arguments: &'a str,
    ) -> BoxFuture<'a, Result<ToolOutcome, ToolEffectError>> {
        Box::pin(async move {
            assert_eq!(name, "read_file");
            assert_eq!(ordinal, self.1.fetch_add(1, Ordering::SeqCst) + 1);
            let args: Value = serde_json::from_str(arguments).unwrap();
            assert_eq!(args["raw_json"], true);
            Tool::call(&self.0, serde_json::from_value(args).unwrap(), parent)
                .await
                .map(ToolOutcome::Completed)
                .map_err(|error| ToolEffectError::Unavailable(error.to_string()))
        })
    }
}

pub struct UnreachableToolEffects;

impl ToolEffectDispatcher for UnreachableToolEffects {
    fn definitions<'a>(&'a self) -> BoxFuture<'a, Vec<ToolDefinition>> {
        Box::pin(async { unreachable!("a job step described a tool dispatcher") })
    }

    fn call<'a>(
        &'a self,
        _parent: &'a ToolCallEnvelope,
        _ordinal: u32,
        _name: &'a str,
        _arguments: &'a str,
    ) -> BoxFuture<'a, Result<ToolOutcome, ToolEffectError>> {
        Box::pin(async { unreachable!("a job step reached a tool dispatcher") })
    }
}

#[derive(Default)]
pub struct RecordingToolEffects(pub Mutex<Vec<(u32, String, Value)>>);

impl ToolEffectDispatcher for RecordingToolEffects {
    fn definitions<'a>(&'a self) -> BoxFuture<'a, Vec<ToolDefinition>> {
        Box::pin(async {
            ["query", "charts"]
                .into_iter()
                .map(|name| ToolDefinition {
                    name: name.into(),
                    description: String::new(),
                    parameters: json!({"type": "object"}),
                })
                .collect()
        })
    }

    fn call<'a>(
        &'a self,
        _parent: &'a ToolCallEnvelope,
        ordinal: u32,
        name: &'a str,
        arguments: &'a str,
    ) -> BoxFuture<'a, Result<ToolOutcome, ToolEffectError>> {
        Box::pin(async move {
            let input: Value = serde_json::from_str(arguments).unwrap();
            self.0
                .lock()
                .unwrap()
                .push((ordinal, name.to_owned(), input.clone()));
            let result = match name {
                "query" => json!({"columns": ["day", "sales"], "rows": [["Mon", 10], ["Tue", 20]]}),
                "charts" => {
                    assert_eq!(
                        input["data"],
                        json!({"columns": ["day", "sales"], "rows": [["Mon", 10], ["Tue", 20]]})
                    );
                    json!({"alt": "Sales by day", "rendered": true})
                }
                _ => {
                    return Err(ToolEffectError::Unavailable(
                        "tool is not on this request's selected surface".into(),
                    ));
                }
            };
            Ok(ToolOutcome::Completed(result.to_string()))
        })
    }
}

#[derive(Default)]
pub struct FormCheckingToolEffects(pub Mutex<Vec<Value>>);

impl ToolEffectDispatcher for FormCheckingToolEffects {
    fn call<'a>(
        &'a self,
        _parent: &'a ToolCallEnvelope,
        _ordinal: u32,
        _name: &'a str,
        arguments: &'a str,
    ) -> BoxFuture<'a, Result<ToolOutcome, ToolEffectError>> {
        Box::pin(async move {
            self.0
                .lock()
                .unwrap()
                .push(serde_json::from_str(arguments).unwrap());
            Ok(ToolOutcome::Completed("{}".into()))
        })
    }

    fn definitions<'a>(&'a self) -> BoxFuture<'a, Vec<ToolDefinition>> {
        Box::pin(async {
            [
                ("files", json!({"raw_json": {"type": "boolean"}})),
                ("named", json!({"raw_json": {"type": "string"}})),
                ("plain", json!({})),
            ]
            .into_iter()
            .map(|(name, properties)| ToolDefinition {
                name: name.into(),
                description: String::new(),
                parameters: json!({"type": "object", "properties": properties}),
            })
            .collect()
        })
    }
}

pub struct FatalToolEffects;

impl ToolEffectDispatcher for FatalToolEffects {
    fn definitions<'a>(&'a self) -> BoxFuture<'a, Vec<ToolDefinition>> {
        Box::pin(async {
            vec![ToolDefinition {
                name: "query".into(),
                description: String::new(),
                parameters: json!({"type": "object"}),
            }]
        })
    }

    fn call<'a>(
        &'a self,
        _parent: &'a ToolCallEnvelope,
        _: u32,
        _: &'a str,
        _: &'a str,
    ) -> BoxFuture<'a, Result<ToolOutcome, ToolEffectError>> {
        Box::pin(async {
            Err(ToolEffectError::Fatal(
                "durable child settlement failed".into(),
            ))
        })
    }
}

#[derive(Clone)]
pub struct RoundClock(Arc<Mutex<Instant>>);

impl RoundClock {
    pub fn new() -> Self {
        Self(Arc::new(Mutex::new(Instant::now())))
    }

    pub fn now(&self) -> Instant {
        *self.0.lock().unwrap()
    }

    pub fn advance(&self, milliseconds: u64) {
        *self.0.lock().unwrap() += std::time::Duration::from_millis(milliseconds);
    }
}

impl Default for RoundClock {
    fn default() -> Self {
        Self::new()
    }
}

pub struct SlowToolEffects(pub RoundClock);

impl ToolEffectDispatcher for SlowToolEffects {
    fn definitions<'a>(&'a self) -> BoxFuture<'a, Vec<ToolDefinition>> {
        Box::pin(async {
            vec![ToolDefinition {
                name: "slow".into(),
                description: String::new(),
                parameters: json!({"type": "object"}),
            }]
        })
    }

    fn call<'a>(
        &'a self,
        _parent: &'a ToolCallEnvelope,
        _: u32,
        _: &'a str,
        _: &'a str,
    ) -> BoxFuture<'a, Result<ToolOutcome, ToolEffectError>> {
        let clock = self.0.clone();
        Box::pin(async move {
            clock.advance(400);
            Ok(ToolOutcome::Completed("\"slow\"".into()))
        })
    }
}

pub struct AskingToolEffects(pub PathBuf, pub PathBuf, pub RoundClock);

impl ToolEffectDispatcher for AskingToolEffects {
    fn definitions<'a>(&'a self) -> BoxFuture<'a, Vec<ToolDefinition>> {
        Box::pin(async {
            vec![ToolDefinition {
                name: "read".into(),
                description: String::new(),
                parameters: json!({"type": "object"}),
            }]
        })
    }

    fn call<'a>(
        &'a self,
        _parent: &'a ToolCallEnvelope,
        _: u32,
        _: &'a str,
        _: &'a str,
    ) -> BoxFuture<'a, Result<ToolOutcome, ToolEffectError>> {
        let home = self.0.clone();
        let target = self.1.clone();
        let clock = self.2.clone();
        Box::pin(async move {
            let resolved = gents_pack::plugin::allowed::resolve(
                &target.display().to_string(),
                None,
                None,
                &home,
            )
            .unwrap();
            let request = gents_tools::plugin::approval::Request::new(
                "team/plugin",
                &resolved,
                gents_pack::pack::BindAccess::Read,
                None,
            );
            clock.advance(400);
            let allowed = gents_tools::plugin::approval::ask(
                None,
                &home,
                &request,
                gents_tools::plugin::approval::WAIT,
            )
            .await
            .unwrap();
            assert!(allowed, "the operator allowed the child's path");
            Ok(ToolOutcome::Completed("\"allowed\"".into()))
        })
    }
}

pub struct QueryToolEffects;

impl ToolEffectDispatcher for QueryToolEffects {
    fn call<'a>(
        &'a self,
        _parent: &'a ToolCallEnvelope,
        ordinal: u32,
        name: &'a str,
        arguments: &'a str,
    ) -> BoxFuture<'a, Result<ToolOutcome, ToolEffectError>> {
        Box::pin(async move {
            assert_eq!(name, "query");
            assert_eq!(arguments, "{}");
            Ok(ToolOutcome::Completed(
                json!({"ordinal": ordinal}).to_string(),
            ))
        })
    }

    fn definitions<'a>(&'a self) -> BoxFuture<'a, Vec<ToolDefinition>> {
        Box::pin(async {
            vec![ToolDefinition {
                name: "query".into(),
                description: "Reads rows.".into(),
                parameters: json!({"type": "object"}),
            }]
        })
    }
}

pub struct BatchOnlyToolEffects;

impl ToolEffectDispatcher for BatchOnlyToolEffects {
    fn call<'a>(
        &'a self,
        _: &'a ToolCallEnvelope,
        _: u32,
        _: &'a str,
        _: &'a str,
    ) -> BoxFuture<'a, Result<ToolOutcome, ToolEffectError>> {
        Box::pin(async { panic!("the host split a batch into single calls") })
    }

    fn batch<'a>(
        &'a self,
        _: &'a ToolCallEnvelope,
        calls: Vec<ToolEffectCall>,
    ) -> BoxFuture<'a, Vec<EffectResult>> {
        Box::pin(async move {
            calls
                .into_iter()
                .map(|call| {
                    Ok(ToolOutcome::Completed(
                        json!({"ordinal": call.ordinal, "name": call.name}).to_string(),
                    ))
                })
                .collect()
        })
    }

    fn definitions<'a>(&'a self) -> BoxFuture<'a, Vec<ToolDefinition>> {
        Box::pin(async { Vec::new() })
    }
}

pub struct FixedModelResolver {
    endpoint: Arc<dyn Fn() -> ModelEndpoint + Send + Sync>,
    seen: Option<Arc<Mutex<Vec<Option<String>>>>>,
}

impl FixedModelResolver {
    pub fn new<F>(endpoint: F) -> Self
    where
        F: Fn() -> ModelEndpoint + Send + Sync + 'static,
    {
        Self {
            endpoint: Arc::new(endpoint),
            seen: None,
        }
    }

    pub fn recording<F>(endpoint: F, seen: Arc<Mutex<Vec<Option<String>>>>) -> Self
    where
        F: Fn() -> ModelEndpoint + Send + Sync + 'static,
    {
        Self {
            endpoint: Arc::new(endpoint),
            seen: Some(seen),
        }
    }
}

impl ModelResolver for FixedModelResolver {
    fn resolve<'a>(
        &'a self,
        _: &'a ModelBinding,
        actor: Option<&'a identity::Did>,
    ) -> BoxFuture<'a, Result<ModelEndpoint>> {
        if let Some(seen) = &self.seen {
            seen.lock().unwrap().push(actor.map(ToString::to_string));
        }
        Box::pin(async move { Ok((self.endpoint)()) })
    }
}

pub struct PanicModelResolver(pub &'static str);

impl ModelResolver for PanicModelResolver {
    fn resolve<'a>(
        &'a self,
        _: &'a ModelBinding,
        _: Option<&'a identity::Did>,
    ) -> BoxFuture<'a, Result<ModelEndpoint>> {
        panic!("{}", self.0)
    }
}

pub struct FailingModelResolver(pub &'static str);

impl ModelResolver for FailingModelResolver {
    fn resolve<'a>(
        &'a self,
        _: &'a ModelBinding,
        _: Option<&'a identity::Did>,
    ) -> BoxFuture<'a, Result<ModelEndpoint>> {
        let message = self.0;
        Box::pin(async move { anyhow::bail!("{message}") })
    }
}

/// The Goal the Goals plugin reports for the inspected session.
pub struct ReadWorkerGoal;

#[async_trait::async_trait]
impl SessionMessageGoals for ReadWorkerGoal {
    async fn session_goal_in_txn(
        &self,
        _txn: &ConfigApplyTxn<'_>,
        _node_did: &str,
        session_id: &str,
    ) -> Result<Option<SessionGoal>> {
        Ok((session_id == "read-worker").then(|| SessionGoal {
            goal_id: "read-worker-goal".to_owned(),
            status: "active".to_owned(),
            objective: "verify shard R3a".to_owned(),
            tokens_used: Some(0),
            token_budget: Some(1000),
            active_time_seconds: Some(0),
            assignment_root_request_doc_id: None,
        }))
    }

    async fn stage_goal_backed_request_in_txn(
        &self,
        _txn: &ConfigApplyTxn<'_>,
        _objective: &str,
        _token_budget: Option<i64>,
        _limits: GoalLimits,
        _request: &AgentRequestCreate,
    ) -> Result<EnqueuedAgentRequest> {
        anyhow::bail!("session inspection stages no request")
    }

    async fn prepare_goal_input_in_txn(
        &self,
        _txn: &ConfigApplyTxn<'_>,
        _node_did: &str,
        _session_id: &str,
        _agent_did: &str,
        _requester_did: Option<&str>,
        _input: RequestInput,
    ) -> Result<RequestInput> {
        anyhow::bail!("session inspection prepares no request input")
    }

    async fn request_assignment(
        &self,
        _access: &ConfigAccess,
        _request: &AgentRequestRow,
    ) -> Result<RequestGoalAssignment> {
        anyhow::bail!("session inspection observes no request assignment")
    }
}
