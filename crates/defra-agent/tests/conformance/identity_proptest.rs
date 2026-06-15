//! Loader-dedup proptest.
//!
//! Drives the production helper `assemble_principal_and_behaviors` (from
//! `crates/defra-agent/src/agent/principal_assembly.rs`) — the same
//! helper that both `resolve_document_runtime_snapshot_from_view` and
//! `DefraAgentBuilder::build` funnel through. Asserts `Arc::ptr_eq`
//! across all behaviors in arbitrarily-generated worlds.
//!
//! **Regression class fenced:** if a future change moves the
//! `Arc::new(AgentPrincipal { ... })` inside the factory loop in
//! `assemble_principal_and_behaviors`, every behavior would receive
//! a distinct Arc and `Arc::ptr_eq` would fail. The deliberate-
//! regression experiment demonstrates the failure mode.

use std::sync::Arc;

use proptest::prelude::*;

use defra_agent::__test_internals::{assemble_principal_and_behaviors, BehaviorBuildError};
use defra_agent::{AgentBehavior, AgentIdentity, AgentPrincipal};

#[path = "../support/identity_stubs.rs"]
mod identity_stubs;
use identity_stubs::StubAgentIdentity;

fn build_stub_behavior_factory(
    behavior_id: String,
) -> Box<
    dyn FnOnce(Arc<AgentPrincipal>) -> std::result::Result<AgentBehavior, BehaviorBuildError>
        + Send,
> {
    Box::new(move |principal| {
        Ok(AgentBehavior {
            skills: Vec::new(),
            behavior_id: behavior_id.clone(),
            principal,
            backend_id: None,
            backend_provider_kind: defra_agent::BackendProviderKind::OpenAiCompatible,
            backend_endpoint: String::new(),
            backend_api_key: None,
            backend_api_key_env_var: None,
            model_name: defra_agent::DEFAULT_MODEL_NAME.to_string(),
            context_window: defra_agent::DEFAULT_CONTEXT_WINDOW,
            max_output_tokens: defra_agent::DEFAULT_MAX_OUTPUT_TOKENS,
            max_turns: defra_agent::DEFAULT_MAX_TURNS,
            system_prompt: String::new(),
            request_context_template: None,
            tools: defra_agent::BehaviorToolConfig::default(),
            compaction_threshold: defra_agent::DEFAULT_COMPACTION_THRESHOLD,
            compaction_strategy: defra_agent::CompactionStrategy::StripThenSummarize,
            stream_batch_ms: defra_agent::DEFAULT_STREAM_BATCH_MS,
            stream_liveness_timeout: std::time::Duration::from_secs(
                defra_agent::DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS,
            ),
            deadline_duration: std::time::Duration::from_secs(
                defra_agent::DEFAULT_DEADLINE_DURATION_SECS,
            ),
            sampling: defra_agent::SamplingConfig::default(),
        })
    })
}

fn arb_did() -> impl Strategy<Value = String> {
    prop_oneof![
        Just("did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK".to_string()),
        Just("did:key:z6MkfXG2FkNy3u7Eg3jm8e2YQpGz7Z1JqWgHDAP1hLk9r2bR".to_string()),
        Just("did:key:z6MkpTHR8VNsBxYAAWHut2Geadd9jSwuBV8xRoAnwWsdvktH".to_string()),
    ]
}

fn arb_behavior_id() -> impl Strategy<Value = String> {
    proptest::string::string_regex("[a-z][a-z0-9-]{0,10}").unwrap()
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(256))]

    /// For any snapshot built via the production helper
    /// `assemble_principal_and_behaviors`, every behavior's principal
    /// Arc is pointer-equal to the returned snapshot principal.
    #[test]
    fn snapshot_behaviors_share_principal_arc(
        agent_did in arb_did(),
        behavior_ids in proptest::collection::vec(arb_behavior_id(), 0..20),
    ) {
        let identity: Arc<dyn AgentIdentity> = StubAgentIdentity::arc(agent_did.clone());
        let principal_data = AgentPrincipal {
            agent_did: agent_did.clone(),
            identity,
            default_behavior_id: behavior_ids.first().cloned().unwrap_or_default(),
            display_name: None,
            enabled: true,
        };

        let factories: Vec<_> = behavior_ids
            .iter()
            .cloned()
            .map(build_stub_behavior_factory)
            .collect();

        let (principal, results) = assemble_principal_and_behaviors(principal_data, factories);

        for result in &results {
            let behavior = result.as_ref().expect("stub factory never fails");
            prop_assert!(
                Arc::ptr_eq(&behavior.principal, &principal),
                "behavior {:?} held a different Arc<AgentPrincipal> than the snapshot principal",
                behavior.behavior_id,
            );
            prop_assert_eq!(behavior.principal.agent_did.as_str(), agent_did.as_str());
        }
    }

    /// For any pair of behaviors in the snapshot, their principal Arcs
    /// are pointer-equal — the runtime-layer form of Lean's
    /// `behavior_id_determines_principal`.
    #[test]
    fn pairs_in_snapshot_share_principal_arc(
        agent_did in arb_did(),
        behavior_ids in proptest::collection::vec(arb_behavior_id(), 2..20),
    ) {
        let identity: Arc<dyn AgentIdentity> = StubAgentIdentity::arc(agent_did.clone());
        let principal_data = AgentPrincipal {
            agent_did,
            identity,
            default_behavior_id: behavior_ids.first().cloned().unwrap_or_default(),
            display_name: None,
            enabled: true,
        };

        let factories: Vec<_> = behavior_ids
            .iter()
            .cloned()
            .map(build_stub_behavior_factory)
            .collect();

        let (_principal, results) = assemble_principal_and_behaviors(principal_data, factories);

        let behaviors: Vec<&AgentBehavior> = results
            .iter()
            .map(|r| r.as_ref().expect("stub factory never fails").as_ref())
            .collect();

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
