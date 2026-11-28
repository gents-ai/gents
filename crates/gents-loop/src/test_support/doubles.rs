use std::collections::VecDeque;
use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use chrono::{DateTime, Utc};
use futures::StreamExt;
use gents_protocol::message::Message;
use gents_protocol::plugin::{PluginExecutionReceipt, PluginExecutionVerdict};
use rig::completion::{CompletionError, CompletionModel, CompletionRequest, CompletionResponse};
use rig::http_client::{
    self, HttpClientExt, LazyBody, MultipartForm, Request, Response, StreamingResponse,
};
use rig::streaming::{RawStreamingChoice, RawStreamingToolCall, StreamingCompletionResponse};
use rig::wasm_compat::WasmCompatSend;
use serde_json::{json, Value};
use tokio::sync::{Mutex as AsyncMutex, Notify, Semaphore};
use tokio_util::sync::CancellationToken;

use crate::live_output::{CanonicalOutputAppender, DeferredToolWrites, LiveOutputError};
use crate::loop_stream::{ReboundTools, ToolRebinding};
use crate::provider_call::{ForCall, ProviderCall};
use crate::rendered_request::scope::{test_scope, RequestCaptureScope};
use crate::rendered_request::transport::RenderedRequestCapturingHttpClient;
use crate::rendered_request::{
    AssemblyBuildPath, AssemblyTrace, RenderedRequestCaptureSink, RenderedRequestContext,
};
use crate::session_hook::{ForegroundCall, ForegroundResult, SessionHook};
use crate::test_support::fixed_tool::FixedTool;
use crate::tool::{
    BoxFuture, Tool, ToolCallEnvelope, ToolDefinition, ToolDispatchResult, ToolDyn, ToolError,
    ToolFootprint, ToolRequestContext,
};
use crate::tool_call_lifecycle::{FailureClass, ToolOutcome};
use crate::{HookAction, ToolCallHookAction};

pub(crate) struct DropFlag(pub(crate) Arc<AtomicBool>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

pub(crate) fn test_capture_context() -> RenderedRequestContext {
    RenderedRequestContext {
        request_doc_id: "doc-1".to_string(),
        request_commit_cid: "bafy-request-commit".to_string(),
        request_id: "req-1".to_string(),
        node_did: "did:key:agent".to_string(),
        requester_did: "did:key:requester".to_string(),
        agent_did: "behavior".to_string(),
        session_id: "session".to_string(),
        model_name: "test-model".to_string(),
        provider_family: None,
    }
}

pub(crate) fn noop_capture_sink() -> RenderedRequestCaptureSink {
    Arc::new(|_| Box::pin(async { Ok(()) }))
}

pub(crate) fn test_capture_scope() -> Arc<RequestCaptureScope> {
    test_scope(test_capture_context(), noop_capture_sink())
}

pub(crate) fn empty_trace() -> AssemblyTrace {
    AssemblyTrace::from_effective_messages(AssemblyBuildPath::Budgeted, Vec::new())
}

fn simple_tool_def(
    name: &'static str,
    description: &'static str,
) -> BoxFuture<'static, ToolDefinition> {
    Box::pin(async move {
        ToolDefinition {
            name: name.to_string(),
            description: description.to_string(),
            parameters: json!({"type": "object"}),
        }
    })
}

#[cfg(feature = "native")]
#[derive(serde::Deserialize, serde::Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ResponsesArgs {
    pub(crate) text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) mode: Option<ResponsesMode>,
}

#[cfg(feature = "native")]
#[derive(serde::Deserialize, serde::Serialize)]
pub(crate) enum ResponsesMode {
    Normal,
}

#[cfg(feature = "native")]
pub(crate) struct ResponsesTool(pub(crate) Arc<Mutex<Vec<String>>>);

#[cfg(feature = "native")]
impl Tool for ResponsesTool {
    const NAME: &'static str = "echo";
    type Error = ToolError;
    type Args = ResponsesArgs;
    type Output = String;

    async fn definition(&self, _: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.into(),
            description: String::new(),
            parameters: json!({
                "type": "object",
                "required": ["text"],
                "properties": {
                    "text": {"type": "string"},
                    "mode": {"type": "string", "enum": ["Normal"]}
                },
                "additionalProperties": false
            }),
        }
    }

    async fn call(
        &self,
        args: Self::Args,
        _context: &ToolCallEnvelope,
    ) -> Result<String, Self::Error> {
        self.0
            .lock()
            .unwrap()
            .push(serde_json::to_string(&args).unwrap());
        Ok(args.text.to_uppercase())
    }
}

pub(crate) struct EchoTool {
    pub(crate) calls: Arc<Mutex<Vec<String>>>,
    pub(crate) policy_allows: bool,
}

#[derive(serde::Deserialize)]
pub(crate) struct EchoArgs {
    pub(crate) text: String,
}

impl Tool for EchoTool {
    const NAME: &'static str = "echo";
    type Error = ToolError;
    type Args = EchoArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: "Echoes its input, uppercased.".to_string(),
            parameters: json!({
                "type": "object",
                "properties": { "text": { "type": "string" } },
                "required": ["text"],
            }),
        }
    }

    fn admit(&self, _args: &Self::Args, _context: &ToolCallEnvelope) -> Result<(), Self::Error> {
        if self.policy_allows {
            Ok(())
        } else {
            Err(ToolError::ReportedFailure {
                class: FailureClass::PolicyDenied,
                text: "echo denied by tool policy".into(),
            })
        }
    }

    fn into_dyn_error(error: Self::Error) -> ToolError {
        error
    }

    async fn call(
        &self,
        args: Self::Args,
        _context: &ToolCallEnvelope,
    ) -> Result<Self::Output, Self::Error> {
        self.calls
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push(args.text.clone());
        Ok(args.text.to_uppercase())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RecordedCall {
    CompletionCall { prompt: String },
    ToolCall { tool_name: String },
    AdmissionRejected { tool_name: String, outcome: String },
    ToolResult { tool_name: String, outcome: String },
}

#[derive(Default)]
pub(crate) struct AdmissionGate {
    pub(crate) entered: Notify,
    pub(crate) release: Notify,
}

#[derive(Default)]
pub(crate) struct SettlementGate {
    pub(crate) entered: Notify,
    pub(crate) dropped: AtomicBool,
    pub(crate) caller: Mutex<Option<&'static str>>,
    pub(crate) read_scope: Option<fn() -> Option<&'static str>>,
}

#[derive(Clone, Default)]
pub(crate) struct RecordingHook {
    pub(crate) log: Arc<Mutex<Vec<RecordedCall>>>,
    pub(crate) deny_dispatch: bool,
    pub(crate) admission_gate: Option<Arc<AdmissionGate>>,
    pub(crate) settlement_gate: Option<Arc<SettlementGate>>,
}

impl RecordingHook {
    fn log(&self, call: RecordedCall) {
        self.log
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push(call);
    }

    pub(crate) fn calls(&self) -> Vec<RecordedCall> {
        self.log
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone()
    }
}

#[async_trait]
impl SessionHook for RecordingHook {
    type Error = std::convert::Infallible;

    async fn on_completion_call_with_context(
        &self,
        prompt: &Message,
        _history: &[Message],
        _context: Option<&Message>,
    ) -> HookAction {
        self.log(RecordedCall::CompletionCall {
            prompt: prompt.rag_text().unwrap_or_default(),
        });
        HookAction::Continue
    }

    async fn on_tool_call(
        &self,
        tool_name: &str,
        _tool_call_id: Option<String>,
        _internal_call_id: &str,
        _args: &str,
        _context: &ToolCallEnvelope,
    ) -> ToolCallHookAction {
        self.log(RecordedCall::ToolCall {
            tool_name: tool_name.to_string(),
        });
        if let Some(gate) = &self.admission_gate {
            gate.entered.notify_one();
            gate.release.notified().await;
        }
        if self.deny_dispatch {
            ToolCallHookAction::Terminate {
                reason: "dispatch not acknowledged".to_owned(),
            }
        } else {
            ToolCallHookAction::Continue
        }
    }

    async fn on_tool_admission_rejected(
        &self,
        tool_name: &str,
        _tool_call_id: Option<String>,
        _internal_call_id: &str,
        _args: &str,
        outcome: &ToolOutcome,
    ) -> HookAction {
        self.log(RecordedCall::AdmissionRejected {
            tool_name: tool_name.to_string(),
            outcome: outcome.model_facing_text().to_string(),
        });
        HookAction::Continue
    }

    async fn on_tool_result(
        &self,
        tool_name: &str,
        _tool_call_id: Option<String>,
        _internal_call_id: &str,
        _args: &str,
        outcome: &ToolOutcome,
    ) -> crate::ToolResultHookAction {
        if let Some(gate) = &self.settlement_gate {
            struct Dropped(Arc<SettlementGate>);
            impl Drop for Dropped {
                fn drop(&mut self) {
                    self.0.dropped.store(true, Ordering::SeqCst);
                }
            }
            let _guard = Dropped(gate.clone());
            *gate.caller.lock().unwrap() = gate.read_scope.and_then(|read| read());
            gate.entered.notify_one();
            std::future::pending::<()>().await;
        }
        self.log(RecordedCall::ToolResult {
            tool_name: tool_name.to_string(),
            outcome: outcome.model_facing_text().to_string(),
        });
        crate::ToolResultHookAction::Continue
    }

    async fn foreground_live_output_writer(
        &self,
        internal_call_id: &str,
    ) -> crate::live_output::LiveToolOutputWriter {
        crate::live_output::LiveToolOutputRegistry::default()
            .writer_for(internal_call_id.to_string())
            .await
    }

    async fn session_id(&self) -> Option<String> {
        Some("in-memory-session".to_string())
    }

    async fn register_stream_tool_call_identity(
        &self,
        _internal_call_id: &str,
        _result_id: &str,
        _call_id: Option<&str>,
    ) {
    }
}

pub(crate) struct GatedChunk {
    pub(crate) open: Arc<AtomicBool>,
    pub(crate) emitted: Arc<AtomicBool>,
}

impl futures::Stream for GatedChunk {
    type Item = Result<RawStreamingChoice<()>, CompletionError>;

    fn poll_next(
        self: Pin<&mut Self>,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Self::Item>> {
        if !self.open.load(Ordering::SeqCst) || self.emitted.swap(true, Ordering::SeqCst) {
            return std::task::Poll::Pending;
        }
        std::task::Poll::Ready(Some(Ok(RawStreamingChoice::Message(
            "received partial".into(),
        ))))
    }
}

#[derive(Clone)]
pub(crate) struct GatedModel {
    pub(crate) open: Arc<AtomicBool>,
    pub(crate) emitted: Arc<AtomicBool>,
    pub(crate) call: ProviderCall,
}

impl ForCall for GatedModel {
    fn for_call(&self, call: &ProviderCall) -> Self {
        Self {
            call: call.clone(),
            ..self.clone()
        }
    }
}

#[allow(refining_impl_trait)]
impl CompletionModel for GatedModel {
    type Response = ();
    type StreamingResponse = ();
    type Client = ();

    fn make(_: &Self::Client, _: impl Into<String>) -> Self {
        unreachable!()
    }

    async fn completion(
        &self,
        _: CompletionRequest,
    ) -> Result<CompletionResponse<Self::Response>, CompletionError> {
        unreachable!()
    }

    async fn stream(
        &self,
        _: CompletionRequest,
    ) -> Result<StreamingCompletionResponse<Self::StreamingResponse>, CompletionError> {
        let _ = crate::rendered_request::scope::claim_pending(self.call.capture.as_ref());
        Ok(StreamingCompletionResponse::stream(Box::pin(GatedChunk {
            open: self.open.clone(),
            emitted: self.emitted.clone(),
        })))
    }
}

pub(crate) struct PendingTool {
    pub(crate) name: &'static str,
    pub(crate) description: &'static str,
    pub(crate) dropped: Option<Arc<AtomicBool>>,
    pub(crate) observed: Option<Arc<Mutex<Option<String>>>>,
}

impl PendingTool {
    pub(crate) fn pending() -> Self {
        Self {
            name: "pending",
            description: "test tool",
            dropped: None,
            observed: None,
        }
    }
}

impl ToolDyn for PendingTool {
    fn hidden(&self) -> bool {
        false
    }

    fn name(&self) -> String {
        self.name.to_string()
    }

    fn definition<'a>(&'a self, _: String) -> BoxFuture<'a, ToolDefinition> {
        simple_tool_def(self.name, self.description)
    }

    fn call<'a>(
        &'a self,
        _: String,
        context: &'a ToolCallEnvelope,
    ) -> BoxFuture<'a, Result<String, ToolError>> {
        let marker = self.dropped.clone().map(DropFlag);
        let observed = self.observed.clone();
        Box::pin(async move {
            let _marker = marker;
            if let Some(observed) = observed {
                *observed.lock().unwrap() = context.runtime().unwrap().request_id;
            }
            std::future::pending().await
        })
    }
}

pub(crate) struct ReadEchoTool(pub(crate) Arc<AtomicUsize>);

impl ToolDyn for ReadEchoTool {
    fn hidden(&self) -> bool {
        false
    }

    fn name(&self) -> String {
        "echo".into()
    }

    fn definition<'a>(&'a self, _: String) -> BoxFuture<'a, ToolDefinition> {
        simple_tool_def("echo", "")
    }

    fn call<'a>(
        &'a self,
        _: String,
        _context: &'a ToolCallEnvelope,
    ) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(async {
            self.0.fetch_add(1, Ordering::SeqCst);
            Ok("done".into())
        })
    }

    fn footprint(&self, _: &str, _: &ToolCallEnvelope) -> ToolFootprint {
        ToolFootprint::Read
    }
}

#[derive(Debug, Default)]
pub(crate) struct MemoryAppender(pub(crate) Mutex<String>);

impl CanonicalOutputAppender for MemoryAppender {
    fn append<'a>(
        &'a self,
        text: &'a str,
    ) -> Pin<Box<dyn Future<Output = Result<std::ops::Range<u64>, LiveOutputError>> + Send + 'a>>
    {
        Box::pin(async move {
            let mut source = self.0.lock().unwrap();
            let start = source.len() as u64;
            source.push_str(text);
            Ok(start..source.len() as u64)
        })
    }
}

pub(crate) struct RebindingEchoTool {
    pub(crate) rebind_at: Option<u64>,
    pub(crate) calls: Arc<AtomicU64>,
    pub(crate) staged: Arc<Mutex<Option<ReboundTools>>>,
}

impl RebindingEchoTool {
    pub(crate) fn new(rebind_at: Option<u64>) -> Self {
        Self {
            rebind_at,
            calls: Default::default(),
            staged: Default::default(),
        }
    }

    pub(crate) fn rebinding(&self) -> ToolRebinding {
        let staged = self.staged.clone();
        Arc::new(move || {
            let staged = staged.lock().unwrap().take();
            Box::pin(async move { staged })
        })
    }
}

impl ToolDyn for RebindingEchoTool {
    fn hidden(&self) -> bool {
        false
    }

    fn name(&self) -> String {
        "echo".into()
    }

    fn definition<'a>(&'a self, _: String) -> BoxFuture<'a, ToolDefinition> {
        simple_tool_def("echo", "echo")
    }

    fn call<'a>(
        &'a self,
        _: String,
        _context: &'a ToolCallEnvelope,
    ) -> BoxFuture<'a, Result<String, ToolError>> {
        let turn = self.calls.fetch_add(1, Ordering::SeqCst);
        if self.rebind_at == Some(turn) {
            *self.staged.lock().unwrap() = Some(ReboundTools {
                tools: Arc::new(vec![
                    Box::new(RebindingEchoTool {
                        rebind_at: None,
                        calls: self.calls.clone(),
                        staged: self.staged.clone(),
                    }) as Box<dyn ToolDyn>,
                    Box::new(FixedTool::new("rebound", "REBOUND")),
                ]),
                preamble: None,
                configuration_guard: None,
            });
        }
        Box::pin(async { Ok("ECHOED".into()) })
    }
}

pub(crate) struct ParallelProbeTool {
    pub(crate) calls: Arc<AtomicUsize>,
    pub(crate) pending_second: bool,
}

impl ToolDyn for ParallelProbeTool {
    fn hidden(&self) -> bool {
        false
    }

    fn name(&self) -> String {
        "probe".into()
    }

    fn definition<'a>(&'a self, _: String) -> BoxFuture<'a, ToolDefinition> {
        simple_tool_def("probe", "")
    }

    fn call<'a>(
        &'a self,
        args: String,
        _context: &'a ToolCallEnvelope,
    ) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(async move {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.pending_second && args.contains('1') {
                std::future::pending().await
            } else {
                Ok(args)
            }
        })
    }
}

pub(crate) struct ParallelHook {
    pub(crate) actions: Vec<&'static str>,
    pub(crate) settled: AsyncMutex<Vec<(usize, bool)>>,
    pub(crate) terminate_id: Option<usize>,
    pub(crate) cancelled_terminates: bool,
    pub(crate) cancel_token: bool,
    pub(crate) request_tokens: AsyncMutex<Vec<CancellationToken>>,
}

#[async_trait]
impl SessionHook for ParallelHook {
    type Error = std::convert::Infallible;

    async fn run_foreground_tool(
        &self,
        _: &str,
        _: &str,
        call: ForegroundCall,
        context: &ToolCallEnvelope,
    ) -> ForegroundResult {
        let token = CancellationToken::new();
        if self.cancel_token {
            token.cancel();
        }
        self.request_tokens.lock().await.push(token.clone());
        let runtime = context.runtime().map(|mut runtime| {
            runtime.cancellation_token = token.clone();
            runtime
        });
        let hook = context
            .clone()
            .with_runtime(Some(runtime.unwrap_or_else(|| {
                crate::tool_call_lifecycle::runtime::CurrentToolRuntimeContext::entered(
                    None,
                    crate::tool_call_lifecycle::runtime::ToolScopeEntry {
                        identity: None,
                        deadline_at: None,
                        cancellation_token: token,
                        workspace_cwd: None,
                        live_output: None,
                        session_id: None,
                        correlation: None,
                        source_fields: Default::default(),
                        background: false,
                    },
                )
            })));
        ForegroundResult::Settled(call(hook, None).await)
    }

    async fn on_completion_call_with_context(
        &self,
        _: &Message,
        _: &[Message],
        _: Option<&Message>,
    ) -> HookAction {
        HookAction::Continue
    }

    async fn on_tool_call(
        &self,
        _: &str,
        _: Option<String>,
        id: &str,
        _: &str,
        _: &ToolCallEnvelope,
    ) -> ToolCallHookAction {
        match self.actions[id.parse::<usize>().unwrap()] {
            "skip" => ToolCallHookAction::Skip {
                reason: "skipped".into(),
            },
            "terminate" => ToolCallHookAction::Terminate {
                reason: "stopped".into(),
            },
            _ => ToolCallHookAction::Continue,
        }
    }

    async fn on_tool_admission_rejected(
        &self,
        _: &str,
        _: Option<String>,
        _: &str,
        _: &str,
        _: &ToolOutcome,
    ) -> HookAction {
        HookAction::Continue
    }

    async fn on_tool_result(
        &self,
        _: &str,
        _: Option<String>,
        id: &str,
        _: &str,
        outcome: &ToolOutcome,
    ) -> crate::ToolResultHookAction {
        let id = id.parse::<usize>().unwrap();
        self.settled
            .lock()
            .await
            .push((id, matches!(outcome, ToolOutcome::Cancelled)));
        if self.cancelled_terminates && matches!(outcome, ToolOutcome::Cancelled) {
            crate::ToolResultHookAction::Terminate {
                reason: "cancelled".into(),
            }
        } else if self.terminate_id == Some(id) {
            crate::ToolResultHookAction::Terminate {
                reason: "stopped".into(),
            }
        } else {
            crate::ToolResultHookAction::Continue
        }
    }

    async fn foreground_live_output_writer(
        &self,
        id: &str,
    ) -> crate::live_output::LiveToolOutputWriter {
        crate::live_output::LiveToolOutputRegistry::default()
            .writer_for(id.to_string())
            .await
    }

    async fn session_id(&self) -> Option<String> {
        None
    }

    async fn register_stream_tool_call_identity(&self, _: &str, _: &str, _: Option<&str>) {}
}

pub(crate) struct HeldFirstTool {
    pub(crate) entered: Arc<Notify>,
}

impl ToolDyn for HeldFirstTool {
    fn hidden(&self) -> bool {
        false
    }

    fn name(&self) -> String {
        "probe".into()
    }

    fn definition<'a>(&'a self, _: String) -> BoxFuture<'a, ToolDefinition> {
        simple_tool_def("probe", "")
    }

    fn call<'a>(
        &'a self,
        args: String,
        _context: &'a ToolCallEnvelope,
    ) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(async move {
            if args.contains('0') {
                self.entered.notify_one();
                std::future::pending().await
            } else {
                self.entered.notified().await;
                Ok(args)
            }
        })
    }
}

#[derive(Clone, Default)]
pub(crate) struct WireAttempt {
    pub(crate) hold_headers: bool,
    pub(crate) events: Vec<(Duration, String)>,
    pub(crate) hang: bool,
    pub(crate) finalize_delay: Duration,
}

impl std::fmt::Debug for WireAttempt {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("WireAttempt")
    }
}

impl WireAttempt {
    pub(crate) fn event(mut self, after: Duration, data: Value) -> Self {
        self.events.push((after, data.to_string()));
        self
    }

    pub(crate) fn text(self, after: Duration, text: &str) -> Self {
        self.event(after, json!({"type": "text_delta", "text": text}))
    }

    pub(crate) fn stop(self, after: Duration) -> Self {
        self.event(after, json!({"type": "message_stop"}))
    }

    pub(crate) fn completes(text: &str) -> Self {
        Self::default()
            .text(Duration::ZERO, text)
            .stop(Duration::ZERO)
    }

    pub(crate) fn hangs(mut self) -> Self {
        self.hang = true;
        self
    }
}

struct DropCounter(Arc<AtomicUsize>);

impl Drop for DropCounter {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ScriptedWire {
    pub(crate) attempts: Arc<Mutex<VecDeque<WireAttempt>>>,
    pub(crate) sends: Arc<AtomicUsize>,
    pub(crate) dropped_bodies: Arc<AtomicUsize>,
}

impl ScriptedWire {
    pub(crate) fn new(attempts: Vec<WireAttempt>) -> Self {
        Self {
            attempts: Arc::new(Mutex::new(attempts.into())),
            ..Self::default()
        }
    }
}

#[expect(
    clippy::manual_async_fn,
    reason = "the trait requires a 'static future, which an async fn borrowing &self cannot provide"
)]
impl HttpClientExt for ScriptedWire {
    fn send<T, U>(
        &self,
        _req: Request<T>,
    ) -> impl Future<Output = http_client::Result<Response<LazyBody<U>>>> + WasmCompatSend + 'static
    where
        T: Into<Bytes> + WasmCompatSend,
        U: From<Bytes> + WasmCompatSend + 'static,
    {
        async { unreachable!("the owned loop only streams") }
    }

    fn send_multipart<U>(
        &self,
        _req: Request<MultipartForm>,
    ) -> impl Future<Output = http_client::Result<Response<LazyBody<U>>>> + WasmCompatSend + 'static
    where
        U: From<Bytes> + WasmCompatSend + 'static,
    {
        async { unreachable!("the owned loop only streams") }
    }

    fn send_streaming<T>(
        &self,
        _req: Request<T>,
    ) -> impl Future<Output = http_client::Result<StreamingResponse>> + WasmCompatSend
    where
        T: Into<Bytes>,
    {
        self.sends.fetch_add(1, Ordering::SeqCst);
        let attempt = self
            .attempts
            .lock()
            .unwrap()
            .pop_front()
            .expect("provider called more times than scripted");
        let dropped = DropCounter(self.dropped_bodies.clone());
        async move {
            if attempt.hold_headers {
                let _dropped = dropped;
                std::future::pending::<()>().await;
                unreachable!("held headers never arrive");
            }
            let hang = attempt.hang;
            let body = async_stream::stream! {
                let _dropped = dropped;
                for (after, data) in attempt.events {
                    tokio::time::sleep(after).await;
                    yield Ok::<Bytes, http_client::Error>(Bytes::from(format!("data: {data}\n\n")));
                }
                if hang {
                    std::future::pending::<()>().await;
                }
            };
            let body: rig::http_client::sse::BoxedStream = Box::pin(body);
            Ok(Response::builder().status(200).body(body)?)
        }
    }
}

#[derive(Clone)]
pub(crate) struct WireModel {
    client: RenderedRequestCapturingHttpClient<ScriptedWire>,
    finalize_delays: Arc<Mutex<VecDeque<Duration>>>,
    call: ProviderCall,
}

impl WireModel {
    pub(crate) fn new(wire: &ScriptedWire, attempts: &[WireAttempt]) -> Self {
        Self {
            call: Default::default(),
            client: RenderedRequestCapturingHttpClient::new(wire.clone()),
            finalize_delays: Arc::new(Mutex::new(
                attempts
                    .iter()
                    .map(|attempt| attempt.finalize_delay)
                    .collect(),
            )),
        }
    }
}

impl ForCall for WireModel {
    fn for_call(&self, call: &ProviderCall) -> Self {
        Self {
            call: call.clone(),
            ..self.clone()
        }
    }
}

#[allow(refining_impl_trait)]
impl CompletionModel for WireModel {
    type Response = ();
    type StreamingResponse = ();
    type Client = ();

    fn make(_client: &Self::Client, _model: impl Into<String>) -> Self {
        unreachable!("constructed directly")
    }

    async fn completion(
        &self,
        _request: CompletionRequest,
    ) -> Result<CompletionResponse<()>, CompletionError> {
        unreachable!("the owned loop only ever calls stream()")
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<StreamingCompletionResponse<()>, CompletionError> {
        let finalize_delay = self
            .finalize_delays
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_default();
        let body = json!({
            "model": "wire-model",
            "stream": true,
            "messages": [{"role": "user", "content": "hello"}],
        });
        let request = Request::builder()
            .method("POST")
            .uri("https://provider.test/v1/messages")
            .body(Bytes::from(body.to_string()))
            .expect("request");
        let response =
            crate::rig_compat::http::Transport::send_streaming(&self.client, &self.call, request)
                .await?;
        let mut body = response.into_body();
        let items = async_stream::stream! {
            let mut buffer = Vec::new();
            while let Some(chunk) = body.next().await {
                let chunk = match chunk {
                    Ok(chunk) => chunk,
                    Err(error) => {
                        yield Err(CompletionError::HttpError(error));
                        return;
                    }
                };
                buffer.extend_from_slice(&chunk);
                while let Some(end) = buffer.windows(2).position(|window| window == b"\n\n") {
                    let event: Vec<u8> = buffer.drain(..end + 2).collect();
                    let data = std::str::from_utf8(&event).unwrap().trim();
                    let value: Value =
                        serde_json::from_str(data.strip_prefix("data: ").unwrap()).unwrap();
                    match value["type"].as_str() {
                        Some("text_delta") => yield Ok(RawStreamingChoice::Message(
                            value["text"].as_str().unwrap().to_string(),
                        )),
                        Some("tool_use") => yield Ok(RawStreamingChoice::ToolCall(
                            RawStreamingToolCall {
                                id: "call_1".into(),
                                internal_call_id: "call_1".into(),
                                call_id: Some("call_1".into()),
                                name: "echo".into(),
                                arguments: json!({"text": "hi"}),
                                signature: None,
                                additional_params: None,
                            },
                        )),
                        Some("message_stop") => {
                            tokio::time::sleep(finalize_delay).await;
                            yield Ok(RawStreamingChoice::FinalResponse(()));
                        }
                        _ => {}
                    }
                }
            }
        };
        Ok(StreamingCompletionResponse::stream(Box::pin(items)))
    }
}

pub(crate) type EffectBatchEvents = Arc<Mutex<Vec<String>>>;

pub(crate) struct EffectBatchProbe(pub(crate) EffectBatchEvents);

impl ToolDyn for EffectBatchProbe {
    fn hidden(&self) -> bool {
        false
    }

    fn name(&self) -> String {
        "probe".into()
    }

    fn definition<'a>(&'a self, _: String) -> BoxFuture<'a, ToolDefinition> {
        simple_tool_def("probe", "")
    }

    fn footprint(&self, args: &str, _: &ToolCallEnvelope) -> ToolFootprint {
        if serde_json::from_str::<Value>(args).unwrap()["write"] == true {
            ToolFootprint::Barrier
        } else {
            ToolFootprint::Read
        }
    }

    fn call<'a>(
        &'a self,
        args: String,
        _: &'a ToolCallEnvelope,
    ) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(async move {
            let args: Value = serde_json::from_str(&args).unwrap();
            let id = args["id"].as_u64().unwrap();
            self.0.lock().unwrap().push(format!("start:{id}"));
            tokio::time::sleep(Duration::from_millis(args["ms"].as_u64().unwrap_or(1000))).await;
            self.0.lock().unwrap().push(format!("end:{id}"));
            Ok(id.to_string())
        })
    }
}

pub(crate) struct EffectBatchHook {
    pub(crate) events: EffectBatchEvents,
    pub(crate) approval: Semaphore,
    pub(crate) block: bool,
    pub(crate) terminate: Option<u32>,
    pub(crate) terminate_result: Option<u32>,
    pub(crate) replace_result: Option<(u32, ToolOutcome)>,
    pub(crate) skip: Option<u32>,
    pub(crate) fail_reservation: Option<u32>,
    pub(crate) fail_finish: Option<u32>,
}

impl Default for EffectBatchHook {
    fn default() -> Self {
        Self {
            events: Default::default(),
            approval: Semaphore::new(0),
            block: false,
            terminate: None,
            terminate_result: None,
            replace_result: None,
            skip: None,
            fail_reservation: None,
            fail_finish: None,
        }
    }
}

#[async_trait]
impl SessionHook for EffectBatchHook {
    type Error = std::convert::Infallible;

    async fn admit_tool_effect(
        &self,
        _: &str,
        ordinal: u32,
        _: &str,
        _: &str,
        _: &ToolCallEnvelope,
    ) -> Result<String, String> {
        self.events
            .lock()
            .unwrap()
            .push(format!("reserve:{ordinal}"));
        if self.fail_reservation == Some(ordinal) {
            return Err(format!("reservation {ordinal} unavailable"));
        }
        Ok(ordinal.to_string())
    }

    async fn on_completion_call_with_context(
        &self,
        _: &Message,
        _: &[Message],
        _: Option<&Message>,
    ) -> HookAction {
        HookAction::Continue
    }

    async fn on_tool_call(
        &self,
        _: &str,
        _: Option<String>,
        id: &str,
        _: &str,
        _: &ToolCallEnvelope,
    ) -> ToolCallHookAction {
        self.events.lock().unwrap().push(format!("ask:{id}"));
        if self.block {
            self.approval.acquire().await.unwrap().forget();
        }
        self.events.lock().unwrap().push(format!("approved:{id}"));
        let id: u32 = id.parse().unwrap();
        if self.terminate == Some(id) {
            ToolCallHookAction::Terminate {
                reason: "approval terminated".into(),
            }
        } else if self.skip == Some(id) {
            ToolCallHookAction::Skip {
                reason: "denied".into(),
            }
        } else {
            ToolCallHookAction::Continue
        }
    }

    async fn on_tool_admission_rejected(
        &self,
        _: &str,
        _: Option<String>,
        _: &str,
        _: &str,
        _: &ToolOutcome,
    ) -> HookAction {
        HookAction::Continue
    }

    async fn on_tool_result(
        &self,
        _: &str,
        _: Option<String>,
        id: &str,
        _: &str,
        _: &ToolOutcome,
    ) -> crate::ToolResultHookAction {
        self.events.lock().unwrap().push(format!("result:{id}"));
        if self.terminate_result == id.parse().ok() {
            crate::ToolResultHookAction::Terminate {
                reason: "settlement failed".into(),
            }
        } else if let Some((target, replacement)) = &self.replace_result {
            if id.parse().ok() == Some(*target) {
                crate::ToolResultHookAction::replace_outcome(replacement.clone())
            } else {
                crate::ToolResultHookAction::Continue
            }
        } else {
            crate::ToolResultHookAction::Continue
        }
    }

    async fn finish_tool_effect(
        &self,
        id: &str,
        interruption: Option<&ToolOutcome>,
    ) -> Result<(), String> {
        self.events
            .lock()
            .unwrap()
            .push(format!("finish:{id}:{}", interruption.is_some()));
        if id.parse().ok() == self.fail_finish {
            return Err("finish failed".into());
        }
        Ok(())
    }

    async fn foreground_live_output_writer(
        &self,
        id: &str,
    ) -> crate::live_output::LiveToolOutputWriter {
        crate::live_output::LiveToolOutputRegistry::default()
            .writer_for(id)
            .await
    }

    async fn session_id(&self) -> Option<String> {
        None
    }

    async fn register_stream_tool_call_identity(&self, _: &str, _: &str, _: Option<&str>) {}
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct ArgumentFixture {
    pub(crate) text: String,
    pub(crate) mode: Option<ArgumentMode>,
}

#[derive(Debug, serde::Deserialize)]
pub(crate) enum ArgumentMode {
    Normal,
}

pub(crate) struct ParseOnlyTool;

impl Tool for ParseOnlyTool {
    const NAME: &'static str = "parse";
    type Error = ToolError;
    type Args = ArgumentFixture;
    type Output = String;

    async fn definition(&self, _: String) -> ToolDefinition {
        ToolDefinition {
            name: "parse".into(),
            description: String::new(),
            parameters: json!({"type":"object"}),
        }
    }

    async fn call(
        &self,
        args: Self::Args,
        _context: &ToolCallEnvelope,
    ) -> Result<String, Self::Error> {
        panic!(
            "invalid arguments must never execute; parsed text {:?} in mode {:?}",
            args.text, args.mode
        )
    }
}

#[derive(Default)]
pub(crate) struct EffectHook {
    pub(crate) invoked: AtomicBool,
    pub(crate) rejected: AtomicBool,
    pub(crate) yield_before: bool,
    pub(crate) cleanup_fails: bool,
    pub(crate) cleaned: AtomicBool,
    pub(crate) parent_settled: AtomicBool,
    pub(crate) terminate_before: bool,
}

#[async_trait]
impl SessionHook for EffectHook {
    type Error = std::convert::Infallible;

    async fn admit_tool_effect(
        &self,
        _: &str,
        _: u32,
        _: &str,
        _: &str,
        _: &ToolCallEnvelope,
    ) -> Result<String, String> {
        Ok("derived-call".into())
    }

    async fn finish_tool_effect(
        &self,
        _: &str,
        interruption: Option<&ToolOutcome>,
    ) -> Result<(), String> {
        if interruption.is_some() {
            self.cleaned.store(true, Ordering::SeqCst);
            if self.cleanup_fails {
                return Err("durable cleanup unavailable".into());
            }
        }
        Ok(())
    }

    async fn on_completion_call_with_context(
        &self,
        _: &Message,
        _: &[Message],
        _: Option<&Message>,
    ) -> HookAction {
        HookAction::Continue
    }

    async fn on_tool_call(
        &self,
        _: &str,
        _: Option<String>,
        _: &str,
        _: &str,
        _: &ToolCallEnvelope,
    ) -> ToolCallHookAction {
        self.invoked.store(true, Ordering::SeqCst);
        if self.terminate_before {
            return ToolCallHookAction::Terminate {
                reason: "child hook terminated".into(),
            };
        }
        if self.yield_before {
            tokio::task::yield_now().await;
        }
        ToolCallHookAction::Skip {
            reason: "specialized side effect".into(),
        }
    }

    async fn on_tool_admission_rejected(
        &self,
        _: &str,
        _: Option<String>,
        _: &str,
        _: &str,
        _: &ToolOutcome,
    ) -> HookAction {
        self.rejected.store(true, Ordering::SeqCst);
        HookAction::Continue
    }

    async fn on_tool_result(
        &self,
        _: &str,
        _: Option<String>,
        internal_id: &str,
        _: &str,
        _: &ToolOutcome,
    ) -> crate::ToolResultHookAction {
        if internal_id == "parent" {
            self.parent_settled.store(true, Ordering::SeqCst);
        }
        crate::ToolResultHookAction::Continue
    }

    async fn foreground_live_output_writer(
        &self,
        id: &str,
    ) -> crate::live_output::LiveToolOutputWriter {
        crate::live_output::LiveToolOutputRegistry::default()
            .writer_for(id)
            .await
    }

    async fn session_id(&self) -> Option<String> {
        None
    }

    async fn register_stream_tool_call_identity(&self, _: &str, _: &str, _: Option<&str>) {}
}

pub(crate) struct ReadyAfterWallDeadline {
    pub(crate) deadline_at: DateTime<Utc>,
    pub(crate) called: Arc<AtomicBool>,
    pub(crate) fails: bool,
}

impl ToolDyn for ReadyAfterWallDeadline {
    fn hidden(&self) -> bool {
        false
    }

    fn name(&self) -> String {
        "ready_after_wall_deadline".into()
    }

    fn definition<'a>(&'a self, _prompt: String) -> BoxFuture<'a, ToolDefinition> {
        simple_tool_def(
            "ready_after_wall_deadline",
            "wall-clock deadline regression",
        )
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "the tool must become ready past the wall deadline within one poll"
    )]
    fn call<'a>(
        &'a self,
        _args: String,
        _context: &'a ToolCallEnvelope,
    ) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(async move {
            self.called.store(true, Ordering::SeqCst);
            let remaining = (self.deadline_at - Utc::now()).to_std().unwrap_or_default();
            std::thread::sleep(remaining + Duration::from_millis(10));
            if self.fails {
                Err(ToolError::ReportedFailure {
                    class: FailureClass::ToolReturnedError,
                    text: "tool failed".into(),
                })
            } else {
                Ok("late success".into())
            }
        })
    }
}

pub(crate) struct AdmissionGateTool {
    pub(crate) name: &'static str,
    pub(crate) admitted: bool,
}

impl ToolDyn for AdmissionGateTool {
    fn hidden(&self) -> bool {
        false
    }

    fn name(&self) -> String {
        self.name.into()
    }

    fn definition<'a>(&'a self, _prompt: String) -> BoxFuture<'a, ToolDefinition> {
        simple_tool_def(self.name, "")
    }

    fn call<'a>(
        &'a self,
        _args: String,
        _context: &'a ToolCallEnvelope,
    ) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(async { Ok(String::new()) })
    }

    fn admit(&self, _args: &str, _context: &ToolCallEnvelope) -> Result<(), ToolError> {
        if self.admitted {
            Ok(())
        } else {
            Err(ToolError::ReportedFailure {
                class: FailureClass::PolicyDenied,
                text: "refused by the tool's own admission".into(),
            })
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("strict tool error")]
pub(crate) struct StrictToolError;

#[derive(serde::Deserialize)]
pub(crate) struct StrictArgs {
    pub(crate) text: String,
}

pub(crate) struct StrictArgsTool {
    pub(crate) calls: Arc<AtomicUsize>,
}

impl Tool for StrictArgsTool {
    const NAME: &'static str = "strict";
    type Error = StrictToolError;
    type Args = StrictArgs;
    type Output = String;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.to_string(),
            description: String::new(),
            parameters: json!({ "type": "object" }),
        }
    }

    async fn call(
        &self,
        args: Self::Args,
        _context: &ToolCallEnvelope,
    ) -> Result<Self::Output, Self::Error> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(format!("ran with {}", args.text))
    }
}

pub(crate) struct ReceiptTool(pub(crate) AtomicUsize);

impl Tool for ReceiptTool {
    const NAME: &'static str = "receipt_test";
    type Args = bool;
    type Output = String;
    type Error = std::io::Error;

    async fn definition(&self, _prompt: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.into(),
            description: String::new(),
            parameters: json!({"type": "boolean"}),
        }
    }

    async fn call(
        &self,
        succeed: bool,
        _context: &ToolCallEnvelope,
    ) -> Result<String, Self::Error> {
        self.0.fetch_add(1, Ordering::SeqCst);
        if succeed {
            Ok("visible output".into())
        } else {
            Err(std::io::Error::other("failed operation"))
        }
    }

    fn deferred_writes(&self, _output: &String) -> Option<DeferredToolWrites> {
        Some(DeferredToolWrites::new(17_u64))
    }
}

pub(crate) struct RequestProbe;

impl Tool for RequestProbe {
    const NAME: &'static str = "request_probe";
    type Args = bool;
    type Output = String;
    type Error = std::io::Error;

    async fn definition(&self, _: String) -> ToolDefinition {
        ToolDefinition {
            name: Self::NAME.into(),
            description: String::new(),
            parameters: json!({}),
        }
    }

    async fn call(&self, _: bool, _context: &ToolCallEnvelope) -> Result<String, Self::Error> {
        Err(std::io::Error::other("request required"))
    }

    async fn call_with_request(
        &self,
        _: bool,
        _context: &ToolCallEnvelope,
        request: &ToolRequestContext,
    ) -> Result<String, Self::Error> {
        Ok(format!(
            "{}:{}",
            request.request_doc_id,
            request.admitted_client.as_ref().unwrap().signer_did
        ))
    }
}

pub(crate) fn plugin_receipt() -> PluginExecutionReceipt {
    PluginExecutionReceipt {
        coordinate: "fixture@1".into(),
        artifact_digest: "artifact".into(),
        input_digest: "input".into(),
        output_digest: None,
        authority: None,
        limits: None,
        verdict: PluginExecutionVerdict::Failed,
    }
}

pub(crate) struct LifecycleReceiptTool {
    pub(crate) delay: bool,
    pub(crate) forge_text: bool,
}

impl ToolDyn for LifecycleReceiptTool {
    fn hidden(&self) -> bool {
        false
    }

    fn name(&self) -> String {
        "receipt".into()
    }

    fn definition<'a>(&'a self, _: String) -> BoxFuture<'a, ToolDefinition> {
        simple_tool_def("receipt", "fixture")
    }

    fn call<'a>(
        &'a self,
        _: String,
        _context: &'a ToolCallEnvelope,
    ) -> BoxFuture<'a, Result<String, ToolError>> {
        Box::pin(async { Ok(serde_json::to_string(&plugin_receipt()).unwrap()) })
    }

    #[expect(
        clippy::disallowed_methods,
        reason = "the tool must become ready past the deadline within one poll"
    )]
    fn call_with_receipt<'a>(
        &'a self,
        args: String,
        context: &'a ToolCallEnvelope,
    ) -> BoxFuture<'a, ToolDispatchResult> {
        Box::pin(async move {
            if self.delay {
                std::thread::sleep(Duration::from_millis(30));
            }
            ToolDispatchResult {
                result: self.call(args, context).await,
                plugin_receipt: (!self.forge_text).then(plugin_receipt),
                deferred_writes: None,
            }
        })
    }
}
