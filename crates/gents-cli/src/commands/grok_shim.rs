//! Grok TUI shim assembly.
//!
//! Gents is the leader server; stock Grok is its pager client. This module
//! assembles the shim the same way the Codex shim is assembled, from the
//! in-process [`EmbeddedNode`] plus the *bound* behavior/model/context
//! documents:
//!
//! 1. [`protocol`] owns the length-prefixed wire codec and the
//!    register/registered/ping/pong/disconnect/ACP envelope types;
//! 2. [`server`] owns the leader server: the exclusive leader lock, the
//!    register → registered handshake, readiness gating, ping/pong, and ACP
//!    payload forwarding to the delegate;
//! 3. [`acp`] owns the ACP service: initialize capabilities, session/new with
//!    a preferred id, model/catalog/mode updates, and the shaped method-not-
//!    found stubs (`session/load`, `x.ai/interject`,
//!    `x.ai/compact_conversation`);
//! 4. [`turn`] owns connection-scoped pending prompts: JSON-RPC ids,
//!    submission via [`crate::create_agent_request`], deferred responses until
//!    terminalization, and interruption via [`gents::interrupt_request`];
//! 5. [`projection`] owns the bounded, request-id-scoped read-only projection
//!    of durable rows into fresh Grok `session/update` notification payloads.
//!
//! Every projection query runs in-process (`node.execute(&query).await`) with
//! every interpolated value escaped by
//! [`gents::graphql::escape_graphql_string`]; no HTTP GraphQL helper and no
//! stock Grok import is used anywhere in the shim. All diagnostics go through
//! `tracing` — never `println!`/`eprintln!`.

use std::sync::Arc;

use anyhow::{Context, Result};
use defra_node::EmbeddedNode;

pub(crate) mod acp;
pub(crate) mod projection;
pub(crate) mod protocol;
pub(crate) mod server;
pub(crate) mod turn;

use crate::commands::grok_shim::projection::resolve_bound_model_context;
use crate::commands::grok_shim::server::{spawn_leader, LeaderHandle, LeaderServerConfig};

/// Everything the shim needs to bind, in one place.
///
/// Model and context-window configuration is *bound*: it is resolved once from
/// the bound behavior's `AgentBehavior`/`InferenceProfile` documents before
/// the leader accepts a client, so the pager's model catalog and every
/// `_meta.totalTokens` bound come from real configuration rather than a
/// synthetic catalog entry.
#[derive(Debug, Clone)]
pub(crate) struct GrokShimBindArgs {
    /// In-process node every request, interrupt, and projection query uses.
    pub(crate) node: Arc<EmbeddedNode>,
    /// Bound behavior id; `None` resolves the agent principal's default.
    pub(crate) behavior_id: Option<String>,
    /// Agent DID requests are submitted for.
    pub(crate) agent_did: String,
    /// Unix socket path the leader binds and the pager connects to.
    pub(crate) socket_path: std::path::PathBuf,
}

/// Bind and spawn the Grok shim leader.
///
/// Resolution order mirrors the Codex shim's bound-behavior resolution: an
/// explicit `--grok-shim-behavior-id` override wins, then the agent
/// principal's configured `default_behavior_id`, then the synthesized
/// `<did>:default` fallback. The behavior must exist and select a model and
/// backend before the socket is published, so a misconfigured home fails fast
/// instead of serving a fabricated model catalog.
///
/// The returned [`LeaderHandle`] owns shutdown and the listener task; the
/// caller (the `gents server` launch path) holds it for the lifetime of the
/// serving loop, so dropping it at exit stops the listener and releases the
/// exclusive leader lock.
pub(crate) async fn bind_grok_shim(args: GrokShimBindArgs) -> Result<LeaderHandle> {
    let node = args.node.clone();
    let behavior_id =
        resolve_grok_shim_behavior_id(node.as_ref(), args.behavior_id.as_deref(), &args.agent_did)
            .await;
    let bound = resolve_bound_model_context(node.as_ref(), &behavior_id)
        .await
        .with_context(|| {
            format!(
                "binding the Grok shim to behavior {behavior_id:?}; fix the behavior with \
                 `gents config behavior set --behavior-id {behavior_id} ...`"
            )
        })?;
    tracing::info!(
        behavior_id = %behavior_id,
        model_id = %bound.model_id,
        total_context_tokens = bound.total_context_tokens,
        socket = %args.socket_path.display(),
        "grok shim leader binding"
    );
    let service = crate::commands::grok_shim::acp::AcpService::new(
        crate::commands::grok_shim::acp::AcpServiceConfig {
            node: node.clone(),
            agent_did: args.agent_did.clone(),
            behavior_id: behavior_id.clone(),
            bound,
        },
    );
    let leader = spawn_leader(
        LeaderServerConfig {
            socket_path: args.socket_path.clone(),
        },
        Arc::new(service),
    )
    .await
    .with_context(|| {
        format!(
            "spawning the Grok shim leader on socket {}",
            args.socket_path.display()
        )
    })?;
    tracing::info!(
        socket = %args.socket_path.display(),
        "grok shim leader is accepting pager connections"
    );
    Ok(leader)
}

/// Resolve the behavior the Grok shim binds to.
///
/// An explicit override always wins. Otherwise the agent principal's
/// configured `default_behavior_id` is used — that is the id behaviors are
/// actually stored under — and only a missing or unset principal falls back to
/// the synthesized `<did>:default` form, keeping legacy homes compatible.
pub(crate) async fn resolve_grok_shim_behavior_id(
    node: &EmbeddedNode,
    override_behavior_id: Option<&str>,
    agent_did: &str,
) -> String {
    if let Some(value) = explicit_behavior_override(override_behavior_id) {
        return value;
    }
    match gents::load_agent_principal(node, agent_did).await {
        Ok(Some(principal)) => principal
            .default_behavior_id
            .filter(|id| !id.trim().is_empty())
            .unwrap_or_else(|| gents::default_behavior_id_for_agent(agent_did)),
        _ => gents::default_behavior_id_for_agent(agent_did),
    }
}

/// The trimmed, non-empty form of an explicit behavior override, if any.
///
/// Exposed `pub(crate)` so the CLI surface and tests can share the exact
/// trimming rule the async resolver applies.
pub(crate) fn explicit_behavior_override(override_behavior_id: Option<&str>) -> Option<String> {
    override_behavior_id
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_behavior_overrides_win_and_are_trimmed() {
        assert_eq!(
            explicit_behavior_override(Some("  custom-behavior  ")).as_deref(),
            Some("custom-behavior")
        );
        assert_eq!(explicit_behavior_override(Some("behavior-a")).as_deref(), Some("behavior-a"));
    }

    #[test]
    fn blank_behavior_overrides_are_treated_as_absent() {
        assert_eq!(explicit_behavior_override(Some("   ")), None);
        assert_eq!(explicit_behavior_override(Some("")), None);
        assert_eq!(explicit_behavior_override(None), None);
    }

    #[test]
    fn the_default_behavior_fallback_is_the_agent_scoped_form() {
        assert_eq!(
            gents::default_behavior_id_for_agent("did:test:agent"),
            "did:test:agent:default"
        );
    }

    #[test]
    fn bind_args_carry_socket_behavior_and_identity() {
        let args = GrokShimBindArgs {
            node: test_node_placeholder(),
            behavior_id: Some("behavior-a".to_string()),
            agent_did: "did:test:agent".to_string(),
            socket_path: std::path::PathBuf::from("/tmp/gents-grok.sock"),
        };
        assert_eq!(args.behavior_id.as_deref(), Some("behavior-a"));
        assert_eq!(args.agent_did, "did:test:agent");
        assert_eq!(
            args.socket_path,
            std::path::PathBuf::from("/tmp/gents-grok.sock")
        );
        let cloned = args.clone();
        assert_eq!(cloned.agent_did, args.agent_did);
        assert_eq!(cloned.socket_path, args.socket_path);
    }

    /// `EmbeddedNode` needs a real data directory; assembly-level tests only
    /// inspect the plain fields of [`GrokShimBindArgs`], so a placeholder
    /// pointer keeps them node-free. The node-backed principal and
    /// model-resolution paths are exercised by the convergence gate's
    /// integration tests.
    fn test_node_placeholder() -> Arc<EmbeddedNode> {
        unreachable!("assembly unit tests must not construct an EmbeddedNode")
    }
}
