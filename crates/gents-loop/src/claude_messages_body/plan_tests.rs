use gents_claude::attribution;
use gents_claude::betas;
use gents_claude::origin::{Cause, Kind, Origin};
use gents_claude::profile;
use gents_protocol::message::{
    AssistantContent, Message, ToolCall, ToolFunction, ToolResultContent, UserContent,
};
use rig::completion::{CompletionRequest, ToolDefinition};
use serde::de::{Deserializer, IgnoredAny, MapAccess, Visitor};
use serde::Deserialize;
use serde_json::{json, Value};

use super::*;

struct Keys(Vec<String>);

impl<'de> Deserialize<'de> for Keys {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct KeyVisitor;
        impl<'de> Visitor<'de> for KeyVisitor {
            type Value = Keys;

            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("an object")
            }

            fn visit_map<A>(self, mut map: A) -> Result<Keys, A::Error>
            where
                A: MapAccess<'de>,
            {
                let mut keys = Vec::new();
                while let Some(key) = map.next_key::<String>()? {
                    map.next_value::<IgnoredAny>()?;
                    keys.push(key);
                }
                Ok(Keys(keys))
            }
        }
        deserializer.deserialize_map(KeyVisitor)
    }
}

fn top_keys(bytes: &[u8]) -> Vec<String> {
    serde_json::from_slice::<Keys>(bytes).unwrap().0
}

fn request_of(history: Vec<Message>) -> CompletionRequest {
    CompletionRequest {
        model: None,
        preamble: None,
        chat_history: rig::OneOrMany::many(
            history
                .iter()
                .map(crate::rig_compat::to_rig_message)
                .collect::<Vec<_>>(),
        )
        .unwrap(),
        documents: Vec::new(),
        tools: Vec::new(),
        temperature: None,
        max_tokens: Some(512),
        tool_choice: None,
        additional_params: None,
        output_schema: None,
    }
}

fn with_context(
    mut request: CompletionRequest,
    context: &ClaudeRequestContext,
) -> CompletionRequest {
    request.additional_params = Some(json!({ CLAUDE_REQUEST_PARAM: context.to_param().unwrap() }));
    request
}

fn account() -> ClaudeAccountContext {
    ClaudeAccountContext {
        device_id: "ab".repeat(32),
        account_uuid: "acct-1".to_owned(),
        subscriber: true,
        using_overage: false,
        api_key: false,
    }
}

fn origin(cause: Cause, root_client: bool, scheduled: bool, kind: Kind) -> Origin {
    Origin {
        cause,
        root_client,
        scheduled,
        kind,
        appends_system_prompt: true,
    }
}

fn context(origin: Origin) -> ClaudeRequestContext {
    ClaudeRequestContext {
        origin,
        session_key: "session-1".to_owned(),
        prompt_key: "request-doc-1".to_owned(),
        parent_session_id: None,
        previous_request_id: None,
        agent: None,
    }
}

fn header(plan: &ClaudeRequestPlan) -> String {
    plan.body.system[0].text.clone()
}

fn tool_exchange(id: &str) -> [Message; 2] {
    [
        Message::Assistant {
            id: None,
            content: vec![AssistantContent::ToolCall(ToolCall::new(
                id.into(),
                ToolFunction::new("echo".into(), json!({})),
            ))],
        },
        Message::User {
            content: vec![UserContent::tool_result(
                id,
                vec![ToolResultContent::text("ok")],
            )],
        },
    ]
}

#[test]
fn a_turn_body_follows_claude_codes_key_order() {
    let plan = plan_claude_request(
        "claude-sonnet-5",
        &request_of(vec![Message::user("hi")]),
        &account(),
    )
    .unwrap();
    let bytes = gents_claude::wire::to_bytes(&plan.body).unwrap();
    assert_eq!(
        top_keys(&bytes),
        [
            "model",
            "messages",
            "system",
            "tools",
            "metadata",
            "max_tokens",
            "thinking",
            "context_management",
            "output_config",
            "diagnostics",
            "stream"
        ]
    );
    let body: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(body["model"], "claude-sonnet-5");
    assert_eq!(body["max_tokens"], 64_000);
    assert_eq!(body["tools"], json!([]));
    assert_eq!(
        body["thinking"],
        json!({"type": "adaptive", "display": "updates"})
    );
    assert_eq!(body["output_config"], json!({"effort": "high"}));
    assert_eq!(body["diagnostics"], json!({"previous_message_id": null}));
    assert_eq!(
        body["context_management"],
        json!({"edits": [{"type": "clear_thinking_20251015", "keep": "all"}]})
    );
    let fingerprint = attribution::fingerprint("hi", profile::VERSION);
    let ids = ClaudeRequestContext::client_turn("").ids();
    assert_eq!(
        body["system"][0]["text"],
        format!(
            "x-anthropic-billing-header: cc_version=2.1.311.{fingerprint}; cc_entrypoint=cli; cch=00000; cc_prompt_id={}; cc_turn_origin=human; cc_prompt_index=1; cc_turn_index=1;",
            ids.prompt
        )
    );
    assert!(body["system"][0].get("cache_control").is_none());
    assert_eq!(
        body["system"][1]["cache_control"],
        json!({"type": "ephemeral", "ttl": "1h"})
    );
    assert_eq!(
        body["messages"][0]["content"][0]["cache_control"],
        json!({"type": "ephemeral", "ttl": "1h"})
    );
    assert_eq!(
        plan.betas,
        [
            betas::CLAUDE_CODE,
            betas::OAUTH,
            betas::INTERLEAVED_THINKING,
            betas::CONTEXT_MANAGEMENT,
            betas::PROMPT_CACHING_SCOPE,
            betas::THINKING_TOKEN_COUNT,
            betas::EFFORT,
            betas::THINKING_BINDING_CONTROLS,
            betas::THINKING_DISPLAY_UPDATES,
            betas::EXTENDED_CACHE_TTL,
            betas::CACHE_DIAGNOSIS,
        ]
    );
}

#[test]
fn a_title_is_the_main_builder_without_caching_effort_or_diagnostics() {
    let mut request = request_of(vec![Message::user("name this chat")]);
    request.output_schema = Some(schemars::schema_for!(Vec<String>));
    let plan = plan_claude_request(
        "claude-haiku-4-5",
        &with_context(
            request,
            &context(origin(Cause::Client, true, false, Kind::Title)),
        ),
        &account(),
    )
    .unwrap();
    let bytes = gents_claude::wire::to_bytes(&plan.body).unwrap();
    assert_eq!(
        top_keys(&bytes),
        [
            "model",
            "messages",
            "system",
            "tools",
            "metadata",
            "max_tokens",
            "thinking",
            "temperature",
            "output_config",
            "stream"
        ]
    );
    let text = String::from_utf8(bytes).unwrap();
    assert!(!text.contains("cache_control"), "{text}");
    let body: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(body["thinking"], json!({"type": "disabled"}));
    assert_eq!(body["temperature"], 1);
    assert_eq!(body["max_tokens"], 32_000);
    assert!(body["output_config"].get("effort").is_none());
    assert_eq!(body["output_config"]["format"]["type"], "json_schema");
    assert!(plan.betas.contains(&betas::STRUCTURED_OUTPUTS));
    assert!(!plan.betas.contains(&betas::EXTENDED_CACHE_TTL));
    assert!(!header(&plan).contains("cc_turn_origin"));
    assert!(!header(&plan).contains("cc_prompt_index"));
    assert!(plan.mode.request_class == gents_claude::headers::RequestClass::Auxiliary);
}

#[test]
fn prompt_and_turn_index_count_the_prompt_rows_of_the_history() {
    let mut history = vec![Message::user("first")];
    history.extend(tool_exchange("toolu_1"));
    history.push(Message::assistant("done"));
    history.push(Message::user("second"));
    history.extend(tool_exchange("toolu_2"));
    history.push(Message::user("third"));
    let plan = plan_claude_request("claude-opus-5-5", &request_of(history), &account()).unwrap();
    assert!(
        header(&plan).contains(" cc_prompt_index=3; cc_turn_index=3;"),
        "{}",
        header(&plan)
    );
}

#[test]
fn a_compaction_summary_that_lost_the_position_omits_both_indices() {
    let summary =
        crate::prompt::compaction_summary_message(&["# Continuation checkpoint".to_owned()])
            .unwrap();
    let plan = plan_claude_request(
        "claude-opus-5-5",
        &request_of(vec![summary, Message::user("next")]),
        &account(),
    )
    .unwrap();
    assert!(
        !header(&plan).contains("cc_prompt_index"),
        "{}",
        header(&plan)
    );
    assert!(header(&plan).contains("cc_turn_origin=human;"));
}

#[test]
fn system_reminders_are_not_prompts() {
    let reminder = crate::prompt::system_reminder("a skill");
    let plan = plan_claude_request(
        "claude-opus-5-5",
        &request_of(vec![reminder, Message::user("hello")]),
        &account(),
    )
    .unwrap();
    assert!(header(&plan).contains(" cc_prompt_index=1; cc_turn_index=1;"));
    assert!(header(&plan).contains(&attribution::fingerprint("hello", profile::VERSION)));
}

#[test]
fn the_previous_message_id_is_the_last_anthropic_assistant_message() {
    let diagnostics = |ids: [Option<&str>; 2]| {
        let history = vec![
            Message::user("a"),
            Message::Assistant {
                id: ids[0].map(str::to_owned),
                content: vec![AssistantContent::text("one")],
            },
            Message::user("b"),
            Message::Assistant {
                id: ids[1].map(str::to_owned),
                content: vec![AssistantContent::text("two")],
            },
            Message::user("c"),
        ];
        let plan =
            plan_claude_request("claude-opus-5-5", &request_of(history), &account()).unwrap();
        serde_json::to_value(&plan.body).unwrap()["diagnostics"].clone()
    };
    assert_eq!(
        diagnostics([Some("msg_1"), Some("msg_2")]),
        json!({"previous_message_id": "msg_2"})
    );
    assert_eq!(
        diagnostics([Some("msg_1"), Some("resp_x")]),
        json!({"previous_message_id": "msg_1"})
    );
    assert_eq!(
        diagnostics([None, None]),
        json!({"previous_message_id": null})
    );
}

#[test]
fn the_origin_decides_the_wire_mode_and_the_attribution() {
    let plan_of = |origin: Origin, agent: Option<ClaudeAgentIdentity>| {
        let mut context = context(origin);
        context.agent = agent;
        plan_claude_request(
            "claude-opus-5-5",
            &with_context(request_of(vec![Message::user("go")]), &context),
            &account(),
        )
        .unwrap()
    };
    let chat = plan_of(origin(Cause::Client, true, false, Kind::Turn), None);
    assert!(header(&chat).contains("cc_entrypoint=cli;"));
    assert!(header(&chat).contains("cc_turn_origin=human;"));
    assert!(!header(&chat).contains("cc_workload"));
    assert!(chat.agent.is_none());

    let scheduled = plan_of(origin(Cause::TaskTrigger, false, true, Kind::Turn), None);
    assert!(header(&scheduled).contains("cc_entrypoint=sdk-cli;"));
    assert!(header(&scheduled).contains(" cc_workload=cron;"));
    assert!(header(&scheduled)
        .contains("cc_turn_origin=scheduled; cc_prompt_index=0; cc_turn_index=1;"));
    assert!(scheduled.mode.background);

    let unattended = plan_of(origin(Cause::TaskTrigger, false, false, Kind::Turn), None);
    assert!(header(&unattended).contains("cc_turn_origin=sdk;"));

    let identity = ClaudeAgentIdentity {
        agent_id: "worker".to_owned(),
        parent_agent_id: Some("lead".to_owned()),
    };
    for cause in [Cause::AgentNew, Cause::PluginJob] {
        let subagent = plan_of(
            origin(cause, true, false, Kind::Turn),
            Some(identity.clone()),
        );
        assert!(header(&subagent).contains(" cc_is_subagent=true;"));
        assert!(!header(&subagent).contains("cc_turn_origin"));
        assert!(!header(&subagent).contains("cc_prompt_index"));
        assert_eq!(subagent.agent, Some(identity.clone()));
        assert!(subagent.body.system[1]
            .cache_control
            .as_ref()
            .is_some_and(|cc| cc.ttl.is_none()));
    }
    let top_level = plan_of(
        origin(Cause::Client, true, false, Kind::Turn),
        Some(identity.clone()),
    );
    assert!(
        top_level.agent.is_none(),
        "agent headers are for subagent turns only"
    );

    let compaction = plan_of(origin(Cause::Client, true, false, Kind::Compaction), None);
    assert!(!header(&compaction).contains("cc_turn_origin"));
    assert_eq!(
        serde_json::to_value(&compaction.body).unwrap()["diagnostics"],
        json!({"previous_message_id": null})
    );
}

#[test]
fn the_context_never_reaches_the_wire_and_fixes_the_ids() {
    let context = context(origin(Cause::Client, true, false, Kind::Turn));
    let request = with_context(request_of(vec![Message::user("hi")]), &context);
    let plan = plan_claude_request("claude-opus-5-5", &request, &account()).unwrap();
    let text = String::from_utf8(gents_claude::wire::to_bytes(&plan.body).unwrap()).unwrap();
    assert!(!text.contains(CLAUDE_REQUEST_PARAM));
    let ids = context.ids();
    assert_eq!(plan.session_id, ids.session);
    assert_eq!(plan.prompt_id, ids.prompt);
    assert_eq!(
        plan.session_id,
        gents_claude::metadata::stable_uuid(b"session-1")
    );
    assert_eq!(
        plan.prompt_id,
        gents_claude::metadata::stable_uuid(b"request-doc-1")
    );
    assert!(plan.body.metadata.user_id.contains(&ids.session));
    assert!(plan.body.metadata.user_id.contains(&"ab".repeat(32)));
    assert!(plan.body.metadata.user_id.contains("acct-1"));
    assert!(header(&plan).contains(&format!("cc_prompt_id={};", ids.prompt)));
}

#[test]
fn a_request_without_context_is_a_client_turn_of_its_rotation_session() {
    let mut request = request_of(vec![Message::user("hi")]);
    request.additional_params = Some(json!({ SESSION_KEY_PARAM: "rotation-session" }));
    let derived = ClaudeRequestContext::of_request(&request).unwrap();
    assert_eq!(
        derived,
        ClaudeRequestContext::client_turn("rotation-session")
    );
    request.additional_params = Some(json!({ CLAUDE_REQUEST_PARAM: {"origin": 3} }));
    assert!(matches!(
        ClaudeRequestContext::of_request(&request),
        Err(MessagesBodyError::Context(_))
    ));
}

#[test]
fn model_names_resolve_like_claude_code_sends_them() {
    let plan_of = |model: &str| {
        plan_claude_request(model, &request_of(vec![Message::user("hi")]), &account()).unwrap()
    };
    let long = plan_of("claude-opus-4-5[1m]");
    assert_eq!(long.body.model, "claude-opus-4-5");
    assert!(long.betas.contains(&betas::CONTEXT_1M));
    let dated = plan_of("claude-sonnet-4-5-20250929");
    assert_eq!(dated.body.model, "claude-sonnet-4-5-20250929");
    let alias = plan_of("opus");
    assert_eq!(
        alias.body.model,
        gents_claude::catalog::resolve_alias("opus", None)
            .and_then(gents_claude::catalog::find)
            .and_then(|model| model.id_for(gents_claude::catalog::CatalogProvider::FirstParty))
            .unwrap()
    );
    let unlisted = plan_of("claude-future-9");
    assert_eq!(unlisted.body.model, "claude-future-9");
    assert_eq!(unlisted.body.max_tokens, 32_000);
}

#[test]
fn the_requested_effort_follows_the_advertised_efforts() {
    let effort_of = |advertised: Value, effort: &str| {
        let mut request = request_of(vec![Message::user("hi")]);
        request.additional_params = Some(json!({
            ADVERTISED_REASONING_EFFORTS_PARAM: advertised,
            "output_config": {"effort": effort},
        }));
        let plan = plan_claude_request("claude-sonnet-5", &request, &account()).unwrap();
        plan.body.output_config.unwrap().effort
    };
    assert_eq!(
        effort_of(json!(["low", "high"]), "low").as_deref(),
        Some("low")
    );
    assert_eq!(
        effort_of(json!(["low"]), "high").as_deref(),
        Some("high"),
        "falls back to the model default"
    );
    assert_eq!(effort_of(Value::Null, "low").as_deref(), Some("high"));
}

#[test]
fn the_cache_prefix_parameter_moves_the_breakpoint_off_the_final_message() {
    let mut request = request_of(vec![
        Message::user("shared history"),
        Message::assistant("answer"),
        Message::user("final instruction"),
    ]);
    request.additional_params = Some(json!({ CACHE_PREFIX_BREAKPOINT_PARAM: true }));
    let plan = plan_claude_request("claude-opus-5-5", &request, &account()).unwrap();
    let marked: Vec<bool> = plan
        .body
        .messages
        .iter()
        .map(|message| match &message.content {
            gents_claude::wire::MessageContent::Blocks(blocks) => {
                blocks.iter().any(|block| block.cache_control().is_some())
            }
            gents_claude::wire::MessageContent::Text(_) => false,
        })
        .collect();
    // The second row is the per-turn effort control Claude Code inserts after the first prompt.
    assert_eq!(marked, [false, false, true, false]);
}

#[test]
fn an_api_key_account_is_neither_a_subscriber_nor_cached_for_an_hour() {
    let key = ClaudeAccountContext {
        subscriber: false,
        api_key: true,
        ..account()
    };
    let plan = plan_claude_request(
        "claude-opus-5-5",
        &request_of(vec![Message::user("hi")]),
        &key,
    )
    .unwrap();
    assert!(!plan.betas.contains(&betas::OAUTH));
    assert!(!plan.betas.contains(&betas::EXTENDED_CACHE_TTL));
    assert!(plan.body.system[1]
        .cache_control
        .as_ref()
        .is_some_and(|cc| cc.ttl.is_none()));
    let overage = ClaudeAccountContext {
        using_overage: true,
        ..account()
    };
    let plan = plan_claude_request(
        "claude-opus-5-5",
        &request_of(vec![Message::user("hi")]),
        &overage,
    )
    .unwrap();
    assert!(plan.body.system[1]
        .cache_control
        .as_ref()
        .is_some_and(|cc| cc.ttl.is_none()));
}

#[test]
fn tools_are_sent_with_their_schema_bytes() {
    let mut request = request_of(vec![Message::user("hi")]);
    request.tools = vec![ToolDefinition {
        name: "echo".into(),
        description: "echo".into(),
        parameters: json!({"type": "object"}),
    }];
    let plan = plan_claude_request("claude-opus-5-5", &request, &account()).unwrap();
    let body = serde_json::to_value(&plan.body).unwrap();
    assert_eq!(
        body["tools"],
        json!([{"name": "echo", "description": "echo", "input_schema": {"type": "object"}}])
    );
}
