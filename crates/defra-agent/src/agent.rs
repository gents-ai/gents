use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use defra_node::EmbeddedNode;
use futures::{FutureExt, StreamExt};
use rig::agent::{Agent, MultiTurnStreamItem};
use rig::client::CompletionClient;
use rig::completion::message::{
    AssistantContent as AssistantMessageContent, Message as CompletionMessage,
    Reasoning as AssistantReasoning, Text as CompletionText, ToolCall as AssistantToolCall,
};
use rig::completion::CompletionModel;
use rig::one_or_many::OneOrMany;
use rig::streaming::{StreamedAssistantContent, StreamedUserContent, StreamingPrompt};
use tokio::sync::{watch, Mutex, Notify};
use tokio::task::JoinSet;
use tokio_util::sync::CancellationToken;

use crate::backend_registry::{self, BackendPermit, BackendTracker};
use crate::compaction::{self, CompactionOptions, CompactionStrategy, Compactor, DefraCompactor};
use crate::config::{
    ProfileConfig, DEFAULT_COMPACTION_THRESHOLD, DEFAULT_CONTEXT_WINDOW,
    DEFAULT_DEADLINE_DURATION_SECS, DEFAULT_MAX_OUTPUT_TOKENS, DEFAULT_MAX_TURNS,
    DEFAULT_MODEL_ENDPOINT, DEFAULT_MODEL_NAME, DEFAULT_STREAM_BATCH_MS,
    DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS,
};
use crate::error::classify_completion_error;
use crate::health_checker::{spawn_health_checker, ServiceHealthMap};
use crate::hook::{DefraSessionHook, FailurePolicy};
use crate::identity::AgentIdentity;
use crate::lifecycle::{ClaimOutcome, RequestLifecycle};
use crate::mcp_pool::McpPool;
use crate::meta_tools::build_meta_tools;
use crate::prompt::{LayeredPromptBuilder, PromptBuilder};
use crate::retry::RetryPolicy;
use crate::session;
use crate::streaming::{DefraStreamWriter, StreamStatus, StreamWriter};
use crate::toolset::build_delegate_tool;
use crate::toolset::ToolSet;
use crate::watcher::{DefraWatcher, Watcher};

const BACKEND_WAIT_POLL_MS: u64 = 1_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProcessLifecycleState {
    Uninitialized,
    Recovering,
    Ready,
    ShuttingDown,
    Shutdown,
}

impl ProcessLifecycleState {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Uninitialized => "uninitialized",
            Self::Recovering => "recovering",
            Self::Ready => "ready",
            Self::ShuttingDown => "shuttingDown",
            Self::Shutdown => "shutdown",
        }
    }
}

pub trait ProcessLifecycleObserver: Send + Sync {
    fn on_process_state_change(&self, state: ProcessLifecycleState);
}

#[derive(Clone)]
pub struct DefraAgent {
    node: Arc<EmbeddedNode>,
    profiles: Vec<Arc<ProfileConfig>>,
    mcp_pool: McpPool,
    local_hostname: String,
    local_subnet: Option<String>,
    retry_policy: RetryPolicy,
    hook_failure_policy: FailurePolicy,
    process_state_observer: Option<Arc<dyn ProcessLifecycleObserver>>,
}

#[derive(Default)]
pub struct DefraAgentBuilder {
    node: Option<Arc<EmbeddedNode>>,
    profiles: Vec<PendingProfileConfig>,
    mcp_pool: Option<McpPool>,
    local_hostname: Option<String>,
    local_subnet: Option<String>,
    retry_policy: RetryPolicy,
    hook_failure_policy: FailurePolicy,
    process_state_observer: Option<Arc<dyn ProcessLifecycleObserver>>,
}

pub struct ProfileBuilder {
    builder: DefraAgentBuilder,
    profile: PendingProfileConfig,
}

#[derive(Clone)]
struct PendingProfileConfig {
    name: String,
    identity: Option<Arc<dyn AgentIdentity>>,
    backend_id: Option<String>,
    model_endpoint: String,
    model_name: String,
    context_window: usize,
    max_output_tokens: usize,
    max_turns: usize,
    system_prompt: String,
    native_tools: ToolSet,
    compaction_threshold: f64,
    compaction_strategy: CompactionStrategy,
    stream_batch_ms: u64,
    deadline_duration: Duration,
}

impl PendingProfileConfig {
    fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            identity: None,
            backend_id: None,
            model_endpoint: DEFAULT_MODEL_ENDPOINT.to_string(),
            model_name: DEFAULT_MODEL_NAME.to_string(),
            context_window: DEFAULT_CONTEXT_WINDOW,
            max_output_tokens: DEFAULT_MAX_OUTPUT_TOKENS,
            max_turns: DEFAULT_MAX_TURNS,
            system_prompt: String::new(),
            native_tools: ToolSet::meta_only(),
            compaction_threshold: DEFAULT_COMPACTION_THRESHOLD,
            compaction_strategy: CompactionStrategy::StripThenSummarize,
            stream_batch_ms: DEFAULT_STREAM_BATCH_MS,
            deadline_duration: Duration::from_secs(DEFAULT_DEADLINE_DURATION_SECS),
        }
    }

    fn build(self) -> Result<ProfileConfig> {
        let identity = self
            .identity
            .ok_or_else(|| anyhow!("profile '{}' is missing identity", self.name))?;

        Ok(ProfileConfig {
            name: self.name,
            identity,
            backend_id: self.backend_id,
            model_endpoint: self.model_endpoint,
            model_name: self.model_name,
            context_window: self.context_window,
            max_output_tokens: self.max_output_tokens,
            max_turns: self.max_turns,
            system_prompt: self.system_prompt,
            native_tools: self.native_tools,
            compaction_threshold: self.compaction_threshold,
            compaction_strategy: self.compaction_strategy,
            stream_batch_ms: self.stream_batch_ms,
            deadline_duration: self.deadline_duration,
        })
    }
}

impl DefraAgent {
    pub fn builder() -> DefraAgentBuilder {
        DefraAgentBuilder::default()
    }

    pub fn profiles(&self) -> &[Arc<ProfileConfig>] {
        &self.profiles
    }

    pub async fn run(self, shutdown: watch::Receiver<bool>) -> Result<()> {
        let cancel = CancellationToken::new();
        let health_map = ServiceHealthMap::new();
        let _health_checker = spawn_health_checker(
            self.node.clone(),
            self.mcp_pool.clone(),
            health_map.clone(),
            self.local_hostname.clone(),
            self.local_subnet.clone(),
            cancel.child_token(),
        );

        if let Some(observer) = &self.process_state_observer {
            observer.on_process_state_change(ProcessLifecycleState::Recovering);
        }

        let startup_barrier = Arc::new(StartupBarrier::new(&self.profiles));

        let ops_graphql_endpoint = std::env::var("OPS_GRAPHQL_ENDPOINT")
            .unwrap_or_else(|_| "http://127.0.0.1:9202/api/v0/graphql".to_string());
        let backend_tracker = Arc::new(BackendTracker::new());
        let scheduler = crate::scheduler::Scheduler::new(
            self.node.clone(),
            self.profiles.clone(),
            self.mcp_pool.clone(),
            health_map.clone(),
            self.local_hostname.clone(),
            self.local_subnet.clone(),
            ops_graphql_endpoint,
            backend_tracker.clone(),
        );

        let scheduler_cancel = cancel.child_token();
        let scheduler_startup_barrier = startup_barrier.clone();
        let scheduler_handle = tokio::spawn(async move {
            tokio::select! {
                _ = scheduler_cancel.cancelled() => return,
                _ = scheduler_startup_barrier.wait_ready() => {}
            }
            if let Err(error) = scheduler.run(scheduler_cancel).await {
                tracing::error!(error = %error, "scheduler exited with error");
            }
        });

        let ready_cancel = cancel.child_token();
        let ready_startup_barrier = startup_barrier.clone();
        let ready_observer = self.process_state_observer.clone();
        let ready_profile_count = self.profiles.len();
        let readiness_handle = tokio::spawn(async move {
            tokio::select! {
                _ = ready_cancel.cancelled() => return,
                _ = ready_startup_barrier.wait_ready() => {}
            }
            if let Some(observer) = &ready_observer {
                observer.on_process_state_change(ProcessLifecycleState::Ready);
            }
            tracing::info!(profiles = ready_profile_count, "defra-agent ready");
        });

        let runtime = RuntimeContext {
            node: self.node.clone(),
            mcp_pool: self.mcp_pool.clone(),
            health_map,
            local_hostname: self.local_hostname.clone(),
            local_subnet: self.local_subnet.clone(),
            backend_tracker,
            retry_policy: self.retry_policy.clone(),
            hook_failure_policy: self.hook_failure_policy,
            startup_barrier,
        };

        let result = supervise_profiles_with_runner(
            self.profiles.clone(),
            shutdown,
            self.retry_policy.clone(),
            move |profile, shutdown| {
                let runtime = runtime.clone();
                async move { runtime.run_profile(profile, shutdown).await }
            },
        )
        .await;

        if let Some(observer) = &self.process_state_observer {
            observer.on_process_state_change(ProcessLifecycleState::ShuttingDown);
        }
        cancel.cancel();
        let _ = readiness_handle.await;
        let _ = scheduler_handle.await;
        if let Some(observer) = &self.process_state_observer {
            observer.on_process_state_change(ProcessLifecycleState::Shutdown);
        }
        result
    }
}

impl DefraAgentBuilder {
    pub fn node(mut self, node: Arc<EmbeddedNode>) -> Self {
        self.node = Some(node);
        self
    }

    pub fn mcp_pool(mut self, mcp_pool: McpPool) -> Self {
        self.mcp_pool = Some(mcp_pool);
        self
    }

    pub fn local_hostname(mut self, hostname: impl Into<String>) -> Self {
        self.local_hostname = Some(hostname.into());
        self
    }

    pub fn local_subnet(mut self, subnet: impl Into<String>) -> Self {
        self.local_subnet = Some(subnet.into());
        self
    }

    pub fn retry_policy(mut self, policy: RetryPolicy) -> Self {
        self.retry_policy = policy;
        self
    }

    pub fn hook_failure_policy(mut self, policy: FailurePolicy) -> Self {
        self.hook_failure_policy = policy;
        self
    }

    pub fn process_state_observer(
        mut self,
        observer: Arc<dyn ProcessLifecycleObserver>,
    ) -> Self {
        self.process_state_observer = Some(observer);
        self
    }

    pub fn profile(self, name: impl Into<String>) -> ProfileBuilder {
        ProfileBuilder {
            builder: self,
            profile: PendingProfileConfig::new(name),
        }
    }

    pub fn build(self) -> Result<DefraAgent> {
        let node = self
            .node
            .ok_or_else(|| anyhow!("DefraAgent builder requires a node"))?;

        if self.profiles.is_empty() {
            bail!("DefraAgent requires at least one profile");
        }

        let mut names = std::collections::HashSet::new();
        let mut profiles = Vec::with_capacity(self.profiles.len());
        for profile in self.profiles {
            if !names.insert(profile.name.clone()) {
                bail!("duplicate profile name '{}'", profile.name);
            }
            profiles.push(Arc::new(profile.build()?));
        }

        Ok(DefraAgent {
            node,
            profiles,
            mcp_pool: self.mcp_pool.unwrap_or_default(),
            local_hostname: self.local_hostname.unwrap_or_else(default_hostname),
            local_subnet: self.local_subnet,
            retry_policy: self.retry_policy,
            hook_failure_policy: self.hook_failure_policy,
            process_state_observer: self.process_state_observer,
        })
    }
}

impl ProfileBuilder {
    pub fn identity<I>(mut self, identity: I) -> Self
    where
        I: AgentIdentity + 'static,
    {
        self.profile.identity = Some(Arc::new(identity));
        self
    }

    pub fn system_prompt(mut self, prompt: impl Into<String>) -> Self {
        self.profile.system_prompt = prompt.into();
        self
    }

    pub fn native_tools(mut self, native_tools: ToolSet) -> Self {
        self.profile.native_tools = native_tools;
        self
    }

    pub fn model_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.profile.model_endpoint = endpoint.into();
        self
    }

    pub fn model_name(mut self, name: impl Into<String>) -> Self {
        self.profile.model_name = name.into();
        self
    }

    pub fn context_window(mut self, context_window: usize) -> Self {
        self.profile.context_window = context_window;
        self
    }

    pub fn max_output_tokens(mut self, max_output_tokens: usize) -> Self {
        self.profile.max_output_tokens = max_output_tokens;
        self
    }

    pub fn max_turns(mut self, max_turns: usize) -> Self {
        self.profile.max_turns = max_turns;
        self
    }

    pub fn compaction_threshold(mut self, threshold: f64) -> Self {
        self.profile.compaction_threshold = threshold;
        self
    }

    pub fn compaction_strategy(mut self, strategy: CompactionStrategy) -> Self {
        self.profile.compaction_strategy = strategy;
        self
    }

    pub fn stream_batch_ms(mut self, stream_batch_ms: u64) -> Self {
        self.profile.stream_batch_ms = stream_batch_ms;
        self
    }

    pub fn deadline_duration(mut self, deadline_duration: Duration) -> Self {
        self.profile.deadline_duration = deadline_duration;
        self
    }

    pub fn backend_id(mut self, id: impl Into<String>) -> Self {
        self.profile.backend_id = Some(id.into());
        self
    }

    pub fn done(mut self) -> DefraAgentBuilder {
        self.builder.profiles.push(self.profile);
        self.builder
    }
}

#[derive(Clone)]
struct RuntimeContext {
    node: Arc<EmbeddedNode>,
    mcp_pool: McpPool,
    health_map: ServiceHealthMap,
    local_hostname: String,
    local_subnet: Option<String>,
    backend_tracker: Arc<BackendTracker>,
    retry_policy: RetryPolicy,
    hook_failure_policy: FailurePolicy,
    startup_barrier: Arc<StartupBarrier>,
}

struct StartupBarrier {
    pending_profiles: Mutex<std::collections::HashSet<String>>,
    notify: Notify,
}

impl StartupBarrier {
    fn new(profiles: &[Arc<ProfileConfig>]) -> Self {
        Self {
            pending_profiles: Mutex::new(
                profiles
                    .iter()
                    .map(|profile| profile.name.clone())
                    .collect::<std::collections::HashSet<_>>(),
            ),
            notify: Notify::new(),
        }
    }

    async fn mark_profile_ready(&self, profile_name: &str) {
        let mut pending = self.pending_profiles.lock().await;
        let removed = pending.remove(profile_name);
        let is_empty = pending.is_empty();
        drop(pending);

        if removed && is_empty {
            self.notify.notify_waiters();
        }
    }

    async fn wait_ready(&self) {
        loop {
            if self.pending_profiles.lock().await.is_empty() {
                return;
            }
            self.notify.notified().await;
        }
    }
}

impl RuntimeContext {
    async fn run_profile(
        &self,
        profile: Arc<ProfileConfig>,
        shutdown: watch::Receiver<bool>,
    ) -> Result<()> {
        let api_key =
            std::env::var("AGENT_DAEMON_API_KEY").unwrap_or_else(|_| "no-key".to_string());
        let openai_client: rig::providers::openai::CompletionsClient =
            rig::providers::openai::CompletionsClient::builder()
                .api_key(&api_key)
                .base_url(&profile.model_endpoint)
                .build()?;

        let prompt_builder = LayeredPromptBuilder::from_profile(profile.as_ref());
        let preamble = prompt_builder.preamble().to_string();

        let mut tools = profile.native_tools.build_native_tools()?;
        tools.push(build_delegate_tool(self.node.clone()));
        tools.extend(build_meta_tools(
            self.node.clone(),
            self.mcp_pool.clone(),
            self.health_map.clone(),
            self.local_hostname.clone(),
            self.local_subnet.clone(),
        ));

        let agent = openai_client
            .agent(&profile.model_name)
            .preamble(&preamble)
            .default_max_turns(profile.max_turns)
            .tools(tools)
            .build();

        let mut daemon = ProfileDaemon::new(
            self.node.clone(),
            profile,
            agent,
            self.backend_tracker.clone(),
            self.retry_policy.clone(),
            self.hook_failure_policy,
            self.startup_barrier.clone(),
        );
        daemon.run(shutdown).await
    }
}

struct ProfileDaemon<M: CompletionModel> {
    node: Arc<EmbeddedNode>,
    profile: Arc<ProfileConfig>,
    agent: Agent<M>,
    watcher: DefraWatcher,
    backend_tracker: Arc<BackendTracker>,
    prompt_builder: LayeredPromptBuilder,
    stream_writer: DefraStreamWriter,
    compactor: DefraCompactor<M>,
    compaction_options: CompactionOptions,
    retry_policy: RetryPolicy,
    hook_failure_policy: FailurePolicy,
    startup_barrier: Arc<StartupBarrier>,
}

enum HandleRequestOutcome {
    Completed,
    FailedAfterResponse(anyhow::Error),
}

enum StreamAction {
    Continue,
    Done,
    Error(rig::agent::StreamingError),
}

struct StreamProcessor<'a> {
    persistence_hook: &'a DefraSessionHook,
    stream_writer: &'a DefraStreamWriter,
    lifecycle: &'a mut RequestLifecycle,
    assistant_turn: AssistantTurnAccumulator,
    streamed_text: String,
    final_text: Option<String>,
    doc_id: &'a str,
}

impl StreamProcessor<'_> {
    async fn process_item<R>(
        &mut self,
        item: Result<MultiTurnStreamItem<R>, rig::agent::StreamingError>,
    ) -> Result<StreamAction> {
        match item {
            Ok(MultiTurnStreamItem::StreamAssistantItem(StreamedAssistantContent::Text(text))) => {
                self.assistant_turn.push_text(&text.text);
                self.streamed_text.push_str(&text.text);
                let flushed = self
                    .stream_writer
                    .write_tokens(self.doc_id, &text.text)
                    .await?;
                if flushed {
                    self.lifecycle.advance().await?;
                }
                Ok(StreamAction::Continue)
            }
            Ok(MultiTurnStreamItem::StreamAssistantItem(StreamedAssistantContent::Reasoning(
                reasoning,
            ))) => {
                self.assistant_turn.push_reasoning(reasoning);
                Ok(StreamAction::Continue)
            }
            Ok(MultiTurnStreamItem::StreamAssistantItem(
                StreamedAssistantContent::ReasoningDelta { reasoning, id },
            )) => {
                self.assistant_turn.push_reasoning_delta(id, &reasoning);
                Ok(StreamAction::Continue)
            }
            Ok(MultiTurnStreamItem::StreamAssistantItem(StreamedAssistantContent::ToolCall {
                tool_call,
                ..
            })) => {
                self.assistant_turn.push_tool_call(tool_call);
                self.lifecycle.advance().await?;
                Ok(StreamAction::Continue)
            }
            Ok(MultiTurnStreamItem::StreamUserItem(StreamedUserContent::ToolResult {
                tool_result,
                ..
            })) => {
                if let Some(message) = self.assistant_turn.take_message() {
                    self.persistence_hook.apply_persistence_policy(
                        self.persistence_hook
                            .persist_message(&message)
                            .await
                            .map(|_| ()),
                        "persist streamed assistant turn",
                    )?;
                }
                self.persistence_hook.apply_persistence_policy(
                    self.persistence_hook
                        .persist_stream_tool_result_message(&tool_result)
                        .await,
                    "persist stream tool result",
                )?;
                self.lifecycle.advance().await?;
                Ok(StreamAction::Continue)
            }
            Ok(MultiTurnStreamItem::FinalResponse(response)) => {
                self.assistant_turn.reconcile_text(response.response());
                if let Some(message) = self.assistant_turn.take_message() {
                    self.persistence_hook.apply_persistence_policy(
                        self.persistence_hook
                            .persist_message(&message)
                            .await
                            .map(|_| ()),
                        "persist final assistant turn",
                    )?;
                }
                self.lifecycle.advance().await?;
                self.final_text = Some(response.response().to_string());
                Ok(StreamAction::Done)
            }
            Ok(_) => Ok(StreamAction::Continue),
            Err(error) => Ok(StreamAction::Error(error)),
        }
    }
}

impl<M: CompletionModel + 'static> ProfileDaemon<M> {
    fn new(
        node: Arc<EmbeddedNode>,
        profile: Arc<ProfileConfig>,
        agent: Agent<M>,
        backend_tracker: Arc<BackendTracker>,
        retry_policy: RetryPolicy,
        hook_failure_policy: FailurePolicy,
        startup_barrier: Arc<StartupBarrier>,
    ) -> Self {
        let watcher = DefraWatcher::new(node.clone(), profile.did());
        let prompt_builder = LayeredPromptBuilder::from_profile(profile.as_ref());
        let stream_writer = DefraStreamWriter::new(
            node.clone(),
            profile.did(),
            Duration::from_millis(profile.stream_batch_ms),
        );
        let compactor = DefraCompactor::new(agent.clone());
        let compaction_options = CompactionOptions {
            threshold: profile.compaction_threshold,
            strategy: profile.compaction_strategy.clone(),
            ..Default::default()
        };

        Self {
            node,
            profile,
            agent,
            watcher,
            backend_tracker,
            prompt_builder,
            stream_writer,
            compactor,
            compaction_options,
            retry_policy,
            hook_failure_policy,
            startup_barrier,
        }
    }

    async fn run(&mut self, mut shutdown: watch::Receiver<bool>) -> Result<()> {
        tracing::info!(
            profile = %self.profile.name,
            did = %self.profile.did(),
            model = %self.profile.model_name,
            context_window = self.profile.context_window,
            "defra-agent profile started"
        );

        match RequestLifecycle::recover_all(&self.node, self.profile.did()).await {
            Ok(report) => {
                if report.requests_recovered > 0 {
                    tracing::info!(profile = %self.profile.name, count = report.requests_recovered, "recovered stuck requests");
                }
                if report.responses_recovered > 0 {
                    tracing::info!(profile = %self.profile.name, count = report.responses_recovered, "recovered stuck responses");
                }
                if report.conversations_recovered > 0 {
                    tracing::info!(profile = %self.profile.name, count = report.conversations_recovered, "recovered stuck conversations");
                }
            }
            Err(error) => {
                tracing::warn!(profile = %self.profile.name, error = %error, "startup recovery failed");
            }
        }

        self.startup_barrier
            .mark_profile_ready(&self.profile.name)
            .await;
        tracing::info!(
            profile = %self.profile.name,
            did = %self.profile.did(),
            "defra-agent profile ready"
        );

        loop {
            let request = tokio::select! {
                biased;

                _ = shutdown.changed() => {
                    tracing::info!(profile = %self.profile.name, "shutdown signal received");
                    return Ok(());
                }

                req = self.watcher.next_request() => {
                    match req {
                        Some(Ok(req)) => req,
                        Some(Err(error)) => {
                            tracing::error!(profile = %self.profile.name, error = %error, "watcher error, retrying");
                            continue;
                        }
                        None => return Ok(()),
                    }
                }
            };

            let mut lifecycle = RequestLifecycle::new_with_execution_binding(
                self.node.clone(),
                &self.profile.name,
                self.profile.did(),
                request.clone(),
                self.profile.deadline_duration.as_secs(),
                crate::lifecycle::ExecutionOrigin::Interactive,
                self.profile.backend_id.clone().unwrap_or_default(),
            );

            match lifecycle.claim_with_identity().await {
                Ok(ClaimOutcome::Claimed) => {}
                Ok(ClaimOutcome::Superseded) => {
                    tracing::info!(
                        profile = %self.profile.name,
                        request_id = %request.request_id,
                        session_id = %request.session_id,
                        "request superseded by an earlier non-terminal request"
                    );
                    continue;
                }
                Err(error) => {
                    tracing::warn!(
                        profile = %self.profile.name,
                        request_id = %request.request_id,
                        error = %error,
                        "failed to claim request"
                    );
                    continue;
                }
            }

            match self.handle_request(&mut lifecycle).await {
                Ok(HandleRequestOutcome::Completed) => {
                    let _ = lifecycle.complete().await;
                }
                Ok(HandleRequestOutcome::FailedAfterResponse(error)) => {
                    tracing::error!(
                        profile = %self.profile.name,
                        request_id = %request.request_id,
                        error = %error,
                        "request failed after response started"
                    );
                    let _ = lifecycle.fail().await;
                }
                Err(error) => {
                    tracing::error!(
                        profile = %self.profile.name,
                        request_id = %request.request_id,
                        error = %error,
                        "request handling failed"
                    );
                    let _ = lifecycle.fail().await;
                    if !lifecycle.response_exists().await.unwrap_or(false) {
                        if let Err(stream_error) = self.write_error_response(&request, &error).await
                        {
                            tracing::error!(
                                profile = %self.profile.name,
                                error = %stream_error,
                                "failed to write error response"
                            );
                        }
                    }
                }
            }
        }
    }

    async fn acquire_backend_permit(
        &self,
        lifecycle: &mut RequestLifecycle,
    ) -> Result<BackendPermit> {
        let backend_id = lifecycle.backend_id();
        if backend_id.is_empty() {
            bail!(
                "request {} cannot start because profile {} has no backend binding",
                lifecycle.request().request_id,
                self.profile.name
            );
        }

        let deadline = tokio::time::Instant::now() + self.profile.deadline_duration;
        loop {
            if tokio::time::Instant::now() >= deadline {
                bail!(
                    "timed out waiting for backend {} capacity before inference start",
                    backend_id
                );
            }

            let backend = match backend_registry::lookup_backend(&self.node, backend_id).await? {
                Some(backend) => backend,
                None => bail!(
                    "backend {} not found for profile {}",
                    backend_id,
                    self.profile.name
                ),
            };

            if backend.is_available() {
                if let Some(permit) = self
                    .backend_tracker
                    .try_acquire_permit(backend_id, backend.max_concurrent)
                {
                    lifecycle.mark_slot_acquired().await?;
                    return Ok(permit);
                }
            }

            tokio::time::sleep(Duration::from_millis(BACKEND_WAIT_POLL_MS)).await;
        }
    }

    async fn handle_request(
        &mut self,
        lifecycle: &mut RequestLifecycle,
    ) -> Result<HandleRequestOutcome> {
        let request = lifecycle.request().clone();
        let full_history = session::load_history(&self.node, &request.session_id).await?;
        let (stripped_history, file_activity) = compaction::strip_tool_results(full_history);
        if !file_activity.is_empty() {
            tracing::debug!(
                profile = %self.profile.name,
                session_id = %request.session_id,
                files_read = ?file_activity.files_read,
                files_modified = ?file_activity.files_modified,
                "files referenced in stripped history"
            );
        }

        let compaction_entries =
            session::load_compaction_entries(&self.node, &request.session_id).await?;
        let mut history = drop_compacted_prefix(
            stripped_history,
            total_compacted_messages(&compaction_entries),
        );
        let mut summaries = compaction_entries
            .into_iter()
            .map(|entry| entry.summary)
            .collect::<Vec<_>>();

        let mut built = self.prompt_builder.build(&history, &summaries).await?;
        if prompt_exceeds_compaction_threshold(
            built.estimated_tokens,
            &request.content,
            self.profile.context_window,
            self.profile.compaction_threshold,
        ) {
            let result = self
                .compactor
                .compact(
                    history,
                    self.profile.context_window,
                    &CompactionOptions {
                        strategy: self.profile.compaction_strategy.clone(),
                        ..self.compaction_options.clone()
                    },
                )
                .await?;

            history = result.messages;
            if let Some(summary) = result.summary {
                let entry = session::save_compaction_entry(
                    &self.node,
                    &request.session_id,
                    &summary,
                    &result.files_read,
                    &result.files_modified,
                    result.messages_compacted,
                    result.original_token_estimate,
                    result.compacted_token_estimate,
                )
                .await?;
                summaries.push(entry.summary);
            }

            built = self.prompt_builder.build(&history, &summaries).await?;
        }

        let _backend_permit = self.acquire_backend_permit(lifecycle).await?;
        lifecycle.begin_execution().await?;

        let doc_id = self
            .stream_writer
            .begin(&request.session_id, &request.request_id)
            .await?;
        lifecycle.set_response_doc_id(&doc_id);
        lifecycle.advance().await?;

        let result = self
            .run_inference(&request, &doc_id, &built.messages, lifecycle)
            .await;

        match result {
            Ok(outcome) => Ok(outcome),
            Err(error) => {
                if let Err(finalize_error) = self
                    .stream_writer
                    .finalize(&doc_id, StreamStatus::Error)
                    .await
                {
                    tracing::error!(
                        profile = %self.profile.name,
                        doc_id = %doc_id,
                        error = %finalize_error,
                        "failed to finalize stream after error"
                    );
                }
                Err(error)
            }
        }
    }

    async fn run_inference(
        &mut self,
        request: &crate::watcher::AgentRequest,
        doc_id: &str,
        history: &[rig::completion::message::Message],
        lifecycle: &mut RequestLifecycle,
    ) -> Result<HandleRequestOutcome> {
        let max_attempts = self.retry_policy.max_retries + 1;
        let mut last_inference_error: Option<crate::error::InferenceError> = None;

        for attempt in 0..max_attempts {
            if attempt > 0 {
                let delay = self.retry_policy.delay_for_attempt(attempt - 1);
                tracing::info!(
                    profile = %self.profile.name,
                    attempt,
                    delay_ms = delay.as_millis() as u64,
                    request_id = %request.request_id,
                    "retrying inference after transient failure"
                );
                tokio::time::sleep(delay).await;
            }

            let hook = DefraSessionHook::resume_or_create_with_identity_policy(
                self.node.clone(),
                &request.session_id,
                &self.profile.name,
                self.profile.did(),
                self.hook_failure_policy,
            )
            .await?;
            let persistence_hook = hook.clone();

            let mut stream = self
                .agent
                .stream_prompt(&request.content)
                .with_history(history.to_vec())
                .with_hook(hook)
                .await;

            let liveness_timeout = Duration::from_secs(DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS);

            let (mut streamed_text, final_text, stream_error) = {
                let mut processor = StreamProcessor {
                    persistence_hook: &persistence_hook,
                    stream_writer: &self.stream_writer,
                    lifecycle,
                    assistant_turn: AssistantTurnAccumulator::default(),
                    streamed_text: String::new(),
                    final_text: None,
                    doc_id,
                };
                let mut stream_error = None;

                loop {
                    let item = match tokio::time::timeout(liveness_timeout, stream.next()).await {
                        Ok(Some(item)) => item,
                        Ok(None) => break,
                        Err(_) => {
                            stream_error = Some(rig::agent::StreamingError::Completion(
                                rig::completion::CompletionError::ProviderError(format!(
                                    "stream liveness timeout: no data received for {}s",
                                    DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS
                                )),
                            ));
                            break;
                        }
                    };
                    match processor.process_item(item).await {
                        Ok(StreamAction::Continue) => {}
                        Ok(StreamAction::Done) => break,
                        Ok(StreamAction::Error(error)) => {
                            stream_error = Some(error);
                            break;
                        }
                        Err(error) => return Err(error),
                    }
                }

                (processor.streamed_text, processor.final_text, stream_error)
            };

            if let Some(error) = stream_error {
                let classified = classify_completion_error(&error);
                let can_retry = classified.is_retryable()
                    && streamed_text_has_no_visible_content(&streamed_text)
                    && attempt + 1 < max_attempts;

                if can_retry {
                    last_inference_error = Some(classified);
                    continue;
                }

                let error_text = format!("Agent error: {}", error);
                if streamed_text_has_no_visible_content(&streamed_text) {
                    let _ = self.stream_writer.write_tokens(doc_id, &error_text).await?;
                }
                self.stream_writer
                    .finalize(doc_id, StreamStatus::Error)
                    .await?;

                return Ok(HandleRequestOutcome::FailedAfterResponse(anyhow!(
                    "agent stream failed: {}",
                    error
                )));
            }

            if let Some(text) = final_text.as_deref() {
                if streamed_text.is_empty() {
                    let _ = self.stream_writer.write_tokens(doc_id, text).await?;
                    streamed_text.push_str(text);
                } else if let Some(remainder) = text.strip_prefix(&streamed_text) {
                    if !remainder.is_empty() {
                        let _ = self.stream_writer.write_tokens(doc_id, remainder).await?;
                        streamed_text.push_str(remainder);
                    }
                }
            }

            self.stream_writer
                .finalize(doc_id, StreamStatus::Complete)
                .await?;

            return Ok(HandleRequestOutcome::Completed);
        }

        let last_error = last_inference_error
            .map(|error| error.to_string())
            .unwrap_or_else(|| "unknown".to_string());
        let error_text = format!(
            "Inference failed after {} attempts: {}",
            max_attempts, last_error
        );
        let _ = self.stream_writer.write_tokens(doc_id, &error_text).await?;
        self.stream_writer
            .finalize(doc_id, StreamStatus::Error)
            .await?;

        Ok(HandleRequestOutcome::FailedAfterResponse(anyhow!(
            "inference retries exhausted"
        )))
    }

    async fn write_error_response(
        &self,
        request: &crate::watcher::AgentRequest,
        error: &anyhow::Error,
    ) -> Result<()> {
        let doc_id = self
            .stream_writer
            .begin(&request.session_id, &request.request_id)
            .await?;
        let error_text = format!("Error: {}", error);
        let _ = self
            .stream_writer
            .write_tokens(&doc_id, &error_text)
            .await?;
        self.stream_writer
            .finalize(&doc_id, StreamStatus::Error)
            .await?;
        Ok(())
    }
}

async fn supervise_profiles_with_runner<F, Fut>(
    profiles: Vec<Arc<ProfileConfig>>,
    mut shutdown: watch::Receiver<bool>,
    retry_policy: RetryPolicy,
    runner: F,
) -> Result<()>
where
    F: Fn(Arc<ProfileConfig>, watch::Receiver<bool>) -> Fut + Send + Sync + Clone + 'static,
    Fut: std::future::Future<Output = Result<()>> + Send + 'static,
{
    let mut join_set = JoinSet::new();
    let mut running = std::collections::HashSet::new();
    let mut failure_counts = std::collections::HashMap::<String, u32>::new();

    for profile in profiles {
        spawn_profile(
            &mut join_set,
            &mut running,
            profile,
            shutdown.clone(),
            runner.clone(),
        );
    }

    loop {
        tokio::select! {
            _ = shutdown.changed() => return Ok(()),
            Some(joined) = join_set.join_next() => {
                let (profile, outcome) = joined?;
                running.remove(&profile.name);

                if shutdown.has_changed().unwrap_or(false) {
                    return Ok(());
                }

                match outcome {
                    Ok(Ok(())) => {
                        if running.is_empty() {
                            return Err(anyhow!("all profiles exited cleanly"));
                        }
                    }
                    Ok(Err(error)) => {
                        let attempt = failure_counts.entry(profile.name.clone()).or_default();
                        let delay = retry_policy.delay_for_attempt(*attempt);
                        *attempt += 1;
                        tracing::error!(
                            profile = %profile.name,
                            error = %error,
                            delay_ms = delay.as_millis() as u64,
                            "profile task failed, scheduling restart"
                        );
                        if running.is_empty() {
                            return Err(anyhow!("all profiles failed"));
                        }
                        wait_for_restart(delay, &mut shutdown).await?;
                        spawn_profile(&mut join_set, &mut running, profile, shutdown.clone(), runner.clone());
                    }
                    Err(_) => {
                        let attempt = failure_counts.entry(profile.name.clone()).or_default();
                        let delay = retry_policy.delay_for_attempt(*attempt);
                        *attempt += 1;
                        tracing::error!(
                            profile = %profile.name,
                            delay_ms = delay.as_millis() as u64,
                            "profile task panicked, scheduling restart"
                        );
                        if running.is_empty() {
                            return Err(anyhow!("all profiles failed"));
                        }
                        wait_for_restart(delay, &mut shutdown).await?;
                        spawn_profile(&mut join_set, &mut running, profile, shutdown.clone(), runner.clone());
                    }
                }
            }
            else => return Ok(()),
        }
    }
}

fn spawn_profile<F, Fut>(
    join_set: &mut JoinSet<(Arc<ProfileConfig>, std::thread::Result<Result<()>>)>,
    running: &mut std::collections::HashSet<String>,
    profile: Arc<ProfileConfig>,
    shutdown: watch::Receiver<bool>,
    runner: F,
) where
    F: Fn(Arc<ProfileConfig>, watch::Receiver<bool>) -> Fut + Send + Sync + Clone + 'static,
    Fut: std::future::Future<Output = Result<()>> + Send + 'static,
{
    let name = profile.name.clone();
    running.insert(name);
    join_set.spawn(async move {
        let outcome = AssertUnwindSafe(runner(profile.clone(), shutdown))
            .catch_unwind()
            .await;
        (profile, outcome)
    });
}

async fn wait_for_restart(delay: Duration, shutdown: &mut watch::Receiver<bool>) -> Result<()> {
    tokio::select! {
        _ = tokio::time::sleep(delay) => Ok(()),
        _ = shutdown.changed() => bail!("shutdown requested"),
    }
}

fn total_compacted_messages(entries: &[session::CompactionEntry]) -> usize {
    entries
        .iter()
        .map(|entry| entry.messages_compacted as usize)
        .sum()
}

fn drop_compacted_prefix(
    mut history: Vec<rig::completion::message::Message>,
    compacted: usize,
) -> Vec<rig::completion::message::Message> {
    let drain_count = compacted.min(history.len());
    history.drain(..drain_count);
    history
}

fn prompt_exceeds_compaction_threshold(
    prompt_tokens: usize,
    request_text: &str,
    context_window: usize,
    threshold: f64,
) -> bool {
    let budget = (context_window as f64 * threshold) as usize;
    prompt_tokens + compaction::estimate_tokens(request_text) > budget
}

fn streamed_text_has_no_visible_content(text: &str) -> bool {
    text.trim().is_empty()
}

fn default_hostname() -> String {
    hostname::get()
        .map(|host| host.to_string_lossy().to_string())
        .unwrap_or_else(|_| "unknown".to_string())
}

#[derive(Default)]
struct AssistantTurnAccumulator {
    text: String,
    reasoning: Vec<AssistantReasoning>,
    pending_reasoning_delta_text: String,
    pending_reasoning_delta_id: Option<String>,
    tool_calls: Vec<AssistantToolCall>,
}

impl AssistantTurnAccumulator {
    fn push_text(&mut self, text: &str) {
        self.text.push_str(text);
    }

    fn push_reasoning(&mut self, reasoning: AssistantReasoning) {
        merge_reasoning_blocks(&mut self.reasoning, &reasoning);
    }

    fn push_reasoning_delta(&mut self, id: Option<String>, reasoning: &str) {
        self.pending_reasoning_delta_text.push_str(reasoning);
        if self.pending_reasoning_delta_id.is_none() {
            self.pending_reasoning_delta_id = id;
        }
    }

    fn push_tool_call(&mut self, tool_call: AssistantToolCall) {
        self.tool_calls.push(tool_call);
    }

    fn reconcile_text(&mut self, final_text: &str) {
        if final_text.is_empty() {
            return;
        }
        if self.text.is_empty() {
            self.text.push_str(final_text);
        } else if let Some(remainder) = final_text.strip_prefix(&self.text) {
            self.text.push_str(remainder);
        }
    }

    fn take_message(&mut self) -> Option<CompletionMessage> {
        if self.reasoning.is_empty() && !self.pending_reasoning_delta_text.is_empty() {
            let mut assembled =
                AssistantReasoning::new(&std::mem::take(&mut self.pending_reasoning_delta_text));
            if let Some(id) = self.pending_reasoning_delta_id.take() {
                assembled = assembled.with_id(id);
            }
            self.push_reasoning(assembled);
        }

        let mut content = Vec::new();
        content.extend(
            self.reasoning
                .drain(..)
                .map(AssistantMessageContent::Reasoning),
        );
        content.extend(
            self.tool_calls
                .drain(..)
                .map(AssistantMessageContent::ToolCall),
        );

        if !self.text.is_empty() {
            content.push(AssistantMessageContent::Text(CompletionText {
                text: std::mem::take(&mut self.text),
            }));
        }

        self.pending_reasoning_delta_text.clear();
        self.pending_reasoning_delta_id = None;

        OneOrMany::many(content)
            .ok()
            .map(|content| CompletionMessage::Assistant { id: None, content })
    }
}

fn merge_reasoning_blocks(
    accumulated_reasoning: &mut Vec<AssistantReasoning>,
    incoming: &AssistantReasoning,
) {
    let ids_match = |existing: &AssistantReasoning| {
        matches!(
            (&existing.id, &incoming.id),
            (Some(existing_id), Some(incoming_id)) if existing_id == incoming_id
        )
    };

    if let Some(existing) = accumulated_reasoning
        .iter_mut()
        .rev()
        .find(|existing| ids_match(existing))
    {
        existing.content.extend(incoming.content.clone());
    } else {
        accumulated_reasoning.push(incoming.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::identity::SimpleIdentity;
    use std::sync::atomic::{AtomicUsize, Ordering};

    async fn test_node() -> Arc<EmbeddedNode> {
        Arc::new(EmbeddedNode::builder().build().await.unwrap())
    }

    fn test_identity(name: &str) -> SimpleIdentity {
        let path = std::env::temp_dir().join(format!("{name}-{}.key", uuid::Uuid::new_v4()));
        SimpleIdentity::new(name, path, None)
    }

    #[tokio::test]
    async fn profile_builder_rejects_missing_identity() {
        let node = test_node().await;
        let error = match DefraAgent::builder()
            .node(node)
            .profile("amy-general")
            .done()
            .build()
        {
            Ok(_) => panic!("expected missing identity error"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("missing identity"));
    }

    #[tokio::test]
    async fn profile_builder_rejects_duplicate_names() {
        let node = test_node().await;
        let error = match DefraAgent::builder()
            .node(node)
            .profile("amy-general")
            .identity(test_identity("amy-general-a"))
            .done()
            .profile("amy-general")
            .identity(test_identity("amy-general-b"))
            .done()
            .build()
        {
            Ok(_) => panic!("expected duplicate profile error"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("duplicate profile name"));
    }

    #[tokio::test]
    async fn supervision_restarts_panicking_profile_while_sibling_continues() {
        let panic_attempts = Arc::new(AtomicUsize::new(0));
        let sibling_ticks = Arc::new(AtomicUsize::new(0));
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let profiles = vec![
            Arc::new(
                PendingProfileConfig::new("panic-profile")
                    .build_with_identity_for_test(test_identity("panic-profile")),
            ),
            Arc::new(
                PendingProfileConfig::new("steady-profile")
                    .build_with_identity_for_test(test_identity("steady-profile")),
            ),
        ];

        let runner = {
            let panic_attempts = panic_attempts.clone();
            let sibling_ticks = sibling_ticks.clone();
            move |profile: Arc<ProfileConfig>, mut shutdown: watch::Receiver<bool>| {
                let panic_attempts = panic_attempts.clone();
                let sibling_ticks = sibling_ticks.clone();
                async move {
                    if profile.name == "panic-profile" {
                        let attempt = panic_attempts.fetch_add(1, Ordering::SeqCst);
                        if attempt < 2 {
                            panic!("boom");
                        }
                    }

                    loop {
                        sibling_ticks.fetch_add(1, Ordering::SeqCst);
                        tokio::select! {
                            _ = shutdown.changed() => return Ok(()),
                            _ = tokio::time::sleep(Duration::from_millis(25)) => {}
                        }
                    }
                }
            }
        };

        let task = tokio::spawn(supervise_profiles_with_runner(
            profiles,
            shutdown_rx,
            RetryPolicy {
                max_retries: 3,
                base_delay_ms: 10,
                max_delay_ms: 25,
            },
            runner,
        ));

        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if panic_attempts.load(Ordering::SeqCst) >= 3
                    && sibling_ticks.load(Ordering::SeqCst) > 3
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("profiles should restart and continue");
        assert!(panic_attempts.load(Ordering::SeqCst) >= 3);
        assert!(sibling_ticks.load(Ordering::SeqCst) > 3);

        let _ = shutdown_tx.send(true);
        task.await.unwrap().unwrap();
    }
}

#[cfg(test)]
impl PendingProfileConfig {
    fn build_with_identity_for_test<I>(mut self, identity: I) -> ProfileConfig
    where
        I: AgentIdentity + 'static,
    {
        self.identity = Some(Arc::new(identity));
        self.build().unwrap()
    }
}
