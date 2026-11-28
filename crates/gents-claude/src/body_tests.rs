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
                messages[index]
                    .output_config
                    .as_ref()
                    .and_then(|c| c.effort.as_deref()),
                Some("medium")
            );
            assert!(matches!(&messages[index].content, MessageContent::Blocks(b) if b.is_empty()));
            assert_eq!(
                messages.iter().filter(|m| m.role == Role::System).count(),
                1
            );
        }
        index.map(|index| index as u64)
    });
}

#[test]
fn catalog_model_facts() {
    let opus55 = facts_of("claude-opus-5-5");
    assert!(
        opus55.adaptive
            && opus55.always_thinks
            && opus55.rejects_disabled
            && opus55.thinking_supported
            && !opus55.accepts_temperature
            && opus55.effort_capable
            && opus55.max_effort
            && opus55.xhigh_effort
            && opus55.per_turn_effort
            && !opus55.caps_effort_when_disabled
    );
    assert_eq!(
        (
            opus55.output_default,
            opus55.output_upper,
            opus55.default_effort
        ),
        (128_000, 128_000, Some(Effort::Medium))
    );

    let haiku = facts_of("claude-haiku-4-5");
    assert!(
        !haiku.adaptive
            && !haiku.always_thinks
            && !haiku.rejects_disabled
            && haiku.thinking_supported
            && haiku.accepts_temperature
            && !haiku.effort_capable
            && !haiku.per_turn_effort
    );
    assert_eq!((haiku.output_default, haiku.output_upper), (32_000, 64_000));

    let sonnet = facts_of("claude-sonnet-4-6");
    assert!(
        sonnet.adaptive
            && !sonnet.always_thinks
            && !sonnet.rejects_disabled
            && sonnet.accepts_temperature
            && sonnet.effort_capable
            && sonnet.max_effort
            && !sonnet.xhigh_effort
    );
    assert_eq!(sonnet.default_effort, None);

    let opus5 = facts_of("claude-opus-5");
    assert!(opus5.caps_effort_when_disabled && !opus5.rejects_disabled);
    let capped = Request {
        thinking: ThinkingConfig::Disabled { mechanical: false },
        first_party: true,
        requested_effort: Some("max"),
        ..Request::default()
    };
    assert_eq!(thinking(&opus5, &capped), Some(Thinking::Disabled));
    assert_eq!(effort(&opus5, &capped), Some(Effort::High));

    let c3 = facts_of("claude-3-5-haiku");
    assert!(!c3.thinking_supported && c3.accepts_temperature && !c3.adaptive);
    let enabled = Request {
        thinking: ThinkingConfig::Enabled { budget: Some(2000) },
        first_party: true,
        ..Request::default()
    };
    assert_eq!(thinking(&c3, &enabled), None);
    assert_eq!(temperature(&c3, &enabled), None);
    assert_eq!(max_tokens(&c3, &enabled), 8192);
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
