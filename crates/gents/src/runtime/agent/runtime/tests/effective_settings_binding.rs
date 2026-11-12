use super::support::{request, test_identity};
use crate::inference::resolved::{ReasoningEffort, ResolvedAgent, ServiceTier};
use crate::runtime::agent::PendingAgent;
use gents_protocol::row::{ExecutedInferenceBinding, ReasoningEffortBinding, ServiceTierBinding};

/// A staged slot holding exactly the settings one generation binds.
fn staged_slot(
    name: &str,
    backend_id: &str,
    model_name: &str,
    reasoning_effort: Option<ReasoningEffort>,
    service_tier: Option<ServiceTier>,
) -> ResolvedAgent {
    let mut agent_config =
        PendingAgent::new("general").build_with_identity_for_test(test_identity(name));
    agent_config.backend_id = Some(backend_id.to_owned());
    agent_config.model_name = model_name.to_owned();
    agent_config.sampling.reasoning_effort = reasoning_effort;
    agent_config.service_tier = service_tier;
    agent_config.profile_id = Some(format!("{backend_id}:profile"));
    agent_config
}

fn service_tier(name: &str) -> ServiceTier {
    serde_json::from_value(serde_json::Value::String(name.to_owned()))
        .expect("the model emits canonical service tier names")
}

/// The Lean owner decides which settings an observer may report for a request
/// after each action; the runtime owners must agree. The generation side is
/// `ResolvedAgent::effective_inference`, the per-execution side is the claim's
/// `ExecutedInferenceSettings` as `AgentRequest::executed_inference` reads it
/// back. A case the model binds to nothing must read as `NotYetBound`, never
/// as the agent's configured selection.
#[test]
fn generated_effective_settings_bindings_match_the_runtime_owners() {
    let cases =
        &crate::lean_vocab_test::lean_contract_snapshot().runtime_effective_settings_binding_cases;
    assert_eq!(cases.len(), 6);
    let mut bound_cases = 0;
    let mut unbound_cases = 0;
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let expected = &case["expected"];
        if expected.is_null() {
            unbound_cases += 1;
            assert_eq!(
                request(Some("did:key:agent"), name).executed_inference(),
                ExecutedInferenceBinding::NotYetBound,
                "{name}: a request the model binds to nothing has no executed settings"
            );
            continue;
        }
        bound_cases += 1;
        let backend_id = expected["backend_id"].as_str().unwrap();
        let model_name = expected["model"].as_str().unwrap();
        let effort = expected["reasoning_effort"].as_str();
        let tier = expected["service_tier"].as_str();
        let slot = staged_slot(
            name,
            backend_id,
            model_name,
            effort.map(|effort| ReasoningEffort::parse(effort).unwrap()),
            tier.map(service_tier),
        );

        let effective = slot.effective_inference();
        assert_eq!(effective.backend_id.as_deref(), Some(backend_id), "{name}");
        assert_eq!(effective.model_name, model_name, "{name}");
        assert_eq!(effective.reasoning_effort.as_deref(), effort, "{name}");
        assert_eq!(
            effective.service_tier,
            match tier {
                None => ServiceTierBinding::Unset,
                Some(tier) => ServiceTierBinding::Selected {
                    tier: tier.to_owned()
                },
            },
            "{name}"
        );
        assert_eq!(
            effective.reasoning_effort_dispatch,
            match effort {
                None => ReasoningEffortBinding::Unset,
                Some(effort) => ReasoningEffortBinding::Sent {
                    effort: effort.to_owned()
                },
            },
            "{name}"
        );

        let executed = slot.executed_inference(7);
        let mut row = request(Some("did:key:agent"), name);
        row.executed_model_name = Some(executed.model_name.clone());
        row.executed_reasoning_effort = executed.reasoning_effort.clone();
        row.executed_service_tier = executed.service_tier.clone();
        row.executed_inference_profile_id = executed.inference_profile_id.clone();
        row.executed_runtime_generation = Some(7);
        let ExecutedInferenceBinding::Bound(read_back) = row.executed_inference() else {
            panic!("{name}: a claimed request reports its binding");
        };
        assert_eq!(read_back, executed, "{name}");
        assert_eq!(read_back.model_name, model_name, "{name}");
        assert_eq!(read_back.reasoning_effort.as_deref(), effort, "{name}");
        assert_eq!(read_back.service_tier.as_deref(), tier, "{name}");
        assert_eq!(read_back.runtime_generation, Some(7), "{name}");
    }
    assert_eq!((bound_cases, unbound_cases), (4, 2));
}
