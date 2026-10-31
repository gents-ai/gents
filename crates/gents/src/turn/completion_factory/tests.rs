use super::*;
use std::sync::Arc;
use std::time::Duration;

use crate::inference::resolved::SamplingConfig;
use crate::request::watcher::AgentRequest;
use crate::runtime::agent::completion_retry::CompletionRetryProfileFields;
use crate::store::identity::{KeyIdentity, NodeIdentity, RuntimeNode};
use crate::tools::surface::AgentToolSurfaceConfig;

fn request() -> AgentRequest {
    AgentRequest {
        retry_parent_request_doc_id: None,
        purpose: gents_protocol::request_admission::RequestPurpose::Normal,
        doc_id: String::new(),
        request_id: "request-123".to_string(),
        node_did: String::new(),
        requester_did: None,
        agent_id: "agent-test".to_owned(),
        session_id: "session-456".to_string(),
        content: String::new(),
        max_total_tokens: None,
        input: Default::default(),
        execution_origin: Some("interactive".to_string()),
        created_at: String::new(),
        deadline: None,
        execution_generation: None,
        execution_lease_expires_at: None,
        execution_lease_secs: None,
        request_hop: 0,
        caused_by_parent_request_id: None,
        caused_by_parent_request_doc_id: None,
        caused_by_parent_tool_call_id: None,
        caused_by_parent_tool_call_doc_id: None,
        caused_by_trigger_id: None,
        caused_by_trigger_kind: None,
        caused_by_source_doc_id: None,
        caused_by_correlation: None,
        caused_by_trigger_context: None,
        workspace_id: None,
        workspace_owner_node_did: None,
        workspace_authority: None,
        workspace_seal_hash: None,
    }
}

#[test]
fn claude_profile_effort_reaches_messages_without_sampling() {
    assert_eq!(
        reasoning_profile_params(
            BackendProviderKind::ClaudeCliSubscription,
            crate::OpenAiWireApi::ChatCompletions,
            Some(ReasoningEffort::High),
        ),
        Some(serde_json::json!({"output_config": {"effort": "high"}}))
    );
}

#[test]
fn absent_profile_effort_does_not_inject_reasoning() {
    assert!(reasoning_profile_params(
        BackendProviderKind::OpenAiCompatible,
        crate::OpenAiWireApi::ChatCompletions,
        None,
    )
    .is_none());
    assert!(reasoning_profile_params(
        BackendProviderKind::OpenAiCompatible,
        crate::OpenAiWireApi::Responses,
        None,
    )
    .is_none());
    assert!(reasoning_profile_params(
        BackendProviderKind::OpenRouter,
        crate::OpenAiWireApi::ChatCompletions,
        None,
    )
    .is_none());
}

#[test]
fn absent_profile_effort_keeps_the_codex_medium_default() {
    assert_eq!(
        reasoning_profile_params(
            BackendProviderKind::ChatGptCodex,
            crate::OpenAiWireApi::Responses,
            None,
        ),
        Some(serde_json::json!({ "reasoning": { "effort": "medium" } })),
    );
    assert_eq!(
        reasoning_profile_params(
            BackendProviderKind::ChatGptCodex,
            crate::OpenAiWireApi::Responses,
            Some(ReasoningEffort::High),
        ),
        Some(serde_json::json!({ "reasoning": { "effort": "high" } })),
    );
}

#[test]
fn generic_responses_request_does_not_force_reasoning() {
    use rig::completion::message::{Message, Text, UserContent};
    use rig::one_or_many::OneOrMany;

    let core_req = rig::completion::CompletionRequest {
        model: None,
        preamble: None,
        chat_history: OneOrMany::one(Message::User {
            content: OneOrMany::one(UserContent::Text(Text {
                text: "hi".to_string(),
            })),
        }),
        documents: Vec::new(),
        tools: Vec::new(),
        temperature: Some(0.0),
        max_tokens: None,
        tool_choice: None,
        additional_params: reasoning_profile_params(
            BackendProviderKind::OpenAiCompatible,
            crate::OpenAiWireApi::Responses,
            None,
        ),
        output_schema: None,
    };

    let openai_req = rig::providers::openai::responses_api::CompletionRequest::try_from((
        "gpt-5.4-mini".to_string(),
        core_req,
    ))
    .expect("OpenAI Responses request conversion should succeed");
    let body = serde_json::to_value(&openai_req).expect("serializing OpenAI Responses request");

    assert_eq!(body["temperature"], 0.0);
    assert!(body.get("reasoning").is_none());
    assert!(body.get("chat_template_kwargs").is_none());
}

#[test]
fn responses_and_openrouter_use_profile_reasoning_effort() {
    let codex = reasoning_profile_params(
        BackendProviderKind::ChatGptCodex,
        crate::OpenAiWireApi::Responses,
        Some(crate::inference::resolved::ReasoningEffort::Ultra),
    )
    .expect("profile reasoning effort must be present");
    let responses = reasoning_profile_params(
        BackendProviderKind::OpenAiCompatible,
        crate::OpenAiWireApi::Responses,
        Some(crate::inference::resolved::ReasoningEffort::XHigh),
    )
    .expect("profile reasoning effort must be present");
    let openrouter = reasoning_profile_params(
        BackendProviderKind::OpenRouter,
        crate::OpenAiWireApi::ChatCompletions,
        Some(crate::inference::resolved::ReasoningEffort::Medium),
    )
    .expect("profile reasoning effort must be present");

    assert_eq!(codex["reasoning"]["effort"], "ultra");
    assert_eq!(responses["reasoning"]["effort"], "xhigh");
    assert_eq!(openrouter["reasoning"]["effort"], "medium");
    assert!(codex.get("chat_template_kwargs").is_none());
}

#[test]
fn profile_none_disables_local_thinking() {
    let value = reasoning_profile_params(
        BackendProviderKind::OpenAiCompatible,
        crate::OpenAiWireApi::ChatCompletions,
        Some(crate::inference::resolved::ReasoningEffort::None),
    )
    .expect("explicit none must be present");

    assert_eq!(value["chat_template_kwargs"]["enable_thinking"], false);
    assert!(value["chat_template_kwargs"]
        .get("reasoning_effort")
        .is_none());
}

#[test]
fn profile_reasoning_serializes_top_level_into_openai_body() {
    use rig::completion::message::{Message, Text, UserContent};
    use rig::one_or_many::OneOrMany;

    let additional_params = merge_optional_params(
        merge_optional_params(
            reasoning_profile_params(
                BackendProviderKind::OpenAiCompatible,
                crate::OpenAiWireApi::ChatCompletions,
                Some(crate::inference::resolved::ReasoningEffort::Max),
            ),
            provider_additional_params(
                BackendProviderKind::OpenAiCompatible,
                crate::OpenAiWireApi::ChatCompletions,
                crate::inference::backends::setup::OPENAI_ENDPOINT,
            ),
        ),
        SamplingConfig::default().additional_params(),
    );

    let core_req = rig::completion::CompletionRequest {
        model: None,
        preamble: None,
        chat_history: OneOrMany::one(Message::User {
            content: OneOrMany::one(UserContent::Text(Text {
                text: "hi".to_string(),
            })),
        }),
        documents: Vec::new(),
        tools: Vec::new(),
        temperature: None,
        max_tokens: None,
        tool_choice: None,
        additional_params,
        output_schema: None,
    };

    let openai_req = rig::providers::openai::CompletionRequest::try_from((
        "fixture-model".to_string(),
        core_req,
    ))
    .expect("openai request conversion should succeed");
    let body = serde_json::to_value(&openai_req).expect("serializing openai request");

    assert_eq!(
        body["chat_template_kwargs"]["enable_thinking"], true,
        "request body must carry top-level chat_template_kwargs.enable_thinking=true; body was {body}"
    );
    assert_eq!(body["chat_template_kwargs"]["reasoning_effort"], "max");
}

#[test]
fn openrouter_additional_params_require_parameters() {
    let value = provider_additional_params(
        BackendProviderKind::OpenRouter,
        crate::OpenAiWireApi::Responses,
        crate::inference::backends::setup::OPENAI_ENDPOINT,
    )
    .expect("OpenRouter should contribute additional params");

    assert_eq!(
        value,
        rig::providers::openrouter::ProviderPreferences::new()
            .require_parameters(true)
            .to_json()
    );
}

#[test]
fn generated_responses_storage_cases_drive_loop_config() {
    use crate::lean_vocab_test::lean_prompt_assembly_responses_storage_cases;

    let cases = lean_prompt_assembly_responses_storage_cases();
    assert!(!cases.is_empty());
    let mut mismatches = Vec::new();
    for case in cases {
        let mut agent = agent_with_retry(Default::default());
        agent.backend_provider_kind =
            BackendProviderKind::parse_optional(Some(&case.family)).expect("Lean family");
        agent.openai_wire_api =
            serde_json::from_value(serde_json::Value::String(case.wire.clone()))
                .expect("Lean wire");
        agent.backend_endpoint = case.endpoint.clone();
        let config = loop_config(
            &agent,
            "preamble".to_string(),
            0,
            CaptureScopeKind::Inference,
        );
        let request = rig::completion::CompletionRequest {
            model: None,
            preamble: config.preamble.clone(),
            chat_history: rig::one_or_many::OneOrMany::one(rig::completion::Message::user("hi")),
            documents: Vec::new(),
            tools: Vec::new(),
            temperature: None,
            max_tokens: None,
            tool_choice: None,
            additional_params: config.additional_params.clone(),
            output_schema: None,
        };
        let body = config
            .provider_input_counter
            .project_body(&request)
            .expect("project body");
        let store = body.get("store").and_then(serde_json::Value::as_bool);
        let encrypted_include = body
            .get("include")
            .and_then(serde_json::Value::as_array)
            .is_some_and(|include| {
                include.iter().any(|item| {
                    item.as_str() == Some(gents_loop::provider_patches::ENCRYPTED_REASONING_INCLUDE)
                })
            });
        if (store, encrypted_include) != (case.store, case.encrypted_include) {
            mismatches.push(format!(
                "{}: store {store:?} include {encrypted_include}",
                case.name
            ));
        }
    }
    assert!(mismatches.is_empty(), "{mismatches:#?}");
}

#[test]
fn generated_responses_effort_cases_drive_loop_config() {
    use crate::inference::resolved::ReasoningEffort;
    use crate::lean_vocab_test::lean_prompt_assembly_responses_effort_cases;

    let parse = |value: &String| ReasoningEffort::parse(value).expect("Lean effort");
    let cases = lean_prompt_assembly_responses_effort_cases();
    assert!(!cases.is_empty());
    let mut mismatches = Vec::new();
    for case in cases {
        let mut agent = agent_with_retry(Default::default());
        agent.backend_provider_kind = BackendProviderKind::OpenAiCompatible;
        agent.openai_wire_api = crate::OpenAiWireApi::Responses;
        agent.backend_endpoint = case.endpoint.clone();
        agent.sampling.reasoning_effort = case.requested.as_ref().map(parse);
        agent.resolved_reasoning_efforts = case
            .advertised
            .as_ref()
            .map(|values| values.iter().map(parse).collect());
        let config = loop_config(
            &agent,
            "preamble".to_string(),
            0,
            CaptureScopeKind::Inference,
        );
        let request = rig::completion::CompletionRequest {
            model: None,
            preamble: config.preamble.clone(),
            chat_history: rig::one_or_many::OneOrMany::one(rig::completion::Message::user("hi")),
            documents: Vec::new(),
            tools: Vec::new(),
            temperature: None,
            max_tokens: None,
            tool_choice: None,
            additional_params: config.additional_params.clone(),
            output_schema: None,
        };
        let body = config
            .provider_input_counter
            .project_body(&request)
            .expect("project body");
        let sent = body
            .pointer("/reasoning/effort")
            .and_then(serde_json::Value::as_str);
        if sent != case.expected.as_deref() {
            mismatches.push(format!("{}: effort {sent:?}", case.name));
        }
    }
    assert!(mismatches.is_empty(), "{mismatches:#?}");
}

#[test]
fn sampling_additional_params_merge_with_provider_params() {
    let sampling = SamplingConfig {
        temperature: Some(0.1),
        top_p: Some(0.95),
        top_k: Some(40),
        max_tokens: Some(1024),
        ..Default::default()
    };

    let value = merge_optional_params(
        provider_additional_params(
            BackendProviderKind::OpenRouter,
            crate::OpenAiWireApi::Responses,
            crate::inference::backends::setup::OPENAI_ENDPOINT,
        ),
        sampling.additional_params(),
    )
    .expect("sampling params should be present");

    assert_eq!(value["provider"]["require_parameters"], true);
    assert_eq!(value["top_p"], 0.95);
    assert_eq!(value["top_k"], 40);
    assert!(value.get("max_tokens").is_none());
    assert!(value.get("temperature").is_none());
}

#[test]
fn openai_cache_scope_prefers_session_id() {
    let value = openai_cache_scope_params(&request()).expect("scope should be present");

    assert_eq!(value["user"], "session-456");
}

#[test]
fn openai_cache_scope_falls_back_to_request_id() {
    let mut request = request();
    request.session_id.clear();

    let value = openai_cache_scope_params(&request).expect("fallback scope should be present");

    assert_eq!(value["user"], "request-123");
}

#[test]
fn codex_cache_key_stays_with_session_across_requests() {
    let mut agent = agent_with_retry(Default::default());
    agent.backend_provider_kind = BackendProviderKind::ChatGptCodex;
    agent.openai_wire_api = crate::OpenAiWireApi::Responses;
    let mut request = request();
    let first = request_additional_params(&agent, &request).unwrap();
    let config = loop_config_for_request(&agent, "system".into(), &request, None, 0).unwrap();
    let params = config.additional_params.unwrap();
    assert_eq!(params["prompt_cache_key"], first["prompt_cache_key"]);
    assert_eq!(params["reasoning"]["effort"], "medium");
    request.request_id = "next-request".to_string();
    assert_eq!(
        request_additional_params(&agent, &request),
        Some(first.clone())
    );
    assert_eq!(
        first,
        serde_json::json!({"prompt_cache_key": "session-456"})
    );
    request.session_id = "another-session".to_string();
    assert_ne!(request_additional_params(&agent, &request), Some(first));
}

#[test]
fn codex_cache_key_uses_request_when_session_is_absent() {
    let mut agent = agent_with_retry(Default::default());
    agent.backend_provider_kind = BackendProviderKind::ChatGptCodex;
    let mut request = request();
    request.session_id = "  ".to_string();
    assert_eq!(
        request_additional_params(&agent, &request),
        Some(serde_json::json!({"prompt_cache_key": "request-123"}))
    );
    request.request_id.clear();
    assert!(request_additional_params(&agent, &request).is_none());
}

#[test]
fn loop_config_for_request_resolves_completion_retry_policy_and_deadline() {
    let agent = agent_with_retry(CompletionRetryProfileFields {
        retry_interactive_max: Some(2),
        ..Default::default()
    });
    let mut request = request();
    request.execution_origin = Some("interactive".to_string());
    request.deadline = Some("2030-01-01T00:00:00Z".to_string());

    let config =
        loop_config_for_request(&agent, "preamble".to_string(), &request, None, 0).unwrap();

    assert_eq!(
        config.retry_policy.transport_backoff,
        vec![Duration::from_secs(2), Duration::from_secs(2)]
    );
    assert_eq!(
        config.deadline.map(|deadline| deadline.timestamp()),
        Some(1_893_456_000)
    );
}

#[test]
fn loop_config_for_request_rejects_unknown_retry_origin() {
    let agent = agent_with_retry(CompletionRetryProfileFields::default());
    let mut request = request();
    request.execution_origin = Some("legacy-or-missing".to_string());

    let Err(error) = loop_config_for_request(&agent, "preamble".to_string(), &request, None, 0)
    else {
        panic!("unknown execution origin must fail closed");
    };
    assert!(error.to_string().contains("unknown execution_origin"));
}

#[test]
fn profile_seed_rejects_provider_paths_without_seed_support() {
    let sampling = SamplingConfig {
        seed: Some(1234),
        ..Default::default()
    };

    sampling
        .validate_for_provider(
            BackendProviderKind::OpenAiCompatible,
            crate::OpenAiWireApi::ChatCompletions,
        )
        .unwrap();
    sampling
        .validate_for_provider(
            BackendProviderKind::OpenRouter,
            crate::OpenAiWireApi::ChatCompletions,
        )
        .unwrap();
    assert!(sampling
        .validate_for_provider(
            BackendProviderKind::ChatGptCodex,
            crate::OpenAiWireApi::Responses,
        )
        .is_err());
    assert!(sampling
        .validate_for_provider(
            BackendProviderKind::XaiGrokOAuth,
            crate::OpenAiWireApi::ChatCompletions,
        )
        .is_err());
    assert!(sampling
        .validate_for_provider(
            BackendProviderKind::ClaudeCliSubscription,
            crate::OpenAiWireApi::ChatCompletions,
        )
        .is_err());
}

fn agent_with_retry(completion_retry: CompletionRetryProfileFields) -> ResolvedAgent {
    let identity = Arc::new(
        KeyIdentity::load_or_create(
            std::env::temp_dir().join(format!("completion-factory-{}.key", uuid::Uuid::new_v4())),
            None,
        )
        .unwrap(),
    );
    let node = Arc::new(RuntimeNode {
        node_did: identity.did().to_string(),
        identity,
        default_agent_id: "general".to_string(),
        display_name: None,
        enabled: true,
    });

    ResolvedAgent {
        profile_id: None,
        agent_id: "general".to_string(),
        node,
        backend_id: Some("backend-general".to_string()),
        backend_provider_kind: BackendProviderKind::OpenAiCompatible,
        openai_wire_api: crate::OpenAiWireApi::ChatCompletions,
        backend_endpoint: "http://127.0.0.1:8999/v1".to_string(),
        backend_auth: crate::documents::BackendAuth::Unauthenticated,
        model_name: crate::inference::resolved::DEFAULT_MODEL_NAME.to_string(),
        service_tier: None,
        resolved_reasoning_efforts: None,
        context_window: crate::inference::resolved::DEFAULT_CONTEXT_WINDOW,
        max_output_tokens: crate::inference::resolved::DEFAULT_MAX_OUTPUT_TOKENS,
        max_turns: crate::inference::resolved::DEFAULT_MAX_TURNS,
        max_turns_provenance: crate::inference::resolved::MaxTurnsProvenance::Default,
        system_prompt: "system".to_string(),
        tools: AgentToolSurfaceConfig::meta_only(),
        compaction: None,
        compaction_inference: None,
        max_total_tokens: None,
        stream_batch_ms: crate::inference::resolved::DEFAULT_STREAM_BATCH_MS,
        stream_liveness_timeout: Duration::from_secs(
            crate::inference::resolved::DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS,
        ),
        deadline_duration: Duration::from_secs(
            crate::inference::resolved::DEFAULT_DEADLINE_DURATION_SECS,
        ),
        provider_idle_timeout: Duration::from_secs(
            crate::inference::resolved::DEFAULT_PROVIDER_IDLE_TIMEOUT_SECS,
        ),
        completion_retry,
        sampling: SamplingConfig::default(),
        skills: Vec::new(),
    }
}

#[test]
fn claude_advertised_efforts_override_spoofed_additional_params() {
    let spoofed = Some(serde_json::json!({
        (crate::inference::clients::claude_messages::ADVERTISED_REASONING_EFFORTS_PARAM): ["high"]
    }));
    let unknown = super::authoritative_reasoning_capabilities(
        spoofed.clone(),
        BackendProviderKind::ClaudeCliSubscription,
        None,
    )
    .unwrap();
    assert!(unknown
        [crate::inference::clients::claude_messages::ADVERTISED_REASONING_EFFORTS_PARAM]
        .is_null());

    let advertised = super::authoritative_reasoning_capabilities(
        spoofed,
        BackendProviderKind::ClaudeCliSubscription,
        Some(&[ReasoningEffort::Low, ReasoningEffort::XHigh]),
    )
    .unwrap();
    assert_eq!(
        advertised[crate::inference::clients::claude_messages::ADVERTISED_REASONING_EFFORTS_PARAM],
        serde_json::json!(["low", "xhigh"])
    );
}

#[test]
fn runtime_budget_construction_preserves_zero_and_rejects_negative_values() {
    let agent = agent_with_retry(CompletionRetryProfileFields::default());
    let invalid = -1;
    let error = parse_aggregate_token_limit(Some(invalid))
        .expect_err("negative aggregate budget must be rejected");
    assert!(error.to_string().contains("must not be negative"));

    assert_eq!(parse_aggregate_token_limit(None).unwrap(), None);
    assert_eq!(parse_aggregate_token_limit(Some(0)).unwrap(), Some(0));
    assert_eq!(
        parse_aggregate_token_limit(Some(4_096)).unwrap(),
        Some(4_096)
    );

    let mut request = request();
    request.max_total_tokens = Some(4_096);
    let budget = Some(AggregateTokenBudget::new(4_096));
    assert!(
        loop_config_for_request(&agent, "system".to_string(), &request, budget, 0)
            .unwrap()
            .aggregate_token_budget
            .is_some()
    );
}

#[test]
fn prior_usage_blocks_dispatch_when_already_at_limit() {
    let budget = AggregateTokenBudget::with_prior_usage(1_000, 1_000);
    let ledger = budget.snapshot().expect("ledger lock");
    assert_eq!(ledger.remaining(), 0);
    assert!(!ledger.can_dispatch(1, 100));
}

#[test]
fn prior_usage_decode_fails_closed_on_malformed_payload() {
    let err = super::prior_usage_rows_from_response(Some(&serde_json::json!({"not": "an-array"})))
        .expect_err("object payload must not decode as empty usage");
    assert!(
        err.to_string().contains("not a row array"),
        "unexpected error: {err}"
    );
    assert!(super::prior_usage_rows_from_response(None)
        .unwrap()
        .is_empty());
    assert!(
        super::prior_usage_rows_from_response(Some(&serde_json::Value::Null))
            .unwrap()
            .is_empty()
    );
    let rows = super::prior_usage_rows_from_response(Some(&serde_json::json!([
        {"prompt_tokens": 100, "completion_tokens": 50},
        {"prompt_tokens": 200, "completion_tokens": 10},
    ])))
    .unwrap();
    assert_eq!(
        crate::provider_usage::sum_charged_from_persisted_parts(
            rows.into_iter()
                .map(|row| (row.prompt_tokens, row.completion_tokens))
        )
        .unwrap(),
        360
    );
}

#[test]
fn prior_usage_decode_rejects_partial_or_negative_components() {
    for payload in [
        serde_json::json!([{"prompt_tokens": 100, "completion_tokens": null}]),
        serde_json::json!([{"prompt_tokens": null, "completion_tokens": 50}]),
        serde_json::json!([{"prompt_tokens": -1, "completion_tokens": 50}]),
        serde_json::json!([{"prompt_tokens": 100, "completion_tokens": -1}]),
    ] {
        let rows = super::prior_usage_rows_from_response(Some(&payload)).unwrap();
        assert!(
            crate::provider_usage::sum_charged_from_persisted_parts(
                rows.into_iter()
                    .map(|row| (row.prompt_tokens, row.completion_tokens))
            )
            .is_err(),
            "invalid durable usage must fail closed: {payload}"
        );
    }
}

#[tokio::test]
async fn rehydrates_aggregate_budget_from_durable_inference_calls() {
    let cases = crate::lean_vocab_test::lean_budget_rehydration_cases();
    assert_eq!(
        cases.len(),
        5,
        "all nullable restart witnesses must be exercised"
    );
    for case in cases {
        let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
        crate::ensure_runtime_schemas(&node).await.unwrap();
        let agent = agent_with_retry(CompletionRetryProfileFields::default());
        crate::test_support::install_test_agent(&node, agent.node_did(), &agent.agent_id).await;
        let mut create = gents_protocol::request_admission::AgentRequestCreate::base(
            gents_protocol::request_admission::RequestPurpose::Normal,
            "req-budget",
            agent.node_did(),
            agent.node_did(),
            &agent.agent_id,
            "session-budget",
            "Resume the request",
            "interactive",
            chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            gents_protocol::request_admission::AgentRequestAdmissionRecord::local_self(
                agent.node_did(),
            ),
        );
        crate::sign_agent_request_create(agent.node.identity.as_ref(), &mut create)
            .await
            .unwrap();
        let response = crate::store::config_client::ConfigAccess::local(node.clone())
            .write("test.budget_parent", &create.graphql_mutation().unwrap())
            .await
            .unwrap();
        let doc_id = crate::store::graphql::created_doc_id(&response, "AgentRequest").unwrap();
        let parent = crate::request::admission::load_request_for_admission_test(&node, &doc_id)
            .await
            .unwrap();
        let mut lifecycle = crate::request::lifecycle::RequestLifecycle::new_with_execution_binding(
            node.clone(),
            &agent.agent_id,
            agent.node_did(),
            parent,
            120,
            crate::request::lifecycle::ExecutionOrigin::Interactive,
            agent.backend_id.clone().unwrap(),
        );
        lifecycle.set_configured_max_total_tokens(case.pinned_limit.filter(|limit| *limit != 0));
        assert_eq!(
            lifecycle.claim_with_identity().await.unwrap(),
            crate::request::lifecycle::ClaimOutcome::Claimed
        );
        let mut request = lifecycle.request().clone();
        if case.pinned_limit == Some(0) {
            let doc_id = escape_graphql_string(&request.doc_id);
            crate::store::config_client::ConfigAccess::local(node.clone())
                .write(
                    "test.observed_zero_budget",
                    &format!("mutation {{ update_AgentRequest(docID: \"{doc_id}\", input: {{ max_total_tokens: 0 }}) {{ _docID }} }}"),
                )
                .await
                .unwrap();
            request =
                crate::request::admission::load_request_for_admission_test(&node, &request.doc_id)
                    .await
                    .unwrap();
        }
        let did = escape_graphql_string(agent.node_did());
        for (index, row) in case.rows.iter().enumerate() {
            let doc_id = escape_graphql_string(if row.request_doc_id == case.request_doc_id {
                &request.doc_id
            } else {
                &row.request_doc_id
            });
            let call_id = escape_graphql_string(&format!("rehydrate-{index}"));
            let call_kind = escape_graphql_string(&row.kind);
            let prompt = row.prompt_tokens;
            let completion = row.completion_tokens;
            let seed = format!(
                r#"mutation {{
                create_InferenceCall(input: {{
                    call_id: "{call_id}"
                    runtime_instance_id: "runtime-rehydrate"
                    request_id: "req-budget"
                    request_doc_id: "{doc_id}"
                    call_seq: {index}
                    backend_id: "backend-rehydrate"
                    agent_id: "agent-rehydrate"
                    node_did: "{did}"
                    call_kind: "{call_kind}"
                    attempt: 1
                    call_state: "completed"
                    prompt_tokens: {prompt}
                    completion_tokens: {completion}
                    usage_completeness: "complete"
                    queue_depth_at_enqueue: 0
                    controller_generation: 1
                    backend_config_fingerprint: "fp"
                }}) {{ _docID }}
            }}"#
            );
            crate::store::config_client::ConfigAccess::local(node.clone())
                .write("test.budget_call", &seed)
                .await
                .unwrap_or_else(|error| panic!("{}: {error}", case.name));
        }
        let budget = aggregate_token_budget_for_request(&node, &request)
            .await
            .unwrap_or_else(|error| panic!("{}: rehydrate: {error}", case.name));
        match (&case.ledger, budget) {
            (None, None) => {}
            (Some(expected), Some(budget)) => {
                let ledger = budget.snapshot().expect("ledger");
                assert_eq!(ledger.used, expected.used, "{}", case.name);
                assert_eq!(ledger.limit, expected.limit, "{}", case.name);
            }
            (expected, actual) => panic!(
                "{}: ledger presence expected {}, got {}",
                case.name,
                expected.is_some(),
                actual.is_some()
            ),
        }
        node.shutdown().await;
    }
}

#[test]
fn every_profile_sampling_knob_reaches_the_provider_body() {
    let sampling = SamplingConfig {
        temperature: Some(0.7),
        top_p: Some(0.95),
        top_k: Some(40),
        seed: Some(1234),
        min_p: Some(0.05),
        frequency_penalty: Some(0.5),
        presence_penalty: Some(-0.25),
        repetition_penalty: Some(1.1),
        max_tokens: Some(1024),
        reasoning_effort: None,
    };

    let value = sampling
        .additional_params()
        .expect("pinned sampling knobs must produce provider params");

    assert_eq!(value["top_p"], 0.95);
    assert_eq!(value["top_k"], 40);
    assert_eq!(value["seed"], 1234);
    assert_eq!(value["min_p"], 0.05);
    assert_eq!(value["frequency_penalty"], 0.5);
    assert_eq!(value["presence_penalty"], -0.25);
    assert_eq!(value["repetition_penalty"], 1.1);
    assert!(value.get("temperature").is_none());
    assert!(value.get("max_tokens").is_none());
}

#[test]
fn unpinned_sampling_knobs_emit_no_provider_params() {
    let sampling = SamplingConfig {
        temperature: Some(0.0),
        max_tokens: Some(256),
        ..Default::default()
    };
    assert!(
        sampling.additional_params().is_none(),
        "a profile that pins no body-param knob must not send any"
    );
}

#[tokio::test]
async fn every_loop_config_arms_the_capture_scope_it_was_built_for() {
    use crate::inference::rendered_request::scope::{
        claim_pending, scope_request, test_scope, CaptureClaim, CaptureScopeKind,
    };
    use crate::inference::rendered_request::{
        AssemblyBuildPath, AssemblyTrace, RenderedRequestCaptureSink, RenderedRequestContext,
    };

    let agent = agent_with_retry(CompletionRetryProfileFields::default());
    let context = RenderedRequestContext {
        request_doc_id: "doc-1".to_string(),
        request_commit_cid: "bafy-request-commit".to_string(),
        request_id: "req-1".to_string(),
        node_did: "did:key:agent".to_string(),
        requester_did: String::new(),
        agent_id: "general".to_string(),
        session_id: "session-1".to_string(),
        model_name: "model".to_string(),
        provider_family: None,
    };
    let sink: RenderedRequestCaptureSink = Arc::new(|_| Box::pin(async { Ok(()) }));
    let scope = test_scope(context, sink);

    scope_request(scope, async {
        for (kind, expected) in [
            (CaptureScopeKind::Inference, "inference.1"),
            (CaptureScopeKind::Compaction, "compaction.1"),
            (
                CaptureScopeKind::CompactionFallback,
                "compaction_fallback.1",
            ),
            (CaptureScopeKind::Title, "title.1"),
            (CaptureScopeKind::OneShot, "oneshot.1"),
        ] {
            let config = loop_config(&agent, "preamble".to_string(), 0, kind);
            let on_rendered_request = config
                .on_rendered_request
                .clone()
                .expect("every production loop config installs an arming sink");
            on_rendered_request(
                0,
                0,
                rig::completion::CompletionRequest {
                    model: None,
                    preamble: None,
                    chat_history: rig::one_or_many::OneOrMany::one(rig::completion::Message::user(
                        "hi",
                    )),
                    documents: Vec::new(),
                    tools: Vec::new(),
                    temperature: None,
                    max_tokens: None,
                    tool_choice: None,
                    additional_params: None,
                    output_schema: None,
                },
                AssemblyTrace::from_effective_messages(AssemblyBuildPath::Budgeted, Vec::new()),
            )
            .await
            .expect("arming never fails");

            let (_, claim) = claim_pending().expect("a scope is installed");
            let CaptureClaim::Armed(pending) = claim else {
                panic!("{kind} did not arm a pending capture");
            };
            assert_eq!(
                pending.capture_scope, expected,
                "{kind} armed the wrong scope"
            );
        }
    })
    .await;
}

#[test]
fn generated_service_tier_cases_drive_loop_config() {
    let cases = crate::lean_vocab_test::lean_prompt_assembly_service_tier_cases();
    assert!(!cases.is_empty());
    for case in cases {
        let family = BackendProviderKind::parse_optional(Some(&case.family)).expect("Lean family");
        let tier: Option<crate::inference::resolved::ServiceTier> =
            serde_json::from_value(serde_json::to_value(&case.requested).unwrap())
                .expect("Lean tier");
        assert_eq!(
            crate::inference::resolved::validate_service_tier(family, tier).is_ok(),
            case.allowed,
            "{case:?}"
        );
        if !case.allowed {
            continue;
        }
        for wire in [
            crate::OpenAiWireApi::Responses,
            crate::OpenAiWireApi::ChatCompletions,
        ] {
            if family == BackendProviderKind::ChatGptCodex
                && wire != crate::OpenAiWireApi::Responses
            {
                continue;
            }
            let mut agent = agent_with_retry(Default::default());
            agent.backend_provider_kind = family;
            agent.openai_wire_api = wire;
            agent.service_tier = tier;
            let config = loop_config(&agent, "preamble".into(), 0, CaptureScopeKind::Inference);
            let request = rig::completion::CompletionRequest {
                model: None,
                preamble: config.preamble.clone(),
                chat_history: rig::one_or_many::OneOrMany::one(rig::completion::Message::user(
                    "hi",
                )),
                documents: Vec::new(),
                tools: Vec::new(),
                temperature: None,
                max_tokens: None,
                tool_choice: None,
                additional_params: config.additional_params.clone(),
                output_schema: None,
            };
            let body = config
                .provider_input_counter
                .project_body(&request)
                .expect("provider body");
            assert_eq!(
                body.get("service_tier").and_then(serde_json::Value::as_str),
                case.wire.as_deref(),
                "{case:?}, {wire:?}"
            );
        }
    }
}

fn calibration_accounting(estimator: &str, estimated: usize, ratio: Option<u64>) -> String {
    serde_json::json!({
        "accounting_version": 3,
        "estimator": estimator,
        "components": {"messages": estimated, "documents": 0, "tool_schemas": 0,
            "additional_parameters": 0, "output_schema": 0},
        "estimated_input_tokens": estimated,
        "raw_estimated_input_tokens": ratio.map(|_| estimated),
        "input_calibration_basis_points": ratio,
        "context_window": 1_000_000,
        "compaction_threshold_basis_points": 7_500,
        "compaction_threshold_tokens": 750_000,
        "compaction_reason": "below_threshold",
    })
    .to_string()
}

/// The ratio the seed sets before any default prior: the window's maximum,
/// else the newest dispatched ratio.
fn session_input_ratio_only(rows: &[CalibrationCallRow], estimator: &str) -> Option<u64> {
    let seed = session_calibration_seed(rows, estimator);
    seed.observations
        .iter()
        .copied()
        .max()
        .or(seed.dispatched_ratio)
}

fn calibration_row(
    usage: Option<(i64, i64, i64)>,
    accounting: Option<String>,
) -> CalibrationCallRow {
    CalibrationCallRow {
        prompt_tokens: usage.map(|(input, _, _)| input),
        completion_tokens: usage.map(|_| 7_000),
        cached_input_tokens: usage.map(|(_, read, _)| read),
        cache_creation_input_tokens: usage.map(|(_, _, write)| write),
        usage_completeness: Some(
            if usage.is_some() {
                "complete"
            } else {
                "missing"
            }
            .into(),
        ),
        context_accounting_json: accounting,
    }
}

#[test]
fn session_input_ratio_reads_the_newest_calibrated_call() {
    let claude = "claude_messages_wire_json_bytes_div_4_v1";
    let accounting = |estimated, ratio| Some(calibration_accounting(claude, estimated, ratio));
    assert_eq!(session_input_ratio_only(&[], claude), None);
    assert_eq!(
        session_input_ratio_only(
            &[calibration_row(
                Some((109_881, 109_000, 878)),
                accounting(65_755, None)
            )],
            claude
        ),
        Some(16_711),
        "a version-2 call's estimate is its raw count; normalized input is the whole count"
    );
    for (row, why) in [
        (
            calibration_row(Some((3, 109_000, 878)), accounting(65_755, None)),
            "pre-normalization uncached input",
        ),
        (
            calibration_row(Some((50_000, 0, 0)), accounting(65_755, None)),
            "input under the estimate",
        ),
        (
            CalibrationCallRow {
                usage_completeness: Some("partial".into()),
                ..calibration_row(Some((109_881, 109_000, 878)), accounting(65_755, None))
            },
            "incomplete usage",
        ),
    ] {
        assert_eq!(
            session_input_ratio_only(&[row], claude),
            None,
            "{why} leaves the prior in place"
        );
    }
    assert_eq!(
        session_input_ratio_only(
            &[
                calibration_row(None, None),
                calibration_row(Some((3, 109_000, 878)), accounting(65_755, None)),
                calibration_row(
                    None,
                    Some(calibration_accounting(
                        "openai_chat_wire_json_bytes_div_4_v1",
                        10,
                        None
                    ))
                ),
                calibration_row(None, accounting(10_000, Some(17_000))),
                calibration_row(Some((20_000, 0, 0)), accounting(10_000, None)),
            ],
            claude
        ),
        Some(20_000),
        "only usable usage of this estimator is an observation; a dispatched ratio is not"
    );
    assert_eq!(
        session_input_ratio_only(
            &[calibration_row(None, accounting(10_000, Some(17_000)))],
            claude
        ),
        Some(17_000),
        "without observations the newest dispatched ratio is the prior"
    );
}

#[test]
fn session_seed_keeps_the_maximum_of_its_newest_observations() {
    let claude = "claude_messages_wire_json_bytes_div_4_v1";
    let observed = |input: i64| {
        calibration_row(
            Some((input, 0, 0)),
            Some(calibration_accounting(claude, 10_000, None)),
        )
    };
    let seed = session_calibration_seed(&[observed(13_000), observed(26_000)], claude);
    assert_eq!(seed.observations, vec![26_000, 13_000], "oldest first");
    assert_eq!(
        session_input_ratio_only(&[observed(13_000), observed(26_000)], claude),
        Some(26_000),
        "a 1.3x call after a 2.6x one still seeds 2.6x"
    );
    let window = crate::turn::provider_input::budget::CALIBRATION_OBSERVATION_WINDOW;
    let mut rows = (0..window).map(|_| observed(13_000)).collect::<Vec<_>>();
    rows.push(observed(26_000));
    assert_eq!(
        session_input_ratio_only(&rows, claude),
        Some(13_000),
        "a 2.6x call older than the newest {window} observations has aged out"
    );
}

#[test]
fn only_a_usable_newest_call_anchors_the_session() {
    let claude = "claude_messages_wire_json_bytes_div_4_v1";
    let accounting = |estimated, ratio| Some(calibration_accounting(claude, estimated, ratio));
    assert_eq!(
        session_calibration_seed(
            &[calibration_row(
                Some((109_881, 109_000, 878)),
                accounting(65_755, None)
            )],
            claude
        ),
        CalibrationSeed {
            observations: vec![16_711],
            dispatched_ratio: None,
            anchor: Some(crate::turn::provider_input::budget::ContextAnchor {
                input_tokens: 109_881,
                output_tokens: 7_000,
                estimated_input_tokens: 65_755,
            }),
        }
    );
    assert_eq!(
        session_calibration_seed(
            &[
                calibration_row(None, accounting(80_000, Some(17_000))),
                calibration_row(Some((109_881, 109_000, 878)), accounting(65_755, None)),
            ],
            claude
        ),
        CalibrationSeed {
            observations: vec![16_711],
            dispatched_ratio: Some(17_000),
            anchor: None,
        },
        "a newest call without usable usage anchors nothing; older usage still observes"
    );
}

#[tokio::test]
async fn session_calibration_seeds_from_durable_inference_calls() {
    let node = Arc::new(crate::test_support::node_builder().build().await.unwrap());
    crate::ensure_runtime_schemas(&node).await.unwrap();
    let agent = agent_with_retry(CompletionRetryProfileFields::default());
    let did = agent.node_did().to_string();
    let session = "session-calibration";
    assert_eq!(
        context_calibration_for_session(&node, &agent, &did, session)
            .await
            .ratio_basis_points(),
        crate::turn::provider_input::budget::UNIT_INPUT_RATIO_BASIS_POINTS,
        "an unobserved OpenAI-compatible session starts unscaled"
    );
    let estimator = crate::turn::provider_input::ProviderInputProfile::resolve(
        agent.backend_provider_kind,
        agent.openai_wire_api,
    )
    .estimator_name();
    let own = agent.agent_id.as_str();
    for (index, (agent_id, session_id, queued_at, prompt, cached, ratio)) in [
        (own, session, "2026-10-28T10:00:00Z", 15_000, 0, None),
        (own, session, "2026-10-28T11:00:00Z", 12_500, 0, None),
        (
            own,
            "other-session",
            "2026-10-28T12:00:00Z",
            30_000,
            0,
            None,
        ),
        (
            own,
            session,
            "2026-10-28T09:00:00Z",
            11_000,
            0,
            Some(19_000),
        ),
        (own, session, "2026-10-28T13:00:00Z", 2, 100_000, None),
        (
            "other-agent",
            session,
            "2026-10-28T14:00:00Z",
            30_000,
            0,
            None,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let accounting = escape_graphql_string(&calibration_accounting(estimator, 10_000, ratio));
        let seed = format!(
            r#"mutation {{
            create_InferenceCall(input: {{
                call_id: "calibration-{index}"
                runtime_instance_id: "runtime-calibration"
                request_id: "req-calibration"
                request_doc_id: "doc-calibration-{index}"
                session_id: "{session_id}"
                model_name: "{model}"
                call_seq: {index}
                backend_id: "backend-calibration"
                agent_id: "{agent_id}"
                node_did: "{did}"
                call_kind: "inference"
                attempt: 1
                call_state: "completed"
                queued_at: "{queued_at}"
                prompt_tokens: {prompt}
                completion_tokens: 10
                cached_input_tokens: {cached}
                cache_creation_input_tokens: 0
                usage_completeness: "complete"
                queue_depth_at_enqueue: 0
                controller_generation: 1
                backend_config_fingerprint: "fp"
                context_accounting_json: "{accounting}"
            }}) {{ _docID }}
        }}"#,
            model = escape_graphql_string(&agent.model_name),
            did = escape_graphql_string(&did),
        );
        crate::store::config_client::ConfigAccess::local(node.clone())
            .write("test.calibration_call", &seed)
            .await
            .unwrap();
    }
    assert_eq!(
        context_calibration_for_session(&node, &agent, &did, session)
            .await
            .ratio_basis_points(),
        15_000,
        "the maximum of this agent's usable calls in the session, not another session's, \
         another agent's or an older build's"
    );
    assert_eq!(
        context_calibration_for_session(&node, &agent, &did, session)
            .await
            .anchor(),
        Some(crate::turn::provider_input::budget::ContextAnchor {
            input_tokens: 12_500,
            output_tokens: 10,
            estimated_input_tokens: 10_000,
        }),
        "its provider count anchors the next request; another agent's newer call does not"
    );
    let first = context_calibration_for_session(&node, &agent, &did, session).await;
    let second = context_calibration_for_session(&node, &agent, &did, session).await;
    first.observe(40_000, 10_000);
    first.invalidate_anchor();
    assert_eq!(
        (second.ratio_basis_points(), second.anchor().is_some()),
        (15_000, true),
        "each request seeds its own calibration; one request's observations never reach another's"
    );
    node.shutdown().await;
}
