use super::*;
use std::sync::Arc;
use std::time::Duration;

use crate::llm::message::AssistantContent;
use crate::test_support::first_content;
use crate::{
    config::{
        AgentBehavior, SamplingConfig, DEFAULT_COMPACTION_THRESHOLD, DEFAULT_CONTEXT_WINDOW,
        DEFAULT_DEADLINE_DURATION_SECS, DEFAULT_MAX_OUTPUT_TOKENS, DEFAULT_MAX_TURNS,
        DEFAULT_STREAM_BATCH_MS, DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS,
    },
    identity::{AgentPrincipal, KeyIdentity},
    prompt_context::PromptContextTemplates,
    tool_surface::BehaviorToolConfig,
    watcher::AgentRequest,
    BackendProviderKind, CompactionStrategy,
};

fn test_builder(system_prompt: &str, behavior_name: &str) -> LayeredPromptBuilder {
    LayeredPromptBuilder::for_behavior(
        system_prompt,
        behavior_name,
        &["list_files", "read_file", "bash"],
        true,
        100_000,
        8_192,
        &[],
    )
}

fn user_msg(text: &str) -> Message {
    Message::User {
        content: vec![UserContent::Text(Text {
            text: text.to_string(),
        })],
    }
}

fn assistant_msg(text: &str) -> Message {
    Message::Assistant {
        id: None,
        content: vec![AssistantContent::Text(Text {
            text: text.to_string(),
        })],
    }
}

fn test_identity(name: &str) -> KeyIdentity {
    let path = std::env::temp_dir().join(format!("{name}-{}.key", uuid::Uuid::new_v4()));
    KeyIdentity::load_or_create(path, None).unwrap()
}

fn behavior_with_prompt_context(
    system_context_template: &str,
    request_context_template: &str,
) -> AgentBehavior {
    let identity: Arc<dyn crate::identity::AgentIdentity> =
        Arc::new(test_identity("prompt-context"));
    let agent_did = identity.did().to_string();
    let principal = Arc::new(AgentPrincipal {
        agent_did,
        identity,
        default_behavior_id: "general".to_string(),
        display_name: None,
        enabled: true,
    });
    AgentBehavior {
        skills: Vec::new(),
        behavior_id: "general".to_string(),
        principal,
        backend_id: None,
        backend_provider_kind: BackendProviderKind::OpenAiCompatible,
        backend_endpoint: "http://localhost:8000/v1".to_string(),
        backend_api_key: None,
        backend_api_key_env_var: None,
        model_name: "test-model".to_string(),
        context_window: DEFAULT_CONTEXT_WINDOW,
        max_output_tokens: DEFAULT_MAX_OUTPUT_TOKENS,
        max_turns: DEFAULT_MAX_TURNS,
        system_prompt: "Be helpful.".to_string(),
        prompt_context: PromptContextTemplates::new(
            Some(system_context_template),
            Some(request_context_template),
        )
        .unwrap(),
        tools: BehaviorToolConfig::meta_only(),
        compaction_threshold: DEFAULT_COMPACTION_THRESHOLD,
        compaction_strategy: CompactionStrategy::StripThenSummarize,
        stream_batch_ms: DEFAULT_STREAM_BATCH_MS,
        stream_liveness_timeout: Duration::from_secs(DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS),
        deadline_duration: Duration::from_secs(DEFAULT_DEADLINE_DURATION_SECS),
        sampling: SamplingConfig::default(),
    }
}

#[test]
fn preamble_combines_prompt_and_behavior_name() {
    let preamble = build_preamble(
        "You are a helpful assistant.",
        "research",
        &["list_files"],
        true,
    );
    assert!(preamble.contains("You are a helpful assistant."));
    assert!(preamble.contains("You are the research agent."));
    assert!(preamble.contains("## Tool Discovery"));
    assert!(preamble.contains("discover_tools"));
    assert!(preamble.contains("describe_tool"));
    assert!(preamble.contains("call_tool"));
    assert!(preamble.contains("native direct tools"));
    assert!(preamble.contains("synthetic `native` service"));
    assert!(preamble.contains("list_files"));
}

#[test]
fn preamble_handles_empty_system_prompt() {
    let preamble = build_preamble("", "general", &["bash"], true);
    assert!(preamble.contains("You are the general agent."));
    assert!(preamble.contains("## Tool Discovery"));
}

#[test]
fn preamble_handles_empty_behavior_name() {
    let preamble = build_preamble("Be helpful.", "", &[], true);
    assert!(preamble.contains("Be helpful."));
    assert!(preamble.contains("## Tool Discovery"));
}

#[test]
fn preamble_strips_title_generation_suffix() {
    let preamble = build_preamble(
        "You are a policy agent.\n\nGenerate concise conversation titles. Return only a lowercase hyphenated 3-5 word title. Never call tools. Never explain.",
        "operator",
        &["bash"],
        true,
    );
    assert!(preamble.contains("You are a policy agent."));
    assert!(!preamble.contains("Generate concise conversation titles."));
    assert!(!preamble.contains("Never call tools. Never explain."));
}

#[test]
fn preamble_is_frozen() {
    let builder = test_builder("System prompt v1.", "test");

    assert_eq!(builder.preamble(), builder.preamble());
    assert!(builder.preamble().contains("System prompt v1."));
}

#[tokio::test]
async fn prompt_context_keeps_dynamic_values_out_of_cached_preamble() {
    let behavior = behavior_with_prompt_context(
        "Agent {{ agent.did }} runs {{ behavior.id }} on {{ model.name }}.",
        "Request {{ request.id }} has {{ request.metadata.kind }} at {{ time.utc_now }}.",
    );
    let node = defra_node::EmbeddedNode::builder().build().await.unwrap();
    crate::ensure_runtime_schemas(&node).await.unwrap();
    let tool_surface = behavior.tools.resolve(&node).await.unwrap();
    let builder = LayeredPromptBuilder::new(&behavior, &tool_surface, &[]).unwrap();

    assert!(builder.preamble().contains("## Runtime Context"));
    assert!(builder.preamble().contains(behavior.agent_did()));
    assert!(builder.preamble().contains("runs general on test-model"));
    assert!(!builder.preamble().contains("req-123"));

    let request = AgentRequest {
        doc_id: "doc-1".to_string(),
        request_id: "req-123".to_string(),
        agent_did: behavior.agent_did().to_string(),
        behavior_id: Some(behavior.behavior_id.clone()),
        session_id: "session-1".to_string(),
        content: "hello".to_string(),
        temperature: None,
        top_p: None,
        top_k: None,
        max_tokens: None,
        metadata: Some(r#"{"kind":"manual"}"#.to_string()),
        execution_origin: None,
        created_at: "2026-01-02T03:04:00Z".to_string(),
        deadline: None,
        subagent_depth: 0,
        caused_by_parent_request_id: None,
        caused_by_parent_tool_call_id: None,
    };
    let now = chrono::DateTime::parse_from_rfc3339("2026-01-02T03:04:05Z")
        .unwrap()
        .with_timezone(&chrono::Utc);
    let reminders = builder
        .request_context_reminders_at(&request, now)
        .expect("request context should render");

    assert_eq!(reminders.len(), 1);
    let Message::User { content } = &reminders[0] else {
        panic!("expected user message");
    };
    let UserContent::Text(text) = first_content(content) else {
        panic!("expected text");
    };
    assert!(text.text.contains("<system-reminder>"));
    assert!(text.text.contains("Request context:"));
    assert!(text.text.contains("Request req-123 has manual"));
    assert!(text.text.contains("2026-01-02T03:04:05+00:00"));
}

#[tokio::test]
async fn build_without_summaries() {
    let builder = test_builder("Be helpful.", "general");

    let messages = vec![user_msg("hello"), assistant_msg("hi")];
    let prompt = builder.build(&messages, &[]).await.unwrap();

    assert_eq!(prompt.messages.len(), 2);
    assert!(prompt.estimated_tokens > 0);
    assert!(prompt.preamble.contains("Be helpful."));
}

#[tokio::test]
async fn build_with_summaries_prepends() {
    let builder = test_builder("Be helpful.", "general");

    let messages = vec![user_msg("what were we discussing?")];
    let summaries = vec!["We discussed project architecture.".to_string()];
    let prompt = builder.build(&messages, &summaries).await.unwrap();

    assert_eq!(prompt.messages.len(), 2);

    if let Message::User { content } = &prompt.messages[0] {
        if let UserContent::Text(t) = first_content(&content) {
            assert!(t.text.contains("<system-reminder>"));
            assert!(t.text.contains("project architecture"));
        } else {
            panic!("expected text");
        }
    } else {
        panic!("expected user message");
    }
}

#[test]
fn system_reminder_format() {
    let msg = LayeredPromptBuilder::system_reminder("The time is 3pm.");
    if let Message::User { content } = &msg {
        if let UserContent::Text(t) = first_content(&content) {
            assert!(t.text.starts_with("<system-reminder>"));
            assert!(t.text.ends_with("</system-reminder>"));
            assert!(t.text.contains("The time is 3pm."));
        } else {
            panic!("expected text");
        }
    } else {
        panic!("expected user message");
    }
}

#[test]
fn message_budget_accounts_for_preamble_and_output() {
    let builder = LayeredPromptBuilder::for_behavior(
        &"x".repeat(4000),
        "general",
        &["list_files", "read_file", "bash"],
        true,
        10_000,
        2_000,
        &[],
    );

    let budget = builder.message_budget();
    assert!(budget < 10000);
    assert!(budget > 5000);
}

#[test]
fn would_exceed_budget_short_messages() {
    let builder = test_builder("Be helpful.", "general");

    let messages = vec![user_msg("hi")];
    assert!(!builder.would_exceed_budget(&messages));
}

#[test]
fn would_exceed_budget_long_messages() {
    let builder = LayeredPromptBuilder::for_behavior(
        "Be helpful.",
        "general",
        &["list_files", "read_file", "bash"],
        true,
        100,
        50,
        &[],
    );

    let big = user_msg(&"x".repeat(10000));
    assert!(builder.would_exceed_budget(&[big]));
}

#[test]
fn preamble_lists_allowed_subagent_targets() {
    let targets = vec![
        (
            "code-reviewer".to_string(),
            "Reviews code for correctness and style.".to_string(),
        ),
        (
            "data-analyst".to_string(),
            "Analyzes datasets and produces summaries.".to_string(),
        ),
    ];
    let preamble = build_preamble_with_targets(
        "You are a coordinator.",
        "orchestrator",
        &["spawn_subagent"],
        false,
        &targets,
    );
    assert!(
        preamble.contains("code-reviewer"),
        "preamble should contain target id 'code-reviewer'"
    );
    assert!(
        preamble.contains("Reviews code for correctness and style."),
        "preamble should contain code-reviewer description"
    );
    assert!(
        preamble.contains("data-analyst"),
        "preamble should contain target id 'data-analyst'"
    );
    assert!(
        preamble.contains("Analyzes datasets and produces summaries."),
        "preamble should contain data-analyst description"
    );
    assert!(
        preamble.contains("spawn_subagent"),
        "preamble should reference the spawn_subagent tool"
    );
}

#[test]
fn preamble_no_targets_block_when_empty() {
    let preamble = build_preamble_with_targets(
        "You are a standalone agent.",
        "standalone",
        &["bash"],
        false,
        &[],
    );
    // Should not contain any subagent section heading
    assert!(
        !preamble.contains("Spawnable Sub-Agents"),
        "preamble should have no subagent section when targets is empty"
    );
    assert!(
        !preamble.contains("spawn_subagent"),
        "preamble should not mention spawn_subagent when there are no targets"
    );
}
