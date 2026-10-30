use serde_json::Value;

use super::*;
use crate::catalog;
use crate::wire::Thinking;

fn facts_of(id: &str) -> ModelFacts {
    ModelFacts::of(catalog::find(id).unwrap())
}

fn flag(value: &Value, key: &str) -> bool {
    value[key].as_bool().unwrap()
}

fn facts_from(value: &Value) -> ModelFacts {
    ModelFacts {
        adaptive: flag(value, "adaptive"),
        always_thinks: flag(value, "always_thinks"),
        thinking_supported: flag(value, "thinking_supported"),
        rejects_disabled: flag(value, "rejects_disabled"),
        accepts_temperature: flag(value, "accepts_temperature"),
        effort_capable: flag(value, "effort_capable"),
        max_effort: flag(value, "max_effort"),
        xhigh_effort: flag(value, "xhigh_effort"),
        caps_effort_when_disabled: flag(value, "caps_effort_when_disabled"),
        per_turn_effort: flag(value, "per_turn_effort"),
        output_default: value["output_default"].as_u64().unwrap(),
        output_upper: value["output_upper"].as_u64().unwrap(),
        default_effort: value["default_effort"]
            .as_str()
            .map(|name| Effort::parse(name).unwrap()),
    }
}

fn config_from(value: &Value) -> ThinkingConfig {
    match value["kind"].as_str().unwrap() {
        "adaptive" => ThinkingConfig::Adaptive,
        "disabled" => ThinkingConfig::Disabled {
            mechanical: flag(value, "mechanical"),
        },
        "enabled" => ThinkingConfig::Enabled {
            budget: value["budget"].as_u64(),
        },
        other => panic!("unknown thinking kind {other}"),
    }
}

fn request_from(value: &Value) -> Request<'_> {
    Request {
        thinking: config_from(&value["thinking"]),
        max_tokens_override: value["max_tokens_override"].as_u64(),
        requested_effort: value["requested_effort"].as_str(),
        first_party: flag(value, "first_party"),
        first_party_default_url: flag(value, "first_party_default_url"),
        experimental_betas_disabled: flag(value, "experimental_betas_disabled"),
        agentic: flag(value, "agentic"),
        fork: flag(value, "fork"),
        display_updates: flag(value, "display_updates"),
        context_management_beta: flag(value, "context_management_beta"),
        per_turn_control_beta: flag(value, "per_turn_control_beta"),
        previous_message_id: value["previous_message_id"].as_str(),
    }
}

#[test]
fn generated_field_cases_decide_the_body_fields() {
    let contract: Value = gents_lean_contract::load_contract_snapshot().unwrap();
    let cases = contract["prompt_assembly_claude_field_cases"]
        .as_array()
        .unwrap();
    assert_eq!(cases.len(), 768, "the Lean cases cover every gated branch");
    for case in cases {
        let name = &case["name"];
        let got = evaluate(&facts_from(&case["facts"]), &request_from(&case["request"]));
        assert_eq!(
            got.max_tokens,
            case["max_tokens"].as_u64().unwrap(),
            "{name}"
        );
        let want = &case["thinking"];
        let (kind, budget, updates) = match got.thinking {
            None => ("none", None, false),
            Some(Thinking::Disabled) => ("disabled", None, false),
            Some(Thinking::Adaptive { display }) => ("adaptive", None, display.is_some()),
            Some(Thinking::Enabled {
                budget_tokens,
                display,
            }) => ("enabled", Some(budget_tokens), display.is_some()),
        };
        assert_eq!(kind, want["kind"].as_str().unwrap(), "{name}");
        assert_eq!(budget, want["budget"].as_u64(), "{name}");
        assert_eq!(updates, flag(want, "updates"), "{name}");
        assert_eq!(
            got.temperature.map(|n| n.as_u64().unwrap()),
            case["temperature"].as_u64(),
            "{name}"
        );
        assert_eq!(
            got.context_management.is_some(),
            flag(case, "context_management"),
            "{name}"
        );
        assert_eq!(
            got.effort.map(Effort::as_str),
            case["effort"].as_str(),
            "{name}"
        );
        let want = &case["diagnostics"];
        assert_eq!(got.diagnostics.is_some(), flag(want, "present"), "{name}");
        assert_eq!(
            got.diagnostics
                .as_ref()
                .and_then(|d| d.previous_message_id.as_deref()),
            want["previous_message_id"].as_str(),
            "{name}"
        );
        assert_eq!(
            got.per_turn_effort.map(Effort::as_str),
            case["per_turn_effort"].as_str(),
            "{name}"
        );
    }
}

fn message(role: Role) -> Message {
    Message {
        role,
        content: MessageContent::Text("x".to_owned()),
        output_config: None,
    }
}

#[test]
fn generated_field_row_cases_place_the_per_turn_row() {
    let contract: Value = gents_lean_contract::load_contract_snapshot().unwrap();
    let cases = contract["prompt_assembly_claude_field_row_cases"]
        .as_array()
        .unwrap();
    assert_eq!(
        cases.len(),
        31,
        "the Lean cases enumerate every role sequence"
    );
    for case in cases {
        let name = &case["name"];
        let is_user: Vec<bool> = case["is_user"]
            .as_array()
            .unwrap()
            .iter()
            .map(|u| u.as_bool().unwrap())
            .collect();
        let want = case["index"].as_u64().map(|i| i as usize);
        assert_eq!(row_index(is_user.iter().copied()), want, "{name}");
        let mut messages: Vec<Message> = is_user
            .iter()
            .map(|&u| message(if u { Role::User } else { Role::Assistant }))
            .collect();
        assert_eq!(
            insert_per_turn_row(&mut messages, Effort::Medium),
            want.is_some(),
            "{name}"
        );
        if let Some(index) = want {
            assert_eq!(messages.len(), is_user.len() + 1, "{name}");
            assert_eq!(messages[index].role, Role::System, "{name}");
            assert_eq!(
                messages.iter().filter(|m| m.role == Role::System).count(),
                1,
                "{name}"
            );
        }
    }
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
fn title_shape_is_a_mechanical_disable_without_effort() {
    let f = facts_of("claude-haiku-4-5");
    let request = Request {
        thinking: ThinkingConfig::Disabled { mechanical: true },
        first_party: true,
        first_party_default_url: true,
        display_updates: false,
        context_management_beta: true,
        ..Request::default()
    };
    let decision = evaluate(&f, &request);
    assert_eq!(decision.thinking, Some(Thinking::Disabled));
    assert_eq!(
        serde_json::to_string(&decision.thinking).unwrap(),
        r#"{"type":"disabled"}"#
    );
    assert_eq!(decision.temperature, Some(Number::from(1u64)));
    assert_eq!(decision.effort, None);
    assert_eq!(decision.context_management, None);
    assert_eq!(decision.max_tokens, 32_000);
    assert_eq!(decision.diagnostics, None);
}

#[test]
fn main_thread_opus_5_5_shape() {
    let f = facts_of("claude-opus-5-5");
    let request = Request {
        first_party: true,
        first_party_default_url: true,
        agentic: true,
        display_updates: true,
        context_management_beta: true,
        per_turn_control_beta: true,
        ..Request::default()
    };
    let decision = evaluate(&f, &request);
    assert_eq!(
        serde_json::to_string(&decision.thinking).unwrap(),
        r#"{"type":"adaptive","display":"updates"}"#
    );
    assert_eq!(decision.temperature, None);
    assert!(decision.context_management.is_some());
    assert_eq!(decision.effort, Some(Effort::Medium));
    assert_eq!(decision.per_turn_effort, Some(Effort::Medium));
    assert_eq!(
        decision.diagnostics,
        Some(Diagnostics {
            previous_message_id: None
        })
    );
}

#[test]
fn enabled_budget_is_clamped_to_max_tokens_with_a_floor() {
    let f = facts_of("claude-haiku-4-5");
    let mut request = Request {
        thinking: ThinkingConfig::Enabled { budget: None },
        ..Request::default()
    };
    assert_eq!(
        thinking(&f, &request),
        Some(Thinking::Enabled {
            budget_tokens: 31_999,
            display: None
        })
    );
    request.max_tokens_override = Some(500);
    assert_eq!(
        thinking(&f, &request),
        Some(Thinking::Enabled {
            budget_tokens: 1024,
            display: None
        })
    );
    request.max_tokens_override = Some(0);
    assert_eq!(max_tokens(&f, &request), 32_000);
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
