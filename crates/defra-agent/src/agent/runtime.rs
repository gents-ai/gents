use std::sync::Arc;

use anyhow::Result;
use rig::client::CompletionClient;
use tokio::sync::{watch, Mutex, Notify};
use tokio_util::sync::CancellationToken;

use super::daemon::ProfileDaemon;
use super::supervision::supervise_profiles_with_runner;
use super::{DefraAgent, ProcessLifecycleState};
use crate::backend_registry::BackendTracker;
use crate::health_checker::{spawn_health_checker, ServiceHealthMap};
use crate::meta_tools::build_meta_tools;
use crate::mcp_pool::McpPool;
use crate::prompt::LayeredPromptBuilder;
use crate::retry::RetryPolicy;
use crate::toolset::build_delegate_tool;

#[derive(Clone)]
struct RuntimeContext {
    node: Arc<defra_node::EmbeddedNode>,
    mcp_pool: McpPool,
    health_map: ServiceHealthMap,
    local_hostname: String,
    local_subnet: Option<String>,
    backend_tracker: Arc<BackendTracker>,
    retry_policy: RetryPolicy,
    hook_failure_policy: crate::hook::FailurePolicy,
    startup_barrier: Arc<StartupBarrier>,
}

pub(super) struct StartupBarrier {
    pending_profiles: Mutex<std::collections::HashSet<String>>,
    notify: Notify,
}

impl StartupBarrier {
    fn new(profiles: &[Arc<crate::config::ProfileConfig>]) -> Self {
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

    pub(super) async fn mark_profile_ready(&self, profile_name: &str) {
        let mut pending = self.pending_profiles.lock().await;
        let removed = pending.remove(profile_name);
        let is_empty = pending.is_empty();
        drop(pending);

        if removed && is_empty {
            self.notify.notify_waiters();
        }
    }

    pub(super) async fn wait_ready(&self) {
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
        profile: Arc<crate::config::ProfileConfig>,
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

pub(super) async fn run_agent(
    agent: DefraAgent,
    shutdown: watch::Receiver<bool>,
) -> Result<()> {
    let cancel = CancellationToken::new();
    let health_map = ServiceHealthMap::new();
    let _health_checker = spawn_health_checker(
        agent.node.clone(),
        agent.mcp_pool.clone(),
        health_map.clone(),
        agent.local_hostname.clone(),
        agent.local_subnet.clone(),
        cancel.child_token(),
    );

    if let Some(observer) = &agent.process_state_observer {
        observer.on_process_state_change(ProcessLifecycleState::Recovering);
    }

    let startup_barrier = Arc::new(StartupBarrier::new(&agent.profiles));

    let ops_graphql_endpoint = std::env::var("OPS_GRAPHQL_ENDPOINT")
        .unwrap_or_else(|_| "http://127.0.0.1:9202/api/v0/graphql".to_string());
    let backend_tracker = Arc::new(BackendTracker::new());
    let scheduler = crate::scheduler::Scheduler::new(
        agent.node.clone(),
        agent.profiles.clone(),
        agent.mcp_pool.clone(),
        health_map.clone(),
        agent.local_hostname.clone(),
        agent.local_subnet.clone(),
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
    let ready_observer = agent.process_state_observer.clone();
    let ready_profile_count = agent.profiles.len();
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
        node: agent.node.clone(),
        mcp_pool: agent.mcp_pool.clone(),
        health_map,
        local_hostname: agent.local_hostname.clone(),
        local_subnet: agent.local_subnet.clone(),
        backend_tracker,
        retry_policy: agent.retry_policy.clone(),
        hook_failure_policy: agent.hook_failure_policy,
        startup_barrier,
    };

    let result = supervise_profiles_with_runner(
        agent.profiles.clone(),
        shutdown,
        agent.retry_policy.clone(),
        move |profile, shutdown| {
            let runtime = runtime.clone();
            async move { runtime.run_profile(profile, shutdown).await }
        },
    )
    .await;

    if let Some(observer) = &agent.process_state_observer {
        observer.on_process_state_change(ProcessLifecycleState::ShuttingDown);
    }
    cancel.cancel();
    let _ = readiness_handle.await;
    let _ = scheduler_handle.await;
    if let Some(observer) = &agent.process_state_observer {
        observer.on_process_state_change(ProcessLifecycleState::Shutdown);
    }
    result
}

pub(super) fn default_hostname() -> String {
    hostname::get()
        .map(|host| host.to_string_lossy().to_string())
        .unwrap_or_else(|_| "unknown".to_string())
}
