use super::support::{request, test_identity};
use crate::runtime::agent::PendingAgent;
use gents_documents::inference_defaults::ReasoningEffort;
use gents_inference::resolved::{ResolvedAgent, ServiceTier};
use gents_protocol::row::ExecutedInferenceBinding;

fn staged_slot(
    name: &str,
    backend_id: &str,
    model_name: &str,
    effort: Option<ReasoningEffort>,
    tier: Option<ServiceTier>,
) -> ResolvedAgent {
    let mut configured =
        PendingAgent::new("general").build_with_identity_for_test(test_identity(name));
    let agent = std::sync::Arc::make_mut(&mut configured.agent);
    agent.backend_id = Some(backend_id.to_owned());
    agent.model_name = model_name.to_owned();
    agent.sampling.reasoning_effort = effort;
    agent.service_tier = tier;
    agent.profile_id = Some(format!("{backend_id}:profile"));
    agent.clone()
}

#[test]
fn generated_effective_settings_bindings_match_the_runtime_owners() {
    use gents_lean_vocab::families::runtime_effective_settings_binding::{
        cases, EffectiveSettings, SelectedModel,
    };
    for case in cases::<String, String, String>() {
        let selected = &case.input.selected;
        let slot = staged_slot(
            &case.input.scenario,
            &selected.selected.backend_id,
            &selected.selected.model,
            selected
                .selected
                .effort
                .as_deref()
                .map(|s| ReasoningEffort::parse(s).unwrap()),
            selected
                .tier
                .as_ref()
                .map(|s| serde_json::from_value(serde_json::Value::String(s.clone())).unwrap()),
        );
        let mut row = request(Some("did:key:agent"), &case.input.scenario);
        if case.input.claimed {
            let executed = slot.executed_inference(case.input.runtime_generation);
            row.executed_model_name = Some(executed.model_name.clone());
            row.executed_reasoning_effort = executed.reasoning_effort.clone();
            row.executed_service_tier = executed.service_tier.clone();
            row.executed_inference_profile_id = executed.inference_profile_id.clone();
            row.executed_runtime_generation =
                Some(case.input.runtime_generation.try_into().unwrap());
        }
        let observed = match row.executed_inference() {
            ExecutedInferenceBinding::NotYetBound => None,
            ExecutedInferenceBinding::Bound(binding) => {
                assert_eq!(
                    binding.runtime_generation,
                    Some(case.input.runtime_generation)
                );
                let effective = slot.effective_inference();
                assert_eq!(binding.inference_profile_id, effective.profile_id);
                Some(EffectiveSettings {
                    selected: SelectedModel {
                        backend_id: effective.backend_id.unwrap(),
                        model: binding.model_name,
                        effort: binding.reasoning_effort,
                    },
                    tier: binding.service_tier,
                })
            }
        };
        case.check(observed);
    }
}
