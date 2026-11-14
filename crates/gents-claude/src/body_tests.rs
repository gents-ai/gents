use super::*;
use crate::catalog;
use crate::wire::Thinking;

fn facts_of(id: &str) -> ModelFacts {
    ModelFacts::of(catalog::find(id).unwrap())
}

#[test]
fn field_decisions_match_lean() {
    use gents_lean_vocab::families::claude_prompt_assembly_field_decision as family;
    family::replay::<String, String>(|i| {
        let facts = ModelFacts {
            adaptive: i.facts.adaptive,
            always_thinks: i.facts.always_thinks,
            thinking_supported: i.facts.thinking_supported,
            rejects_disabled: i.facts.rejects_disabled,
            accepts_temperature: i.facts.accepts_temperature,
            effort_capable: i.facts.effort_capable,
            max_effort: i.facts.max_effort,
            xhigh_effort: i.facts.xhigh_effort,
            caps_effort_when_disabled: i.facts.caps_effort_when_disabled,
            per_turn_effort: i.facts.per_turn_effort,
            output_default: i.facts.output_default,
            output_upper: i.facts.output_upper,
            default_effort: i
                .facts
                .default_effort
                .as_deref()
                .map(|name| Effort::parse(name).unwrap()),
        };
        let wanted = &i.request;
        let request = Request {
            thinking: match wanted.thinking.kind.as_str() {
                "adaptive" => ThinkingConfig::Adaptive,
                "disabled" => ThinkingConfig::Disabled {
                    mechanical: wanted.thinking.mechanical,
                },
                "enabled" => ThinkingConfig::Enabled {
                    budget: wanted.thinking.budget,
                },
                other => panic!("unknown thinking kind {other}"),
            },
            max_tokens_override: wanted.max_tokens_override,
            requested_effort: wanted.requested_effort.as_deref(),
            first_party: wanted.first_party,
            first_party_default_url: wanted.first_party_default_url,
            experimental_betas_disabled: wanted.experimental_betas_disabled,
            agentic: wanted.agentic,
            fork: wanted.fork,
            display_updates: wanted.display_updates,
            context_management_beta: wanted.context_management_beta,
            per_turn_control_beta: wanted.per_turn_control_beta,
            previous_message_id: wanted.previous_message_id.as_deref(),
        };
        let got = evaluate(&facts, &request);
        let (kind, budget, updates) = match got.thinking {
            None => ("absent", None, false),
            Some(Thinking::Disabled) => ("disabled", None, false),
            Some(Thinking::Adaptive { display }) => ("adaptive", None, display.is_some()),
            Some(Thinking::Enabled {
                budget_tokens,
                display,
            }) => ("enabled", Some(budget_tokens), display.is_some()),
        };
        family::FieldOutcome {
            max_tokens: got.max_tokens,
            thinking: family::FieldThinking {
                kind: kind.to_owned(),
                budget,
                updates,
            },
            temperature: got.temperature.map(|n| n.as_u64().unwrap()),
            context_management: got.context_management.is_some(),
            effort: got.effort.map(|effort| effort.as_str().to_owned()),
            diagnostics: family::FieldDiagnostics {
                present: got.diagnostics.is_some(),
                previous_message_id: got
                    .diagnostics
                    .as_ref()
                    .and_then(|d| d.previous_message_id.clone()),
            },
            per_turn_effort: got.per_turn_effort.map(|effort| effort.as_str().to_owned()),
        }
    });
}

fn message(role: Role) -> Message {
    Message {
        role,
        content: MessageContent::Text("x".to_owned()),
        output_config: None,
    }
}

#[test]
fn per_turn_row_placement_matches_lean() {
    gents_lean_vocab::families::claude_prompt_assembly_field_row::replay(|i| {
        let index = row_index(i.is_user.iter().copied());
        let mut messages: Vec<Message> = i
            .is_user
            .iter()
            .map(|&u| message(if u { Role::User } else { Role::Assistant }))
            .collect();
        assert_eq!(
            insert_per_turn_row(&mut messages, Effort::Medium),
            index.is_some()
        );
        if let Some(index) = index {
            assert_eq!(messages.len(), i.is_user.len() + 1);
            assert_eq!(messages[index].role, Role::System);
            assert_eq!(
                messages.iter().filter(|m| m.role == Role::System).count(),
                1
            );
        }
        index.map(|index| index as u64)
    });
}

#[test]
fn opus_5_5_facts() {
    let f = facts_of("claude-opus-5-5");
    assert!(f.adaptive && f.always_thinks && f.rejects_disabled && f.thinking_supported);
    assert!(!f.accepts_temperature);
    assert!(f.effort_capable && f.max_effort && f.xhigh_effort && f.per_turn_effort);
    assert!(!f.caps_effort_when_disabled);
    assert_eq!((f.output_default, f.output_upper), (128_000, 128_000));
    assert_eq!(f.default_effort, Some(Effort::Medium));
}

#[test]
fn haiku_4_5_facts() {
    let f = facts_of("claude-haiku-4-5");
    assert!(!f.adaptive && !f.always_thinks && !f.rejects_disabled && f.thinking_supported);
    assert!(f.accepts_temperature);
    assert!(!f.effort_capable && !f.per_turn_effort);
    assert_eq!((f.output_default, f.output_upper), (32_000, 64_000));
}

#[test]
fn sonnet_4_6_facts() {
    let f = facts_of("claude-sonnet-4-6");
    assert!(f.adaptive && !f.always_thinks && !f.rejects_disabled);
    assert!(f.accepts_temperature);
    assert!(f.effort_capable && f.max_effort && !f.xhigh_effort);
    assert_eq!(f.default_effort, None);
}

#[test]
fn opus_5_caps_effort_when_thinking_is_off() {
    let f = facts_of("claude-opus-5");
    assert!(f.caps_effort_when_disabled && !f.rejects_disabled);
    let request = Request {
        thinking: ThinkingConfig::Disabled { mechanical: false },
        first_party: true,
        requested_effort: Some("max"),
        ..Request::default()
    };
    assert_eq!(thinking(&f, &request), Some(Thinking::Disabled));
    assert_eq!(effort(&f, &request), Some(Effort::High));
}

#[test]
fn claude_3_has_no_thinking_and_takes_temperature() {
    let f = facts_of("claude-3-5-haiku");
    assert!(!f.thinking_supported && f.accepts_temperature && !f.adaptive);
    let request = Request {
        thinking: ThinkingConfig::Enabled { budget: Some(2000) },
        first_party: true,
        ..Request::default()
    };
    assert_eq!(thinking(&f, &request), None);
    assert_eq!(temperature(&f, &request), None);
    assert_eq!(max_tokens(&f, &request), 8192);
}

#[test]
fn catalog_capabilities_agree_with_the_derived_facts() {
    for model in catalog::models() {
        let f = ModelFacts::of(model);
        for (capability, derived) in [
            ("effort", f.effort_capable),
            ("max_effort", f.max_effort),
            ("xhigh_effort", f.xhigh_effort),
            ("adaptive_thinking", f.adaptive),
        ] {
            assert!(
                !model.has(capability) || derived,
                "{} lists {capability} but the derived facts deny it",
                model.id
            );
        }
        assert_eq!(
            f.default_effort.map(Effort::as_str),
            model.default_effort,
            "{}",
            model.id
        );
    }
}

#[test]
fn per_turn_row_is_inserted_after_the_first_user_run() {
    let mut messages = vec![
        message(Role::User),
        message(Role::User),
        message(Role::Assistant),
        message(Role::User),
    ];
    assert!(insert_per_turn_row(&mut messages, Effort::Xhigh));
    let roles: Vec<Role> = messages.iter().map(|m| m.role).collect();
    assert_eq!(
        roles,
        [
            Role::User,
            Role::User,
            Role::System,
            Role::Assistant,
            Role::User
        ]
    );
    assert_eq!(
        messages[2]
            .output_config
            .as_ref()
            .unwrap()
            .effort
            .as_deref(),
        Some("xhigh")
    );
    assert!(matches!(&messages[2].content, MessageContent::Blocks(b) if b.is_empty()));
    let mut none = vec![message(Role::Assistant)];
    assert!(!insert_per_turn_row(&mut none, Effort::Low));
    assert_eq!(none.len(), 1);
}

#[test]
fn an_unlisted_model_takes_the_fallback_limits_and_id_based_facts() {
    let new = ModelFacts::unlisted("claude-sonnet-9");
    assert_eq!((new.output_default, new.output_upper), (32_000, 128_000));
    assert!(new.thinking_supported && new.adaptive && new.effort_capable);
    assert_eq!(new.default_effort, None);
    for (id, limits) in [
        ("claude-3-opus-20240229", (4096, 4096)),
        ("claude-3-haiku-20240307", (4096, 4096)),
        ("claude-3-sonnet-20240229", (8192, 8192)),
    ] {
        let facts = ModelFacts::unlisted(id);
        assert_eq!((facts.output_default, facts.output_upper), limits, "{id}");
        assert!(!facts.thinking_supported && !facts.effort_capable, "{id}");
    }
}

#[test]
fn the_body_follows_the_beta_decisions_on_every_provider() {
    use crate::betas::{self, ModelTraits, Provider};
    let providers = [
        Provider::FirstParty,
        Provider::AnthropicAws,
        Provider::Mantle,
        Provider::Foundry,
        Provider::Vertex,
        Provider::Bedrock,
        Provider::AnthropicGoogleCloud,
        Provider::Gateway,
    ];
    // (model, display on a provider outside upe)
    for (id, outside_upe) in [("claude-opus-4-6", false), ("claude-opus-4-8", true)] {
        let model = catalog::find(id).unwrap();
        let facts = ModelFacts::of(model);
        let traits = ModelTraits {
            canonical: model.id,
            capabilities: model.capabilities,
            native_1m: false,
            thinking_supported: facts.thinking_supported,
        };
        for provider in providers {
            let beta_request = betas::Request {
                provider,
                first_party_base_url: true,
                subscriber: false,
                agentic: true,
                interactive: true,
                show_thinking_summaries: false,
                thinking: true,
                long_context_suffix: false,
                cache_one_hour: false,
                output_format: false,
                experimental_betas_disabled: false,
            };
            let selected = betas::select(&traits, &beta_request);
            let request = Request {
                display_updates: betas::display_updates(&traits, &beta_request),
                context_management_beta: selected.contains(&betas::CONTEXT_MANAGEMENT),
                ..Request::default()
            };
            let upe = matches!(
                provider,
                Provider::FirstParty
                    | Provider::AnthropicAws
                    | Provider::AnthropicGoogleCloud
                    | Provider::Foundry
            );
            let decision = evaluate(&facts, &request);
            let display = match decision.thinking {
                Some(Thinking::Adaptive { display }) => display,
                other => panic!("{id} {provider:?}: {other:?}"),
            };
            assert_eq!(
                display.is_some(),
                upe || outside_upe,
                "{id} {provider:?} display"
            );
            assert_eq!(
                decision.context_management.is_some(),
                provider != Provider::Vertex
                    && provider != Provider::Bedrock
                    && provider != Provider::Gateway,
                "{id} {provider:?} context management"
            );
        }
    }
}
