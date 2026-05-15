//! Proptest fencing the loader-dedup invariant.
//!
//! Within a `DefraAgent` snapshot there is exactly one
//! `Arc<AgentPrincipal>`, and every `Arc<AgentBehavior>` in that
//! snapshot clones the same Arc. If a future code path constructs a
//! fresh principal Arc per behavior (e.g., from the behavior row's
//! `agent_did` FK instead of reusing the snapshot's), Lean's
//! `behavior_id_determines_principal` becomes observable-but-violated:
//! two behaviors with the same agent_did would point at different
//! Arcs, and a downstream caller cloning the principal Arc could
//! accidentally end up with diverging metadata.
//!
//! This proptest constructs arbitrary single-principal worlds and
//! verifies the invariant on the constructed `Vec<Arc<AgentBehavior>>`.

use std::sync::Arc;

use proptest::prelude::*;

use defra_agent::{AgentBehavior, AgentIdentity, AgentPrincipal};

#[path = "support/identity_stubs.rs"]
mod identity_stubs;
use identity_stubs::StubAgentIdentity;

/// Mimic the production loader's principal+behavior construction for
/// one snapshot's worth of behaviors. The production code lives in
/// `crates/defra-agent/src/agent.rs::from_default_behavior_documents`
/// and in the reconcile rebuild path; this helper isolates the
/// load-bearing logic (build one Arc<AgentPrincipal>, clone it into
/// every behavior).
fn build_snapshot_principal_and_behaviors(
    agent_did: String,
    behavior_ids: Vec<String>,
) -> (Arc<AgentPrincipal>, Vec<Arc<AgentBehavior>>) {
    let identity: Arc<dyn AgentIdentity> = StubAgentIdentity::arc(agent_did.clone());
    let principal = Arc::new(AgentPrincipal {
        agent_did,
        identity,
        default_behavior_id: behavior_ids.first().cloned().unwrap_or_default(),
        display_name: None,
        enabled: true,
    });

    let behaviors = behavior_ids
        .into_iter()
        .map(|behavior_id| {
            // Each behavior clones the *same* principal Arc. The
            // invariant under test: this Arc is shared, not freshly
            // constructed per behavior.
            Arc::new(AgentBehavior {
                behavior_id,
                principal: principal.clone(),
                backend_id: None,
                backend_provider_kind: defra_agent::BackendProviderKind::default(),
                backend_endpoint: String::new(),
                backend_api_key: None,
                backend_api_key_env_var: None,
                model_name: defra_agent::DEFAULT_MODEL_NAME.to_string(),
                context_window: defra_agent::DEFAULT_CONTEXT_WINDOW,
                max_output_tokens: defra_agent::DEFAULT_MAX_OUTPUT_TOKENS,
                max_turns: defra_agent::DEFAULT_MAX_TURNS,
                system_prompt: String::new(),
                tools: defra_agent::BehaviorToolConfig::default(),
                compaction_threshold: defra_agent::DEFAULT_COMPACTION_THRESHOLD,
                compaction_strategy: defra_agent::CompactionStrategy::StripThenSummarize,
                stream_batch_ms: defra_agent::DEFAULT_STREAM_BATCH_MS,
                deadline_duration: std::time::Duration::from_secs(
                    defra_agent::DEFAULT_DEADLINE_DURATION_SECS,
                ),
                sampling: defra_agent::SamplingConfig::default(),
            })
        })
        .collect();

    (principal, behaviors)
}

fn arb_did() -> impl Strategy<Value = String> {
    proptest::string::string_regex("did:agent:[a-z]{1,6}").unwrap()
}

fn arb_behavior_id() -> impl Strategy<Value = String> {
    proptest::string::string_regex("[a-z][a-z0-9-]{0,10}").unwrap()
}

proptest! {
    /// For any snapshot constructed via the helper, every behavior's
    /// principal Arc is pointer-equal to the snapshot's single
    /// principal Arc. Future loader changes that build fresh
    /// principal Arcs per behavior would fail this assertion.
    #[test]
    fn snapshot_behaviors_share_principal_arc(
        agent_did in arb_did(),
        behavior_ids in proptest::collection::vec(arb_behavior_id(), 0..20),
    ) {
        let (principal, behaviors) =
            build_snapshot_principal_and_behaviors(agent_did.clone(), behavior_ids);

        for behavior in &behaviors {
            prop_assert!(
                Arc::ptr_eq(&behavior.principal, &principal),
                "behavior {:?} held a different Arc<AgentPrincipal> than the snapshot principal",
                behavior.behavior_id,
            );
            prop_assert_eq!(behavior.principal.agent_did.as_str(), agent_did.as_str());
        }
    }

    /// Symmetric: for any two behaviors in the snapshot, their
    /// principal Arcs are pointer-equal. This is the form
    /// Lean's behavior_id_determines_principal takes at the runtime
    /// layer.
    #[test]
    fn pairs_in_snapshot_share_principal_arc(
        agent_did in arb_did(),
        behavior_ids in proptest::collection::vec(arb_behavior_id(), 2..20),
    ) {
        let (_principal, behaviors) =
            build_snapshot_principal_and_behaviors(agent_did, behavior_ids);

        for (i, b1) in behaviors.iter().enumerate() {
            for b2 in behaviors.iter().skip(i + 1) {
                prop_assert!(
                    Arc::ptr_eq(&b1.principal, &b2.principal),
                    "behaviors {:?} and {:?} held different principal Arcs",
                    b1.behavior_id,
                    b2.behavior_id,
                );
                prop_assert_eq!(b1.agent_did(), b2.agent_did());
            }
        }
    }
}
