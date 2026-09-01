//! Grok shim projection engine root.
//!
//! The projection engine owns the connection-local side of the Grok shim: it
//! turns durable Gents rows (`AgentResponse`, `AgentMessage`,
//! `AgentToolCall`/`AgentToolResult`, and runtime child `AgentRequest` rows)
//! into fresh Grok pager `session/update` notification payloads and stamps the
//! per-connection event metadata (`_meta.eventId`, `_meta.promptId`,
//! `_meta.totalTokens`) those payloads require.
//!
//! The engine is deliberately bounded and request-id-scoped:
//! - every projection helper takes an explicit request id and queries only the
//!   rows that request can own (one query per row family, no graph walks
//!   beyond the direct children of the projected request);
//! - projection is read-only: it never replays the session, never duplicates
//!   durable materialization, and never writes a document;
//! - every interpolated GraphQL value passes through
//!   [`gents::graphql::escape_graphql_string`], and every query executes
//!   in-process through [`EmbeddedNode::execute`].
//!
//! The three leaves own the payload shapes:
//! - [`messages`]: agent/user thought and message chunks plus streaming token
//!   and context metadata;
//! - [`tools`]: tool-call lifecycle, command titles/status/content,
//!   available-command updates, and the pager-style terminal `not supported`
//!   stubs;
//! - [`subagents`]: subagent spawned/progress/finished updates from runtime
//!   child `AgentRequest` rows and the shaped not-found ext stubs.
//!
//! Static `Task` configuration rows are never treated as runtime state and no
//! permission or terminal documents are ever fabricated here.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{anyhow, Context, Result};
use defra_node::EmbeddedNode;
use gents::{load_agent_behavior, load_inference_profile};
use serde_json::{json, Map, Value};

pub(crate) mod messages;
pub(crate) mod subagents;
pub(crate) mod tools;

/// Wire name of the ACP `session/update` notification every projection
/// payload is wrapped in.
pub(crate) const SESSION_UPDATE_METHOD: &str = "session/update";

/// Default context window reported when the bound configuration does not
/// supply one. Mirrors the model catalog's `totalContextTokens` default scale
/// (`gents::DEFAULT_CONTEXT_WINDOW`) so a bound behavior that never pinned a
/// window still reports a truthful, bounded value instead of zero.
pub(crate) const DEFAULT_CONTEXT_WINDOW_TOKENS: u64 = 262_144;

/// Bound model/context configuration the shim was assembled with.
///
/// Model and context-window values come from the bound `AgentBehavior` and its
/// `InferenceProfile`, not from `AgentSession` (which has no model or
/// context-window fields).
#[derive(Debug, Clone)]
pub(crate) struct BoundModelContext {
    /// Grok `modelId` the pager addresses (`backend_id::model_name`).
    pub(crate) model_id: String,
    /// Human display name; falls back to the raw model id when the catalog
    /// has no friendlier entry.
    pub(crate) model_name: String,
    /// `totalContextTokens` reported in the session/new model catalog and
    /// used to bound `_meta.totalTokens`.
    pub(crate) total_context_tokens: u64,
}

impl BoundModelContext {
    pub(crate) fn new(model_id: String, model_name: String, total_context_tokens: u64) -> Self {
        Self {
            model_id,
            model_name,
            total_context_tokens,
        }
    }

    /// Fall back to the catalog default when the bound profile did not pin a
    /// context window.
    pub(crate) fn effective_context_window(&self) -> u64 {
        if self.total_context_tokens == 0 {
            DEFAULT_CONTEXT_WINDOW_TOKENS
        } else {
            self.total_context_tokens
        }
    }
}

/// Connection-scoped, session-keyed projection counters.
///
/// Event ids are monotonic per connection and formatted
/// `"{session_id}-{counter}"`, matching the pager's `NotificationMeta` dedup
/// contract; `totalTokens` is cumulative and never decreases within a
/// session. Splitting the counters out keeps the arithmetic and envelope
/// stamping unit-testable without an embedded node.
#[derive(Debug, Default)]
pub(crate) struct ProjectionSequencer {
    event_counter: AtomicU64,
    total_tokens: AtomicU64,
}

impl ProjectionSequencer {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    /// Allocate the next monotonic event id for `session_id`.
    ///
    /// The counter is per-sequencer (per connection), starts at 1, and is only
    /// bumped on successful allocation, so a connection that projects nothing
    /// never consumes ids.
    pub(crate) fn next_event_id(&self, session_id: &str) -> String {
        let counter = self.event_counter.fetch_add(1, Ordering::SeqCst) + 1;
        format!("{session_id}-{counter}")
    }

    /// The number of event ids allocated so far.
    pub(crate) fn event_counter(&self) -> u64 {
        self.event_counter.load(Ordering::SeqCst)
    }

    /// Confirm a cumulative token count for the session.
    ///
    /// Grok's `_meta.totalTokens` is cumulative and must never decrease, so a
    /// stale or replayed observation is clamped to the running maximum.
    pub(crate) fn note_total_tokens(&self, observed: u64) -> u64 {
        let mut current = self.total_tokens.load(Ordering::SeqCst);
        while observed > current {
            match self.total_tokens.compare_exchange(
                current,
                observed,
                Ordering::SeqCst,
                Ordering::SeqCst,
            ) {
                Ok(_) => return observed,
                Err(actual) => current = actual,
            }
        }
        current
    }

    /// The current cumulative token count.
    pub(crate) fn total_tokens(&self) -> u64 {
        self.total_tokens.load(Ordering::SeqCst)
    }

    /// Build the `_meta` object stamped on one projected notification.
    ///
    /// Fields follow the pager's `NotificationMeta`: `eventId` is
    /// `"{sessionId}-{counter}"`, `totalTokens` is the cumulative session
    /// usage, and `promptId` correlates every update emitted while processing
    /// one prompt.
    pub(crate) fn notification_meta(
        &self,
        session_id: &str,
        prompt_id: Option<&str>,
        is_replay: bool,
    ) -> Value {
        let mut meta = Map::new();
        meta.insert(
            "eventId".to_string(),
            Value::String(self.next_event_id(session_id)),
        );
        meta.insert("totalTokens".to_string(), Value::from(self.total_tokens()));
        if let Some(prompt_id) = prompt_id {
            meta.insert("promptId".to_string(), Value::String(prompt_id.to_string()));
        }
        if is_replay {
            meta.insert("isReplay".to_string(), Value::Bool(true));
        }
        Value::Object(meta)
    }

    /// Wrap one projected update payload in a `session/update` notification.
    ///
    /// The Grok decoder expects the chunk field name `content` (not
    /// `contentBlock`); the leaves own that shape and this wrapper only adds
    /// the session envelope and `_meta`.
    pub(crate) fn session_update_notification(
        &self,
        session_id: &str,
        update: Value,
        prompt_id: Option<&str>,
    ) -> Value {
        let mut params = Map::new();
        params.insert("sessionId".to_string(), Value::String(session_id.to_string()));
        params.insert("update".to_string(), update);
        params.insert(
            "_meta".to_string(),
            self.notification_meta(session_id, prompt_id, false),
        );
        json!({
            "jsonrpc": "2.0",
            "method": SESSION_UPDATE_METHOD,
            "params": Value::Object(params),
        })
    }
}

/// Connection-scoped projection engine.
///
/// One engine instance serves one registered pager connection: it holds the
/// in-process node every projection query executes against, the bound
/// model/context configuration, and the connection's projection sequencer.
#[derive(Debug)]
pub(crate) struct ProjectionEngine {
    node: Arc<EmbeddedNode>,
    bound: BoundModelContext,
    sequencer: ProjectionSequencer,
}

impl ProjectionEngine {
    pub(crate) fn new(node: Arc<EmbeddedNode>, bound: BoundModelContext) -> Self {
        Self {
            node,
            bound,
            sequencer: ProjectionSequencer::new(),
        }
    }

    /// The in-process node every projection query executes against.
    pub(crate) fn node(&self) -> &Arc<EmbeddedNode> {
        &self.node
    }

    /// The bound model/context configuration.
    pub(crate) fn bound(&self) -> &BoundModelContext {
        &self.bound
    }

    /// The connection's projection sequencer.
    pub(crate) fn sequencer(&self) -> &ProjectionSequencer {
        &self.sequencer
    }

    /// Build the `_meta` object stamped on one projected notification.
    pub(crate) fn notification_meta(
        &self,
        session_id: &str,
        prompt_id: Option<&str>,
        is_replay: bool,
    ) -> Value {
        self.sequencer
            .notification_meta(session_id, prompt_id, is_replay)
    }

    /// Wrap one projected update payload in a `session/update` notification.
    pub(crate) fn session_update_notification(
        &self,
        session_id: &str,
        update: Value,
        prompt_id: Option<&str>,
    ) -> Value {
        self.sequencer
            .session_update_notification(session_id, update, prompt_id)
    }
}

/// Resolve the bound model/context configuration for the shim from the bound
/// behavior's `AgentBehavior` and `InferenceProfile` documents.
///
/// `AgentBehavior` selects `model_name` and `backend_id`; `InferenceProfile`
/// owns the context window. `AgentSession` has no model or context-window
/// fields and is never consulted here. Failures are surfaced as errors instead
/// of being papered over with a synthetic catalog entry.
pub(crate) async fn resolve_bound_model_context(
    node: &EmbeddedNode,
    behavior_id: &str,
) -> Result<BoundModelContext> {
    let behavior = load_agent_behavior(node, behavior_id)
        .await
        .with_context(|| format!("loading AgentBehavior {behavior_id:?} for the Grok shim"))?
        .ok_or_else(|| {
            anyhow!(
                "Grok shim is bound to behavior {behavior_id:?}, but no AgentBehavior document \
                 with that behavior_id exists"
            )
        })?;
    let model_name = behavior
        .model_name
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            anyhow!(
                "Grok shim is bound to behavior {behavior_id:?}, but that behavior has no \
                 model_name set, so no Grok modelId can be projected"
            )
        })?;
    let backend_id = behavior
        .backend_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .ok_or_else(|| {
            anyhow!(
                "Grok shim is bound to behavior {behavior_id:?}, but that behavior has no \
                 backend_id set, so no Grok modelId can be projected"
            )
        })?;
    let model_id = format!("{backend_id}::{model_name}");

    let context_window = match behavior.inference_profile_id.as_deref().map(str::trim) {
        Some(profile_id) if !profile_id.is_empty() => {
            let profile = load_inference_profile(node, profile_id)
                .await
                .with_context(|| {
                    format!("loading InferenceProfile {profile_id:?} for the Grok shim")
                })?
                .ok_or_else(|| {
                    anyhow!(
                        "Grok shim is bound to behavior {behavior_id:?}, which references \
                         inference_profile_id {profile_id:?}, but no InferenceProfile document \
                         with that id exists"
                    )
                })?;
                profile
                    .context_window
                    .and_then(|value| u64::try_from(value.max(0)).ok())
                    .unwrap_or(DEFAULT_CONTEXT_WINDOW_TOKENS)
        }
        _ => DEFAULT_CONTEXT_WINDOW_TOKENS,
    };

    Ok(BoundModelContext::new(model_id, model_name, context_window))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn event_ids_are_session_prefixed_and_monotonic() {
        let sequencer = ProjectionSequencer::new();
        assert_eq!(sequencer.next_event_id("session-1"), "session-1-1");
        assert_eq!(sequencer.next_event_id("session-1"), "session-1-2");
        assert_eq!(sequencer.next_event_id("session-2"), "session-2-3");
        assert_eq!(sequencer.event_counter(), 3);
    }

    #[test]
    fn a_fresh_sequencer_allocates_no_ids() {
        let sequencer = ProjectionSequencer::new();
        assert_eq!(sequencer.event_counter(), 0);
        assert_eq!(sequencer.total_tokens(), 0);
    }

    #[test]
    fn total_tokens_never_decrease() {
        let sequencer = ProjectionSequencer::new();
        assert_eq!(sequencer.note_total_tokens(100), 100);
        assert_eq!(sequencer.note_total_tokens(40), 100);
        assert_eq!(sequencer.note_total_tokens(250), 250);
        assert_eq!(sequencer.total_tokens(), 250);
    }

    #[test]
    fn notification_meta_carries_event_total_tokens_and_optional_prompt() {
        let sequencer = ProjectionSequencer::new();
        sequencer.note_total_tokens(64);
        let meta = sequencer.notification_meta("session-1", Some("prompt-9"), false);
        assert_eq!(meta["eventId"], "session-1-1");
        assert_eq!(meta["totalTokens"], 64);
        assert_eq!(meta["promptId"], "prompt-9");
        assert!(meta.get("isReplay").is_none());

        let replay = sequencer.notification_meta("session-1", None, true);
        assert_eq!(replay["isReplay"], true);
        assert!(replay.get("promptId").is_none());
    }

    #[test]
    fn session_update_notification_wraps_payload_with_session_and_meta() {
        let sequencer = ProjectionSequencer::new();
        let notification = sequencer.session_update_notification(
            "session-1",
            json!({
                "sessionUpdate": "agent_message_chunk",
                "content": {"type": "text", "text": "hi"},
            }),
            Some("prompt-1"),
        );
        assert_eq!(notification["jsonrpc"], "2.0");
        assert_eq!(notification["method"], "session/update");
        assert_eq!(notification["params"]["sessionId"], "session-1");
        assert_eq!(
            notification["params"]["update"]["sessionUpdate"],
            "agent_message_chunk"
        );
        // The Grok decoder expects the chunk field name `content`.
        assert_eq!(
            notification["params"]["update"]["content"]["text"],
            "hi"
        );
        assert_eq!(notification["params"]["_meta"]["promptId"], "prompt-1");
        assert_eq!(notification["params"]["_meta"]["eventId"], "session-1-1");
    }

    #[test]
    fn every_notification_consumes_exactly_one_event_id() {
        let sequencer = ProjectionSequencer::new();
        for _ in 0..3 {
            let _ = sequencer.session_update_notification("s", json!({}), None);
        }
        assert_eq!(sequencer.event_counter(), 3);
    }

    #[test]
    fn bound_context_window_falls_back_to_catalog_default() {
        let zeroed = BoundModelContext::new("b::m".to_string(), "m".to_string(), 0);
        assert_eq!(
            zeroed.effective_context_window(),
            DEFAULT_CONTEXT_WINDOW_TOKENS
        );
        let pinned = BoundModelContext::new("b::m".to_string(), "m".to_string(), 8_192);
        assert_eq!(pinned.effective_context_window(), 8_192);
    }

    #[test]
    fn bound_model_context_keeps_model_id_and_display_name() {
        let bound = BoundModelContext::new(
            "backend-a::GLM-5.3-NVFP4".to_string(),
            "GLM-5.3-NVFP4".to_string(),
            262_144,
        );
        assert_eq!(bound.model_id, "backend-a::GLM-5.3-NVFP4");
        assert_eq!(bound.model_name, "GLM-5.3-NVFP4");
        assert_eq!(bound.total_context_tokens, 262_144);
    }
}
