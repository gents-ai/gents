use std::sync::Arc;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use defra_node::EmbeddedNode;

use super::runtime::default_hostname;
use super::{DefraAgent, ProcessLifecycleObserver};
use crate::compaction::CompactionStrategy;
use crate::config::{
    ProfileConfig, DEFAULT_COMPACTION_THRESHOLD, DEFAULT_CONTEXT_WINDOW,
    DEFAULT_DEADLINE_DURATION_SECS, DEFAULT_MAX_OUTPUT_TOKENS, DEFAULT_MAX_TURNS,
    DEFAULT_MODEL_ENDPOINT, DEFAULT_MODEL_NAME, DEFAULT_STREAM_BATCH_MS,
};
use crate::hook::FailurePolicy;
use crate::identity::AgentIdentity;
use crate::mcp_pool::McpPool;
use crate::retry::RetryPolicy;
use crate::toolset::ToolSet;

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
pub(crate) struct PendingProfileConfig {
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
    pub(crate) fn new(name: impl Into<String>) -> Self {
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

#[cfg(test)]
impl PendingProfileConfig {
    pub(crate) fn build_with_identity_for_test<I>(mut self, identity: I) -> ProfileConfig
    where
        I: AgentIdentity + 'static,
    {
        self.identity = Some(Arc::new(identity));
        self.build().unwrap()
    }
}
