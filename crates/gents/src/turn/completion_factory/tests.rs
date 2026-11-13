use super::*;
use std::sync::Arc;
use std::time::Duration;

use gents_inference::resolved::SamplingConfig;
use gents_loop::completion_retry::CompletionRetryProfileFields;
use gents_protocol::row::AgentRequest;
use gents_store::identity::{KeyIdentity, NodeIdentity, RuntimeNode};

fn request() -> AgentRequest {
    AgentRequest {
        retry_parent_request_doc_id: None,
        purpose: gents_protocol::request_admission::RequestPurpose::Normal,
        doc_id: String::new(),
        request_id: "request-123".to_string(),
        node_did: String::new(),
        requester_did: None,
        agent_did: "agent-test".to_owned(),
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
            gents_loop::openai_wire::OpenAiWireApi::Responses,
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
fn profile_reasoning_serializes_top_level_into_openai_body() {
    use rig::completion::message::{Message, Text, UserContent};
    use rig::one_or_many::OneOrMany;

    let additional_params = merge_optional_params(
        merge_optional_params(
            reasoning_profile_params(
                BackendProviderKind::OpenAiCompatible,
                gents_loop::openai_wire::OpenAiWireApi::ChatCompletions,
                Some(gents_documents::inference_defaults::ReasoningEffort::Max),
            ),
            provider_additional_params(
                BackendProviderKind::OpenAiCompatible,
                gents_loop::openai_wire::OpenAiWireApi::ChatCompletions,
                gents_inference::backends::setup::OPENAI_ENDPOINT,
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
        gents_loop::openai_wire::OpenAiWireApi::Responses,
        gents_inference::backends::setup::OPENAI_ENDPOINT,
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
    gents_lean_vocab::families::prompt_assembly_responses_storage_family_vocabulary::replay(&[
        BackendProviderKind::OpenAiCompatible,
        BackendProviderKind::XaiGrokOAuth,
        BackendProviderKind::ChatGptCodex,
        BackendProviderKind::OpenRouter,
    ]);
    gents_lean_vocab::families::contracts_prompt_assembly_responses_storage::replay::<
        BackendProviderKind,
        gents_loop::openai_wire::OpenAiWireApi,
    >(|case| {
        let mut agent = agent_with_retry(Default::default());
        agent.backend_provider_kind = case.family;
        agent.openai_wire_api = case.wire;
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
        gents_lean_vocab::families::contracts_prompt_assembly_responses_storage::ResponsesStorageOutput {
            store,
            encrypted_include,
        }
    });
}

#[test]
fn generated_responses_effort_cases_drive_loop_config() {
    use gents_documents::inference_defaults::ReasoningEffort;

    let parse = |value: &String| ReasoningEffort::parse(value).expect("Lean effort");
    gents_lean_vocab::families::contracts_prompt_assembly_responses_effort::replay(|case| {
        let mut agent = agent_with_retry(Default::default());
        agent.backend_provider_kind = BackendProviderKind::OpenAiCompatible;
        agent.openai_wire_api = gents_loop::openai_wire::OpenAiWireApi::Responses;
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
        body.pointer("/reasoning/effort")
            .and_then(serde_json::Value::as_str)
            .map(str::to_owned)
    });
}

#[test]
fn loop_config_for_request_rejects_unknown_retry_origin() {
    let agent = agent_with_retry(CompletionRetryProfileFields::default());
    let mut request = request();
    request.execution_origin = Some("legacy-or-missing".to_string());

    let Err(error) = loop_config_for_request(&agent, "preamble".to_string(), &request, None, 0, 0)
    else {
        panic!("unknown execution origin must fail closed");
    };
    assert!(error.to_string().contains("unknown execution_origin"));
}

fn agent_with_retry(completion_retry: CompletionRetryProfileFields) -> ResolvedAgent {
    let identity = Arc::new(
        KeyIdentity::load_or_create(
            std::env::temp_dir().join(format!("completion-factory-{}.key", uuid::Uuid::new_v4())),
            None,
        )
        .unwrap(),
    );
    let binding = gents_store::identity::provision_agent_identity(identity.as_ref()).unwrap();
    agent_with_binding(identity, &binding, completion_retry)
}

fn agent_with_binding(
    identity: Arc<dyn NodeIdentity>,
    binding: &gents_protocol::agent_identity::AgentIdentityBinding,
    completion_retry: CompletionRetryProfileFields,
) -> ResolvedAgent {
    let node = Arc::new(RuntimeNode {
        node_did: identity.did().to_string(),
        identity,
        default_agent_did: binding.agent_did.clone(),
        display_name: None,
        enabled: true,
    });

    let agent_identity =
        gents_store::identity::resolve_agent_identity(node.identity.as_ref(), binding).unwrap();
    let agent_did = binding.agent_did.clone();
    ResolvedAgent {
        configuration: gents_inference::resolved::ResolvedConfiguration::Static,
        profile_id: None,
        agent_did,
        agent_identity,
        node,
        backend_id: Some("backend-general".to_string()),
        backend_provider_kind: BackendProviderKind::OpenAiCompatible,
        openai_wire_api: gents_loop::openai_wire::OpenAiWireApi::ChatCompletions,
        backend_endpoint: "http://127.0.0.1:8999/v1".to_string(),
        claude_platform: None,
        backend_auth: gents_documents::BackendAuth::Unauthenticated,
        model_name: gents_inference::resolved::DEFAULT_MODEL_NAME.to_string(),
        service_tier: None,
        resolved_reasoning_efforts: None,
        context_window: gents_inference::resolved::DEFAULT_CONTEXT_WINDOW,
        max_output_tokens: gents_inference::resolved::DEFAULT_MAX_OUTPUT_TOKENS,
        max_turns: gents_inference::resolved::DEFAULT_MAX_TURNS,
        max_turns_provenance: gents_inference::resolved::MaxTurnsProvenance::Default,
        system_prompt: "system".to_string(),
        compaction: None,
        compaction_inference: None,
        max_total_tokens: None,
        stream_batch_ms: gents_inference::resolved::DEFAULT_STREAM_BATCH_MS,
        stream_liveness_timeout: Duration::from_secs(
            gents_documents::inference_defaults::DEFAULT_STREAM_LIVENESS_TIMEOUT_SECS,
        ),
        deadline_duration: Duration::from_secs(
            gents_documents::inference_defaults::DEFAULT_DEADLINE_DURATION_SECS,
        ),
        provider_idle_timeout: Duration::from_secs(
            gents_inference::resolved::DEFAULT_PROVIDER_IDLE_TIMEOUT_SECS,
        ),
        completion_retry,
        sampling: SamplingConfig::default(),
        skills: Vec::new(),
        oauth_pool: Vec::new(),
    }
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
        gents_loop::provider_usage::sum_charged_from_persisted_parts(
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
            gents_loop::provider_usage::sum_charged_from_persisted_parts(
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
    for case in gents_lean_vocab::families::budget_rehydration_ledger::cases::<String>() {
        let (db, user, owner, binding) =
            crate::test_support::root::signed_test_node("completion-budget-rehydration").await;
        let node = db.node.clone();
        let agent = agent_with_binding(
            db.node_identity.clone(),
            &binding,
            CompletionRetryProfileFields::default(),
        );
        let access = gents_store::config_client::ConfigAccess::local_managed_agent(
            node.clone(),
            identity::Did::new(&agent.agent_did).unwrap(),
            agent.agent_identity.as_ref(),
        )
        .unwrap();
        let create = gents_session::request::lifecycle::build_signed_client_request(
            &owner,
            gents_session::request::lifecycle::RequestSpec::new(
                gents_protocol::request_admission::RequestPurpose::Normal,
                gents_session::request::lifecycle::RequestIdentity {
                    request_id: "req-budget".into(),
                    node_did: agent.node_did().into(),
                    requester_did: Some(user.did().into()),
                    agent_did: agent.agent_did.clone(),
                    session_id: "session-budget".into(),
                    content: "Resume the request".into(),
                    execution_origin:
                        gents_session::request::lifecycle::ExecutionOrigin::Interactive,
                    created_at: chrono::Utc::now()
                        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                },
                gents_protocol::request_admission::AgentRequestAdmissionRecord::client(user.did()),
            ),
            gents_session::request::lifecycle::RequestSigner::Identity(&user),
        )
        .await
        .unwrap();
        let session_doc_id = create.session_doc_id.clone().unwrap();
        gents_session::request::lifecycle::submit_client_request(&owner, &create)
            .await
            .unwrap();
        let response = access
            .execute(&format!(
                "{{ AgentRequest {{ {} }} }}",
                gents_protocol::request_admission::SIGNED_REQUEST_FIELDS
            ))
            .await
            .unwrap();
        let row: gents_protocol::row::AgentRequestRow =
            serde_json::from_value(response["data"]["AgentRequest"][0].clone()).unwrap();
        let parent = row.try_into().unwrap();
        let (_authority_owner, authority) =
            gents_p2p::p2p_reconcile::enrollment_authority_channel();
        let verifier = gents_session::request::admission::AgentRequestAdmissionVerifier::new(
            node.clone(),
            db.node_identity.clone(),
            authority,
        );
        let (parent, admission) = verifier
            .verify_fresh_for_claim(&parent, &agent.agent_did)
            .await
            .unwrap();
        let mut lifecycle =
            gents_session::request::lifecycle::RequestLifecycle::new_with_execution_binding(
                node.clone(),
                parent,
                120,
                gents_session::request::lifecycle::ExecutionOrigin::Interactive,
                agent.backend_id.clone().unwrap(),
            );
        lifecycle
            .bind_managed_agent(agent.agent_identity.as_ref())
            .unwrap();
        lifecycle.set_client_claim_admission(admission);
        lifecycle
            .set_configured_max_total_tokens(case.input.pinned_limit.filter(|limit| *limit != 0));
        assert_eq!(
            lifecycle.claim_with_identity().await.unwrap(),
            gents_session::request::lifecycle::ClaimOutcome::Claimed
        );
        let mut request = lifecycle.request().clone();
        if case.input.pinned_limit == Some(0) {
            let doc_id = escape_graphql_string(&request.doc_id);
            access
                .write(
                    "test.observed_zero_budget",
                    &format!("mutation {{ update_AgentRequest(docID: \"{doc_id}\", input: {{ max_total_tokens: 0 }}) {{ _docID }} }}"),
                )
                .await
                .unwrap();
            let response = access
                .execute(&format!(
                    "{{ AgentRequest(filter: {{ _docID: {{ _eq: \"{doc_id}\" }} }}) {{ {} }} }}",
                    gents_protocol::request_admission::SIGNED_REQUEST_FIELDS
                ))
                .await
                .unwrap();
            let row: gents_protocol::row::AgentRequestRow =
                serde_json::from_value(response["data"]["AgentRequest"][0].clone()).unwrap();
            request = row.try_into().unwrap();
        }
        let did = escape_graphql_string(agent.node_did());
        for (index, row) in case.input.rows.iter().enumerate() {
            let doc_id =
                escape_graphql_string(if row.request_doc_id == case.input.request_doc_id {
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
                    agent_did: "{agent_did}"
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
            }}"#,
                agent_did = escape_graphql_string(&agent.agent_did),
            );
            access
                .transact_write("test.budget_call", |txn| {
                    let seed = &seed;
                    let session_doc_id = &session_doc_id;
                    Box::pin(async move {
                        let response = txn.execute(seed).await?;
                        let doc_id =
                            gents_store::graphql::created_doc_id(&response, "InferenceCall")?;
                        txn.add_resource_relationship(
                            "InferenceCall",
                            &doc_id,
                            "parent",
                            "sessions",
                            session_doc_id,
                        )
                        .await?;
                        Ok::<_, anyhow::Error>(())
                    })
                })
                .await
                .unwrap_or_else(|error| panic!("{}: {error}", case.input.name));
        }
        let budget = aggregate_token_budget_for_request(&access, &request)
            .await
            .unwrap_or_else(|error| panic!("{}: rehydrate: {error}", case.input.name));
        let observed = budget.map(|budget| {
            let ledger = budget.snapshot().expect("ledger");
            gents_lean_vocab::families::budget_rehydration_ledger::Ledger {
                limit: ledger.limit,
                used: ledger.used,
            }
        });
        node.shutdown().await.expect("node shutdown");
        case.check(observed);
    }
}

#[tokio::test]
async fn every_loop_config_arms_the_capture_scope_it_was_built_for() {
    use gents_inference::rendered_request::scope::{
        claim_pending, scope_request, test_scope, CaptureClaim, CaptureScopeKind,
    };
    use gents_inference::rendered_request::{
        AssemblyBuildPath, AssemblyTrace, RenderedRequestCaptureSink, RenderedRequestContext,
    };

    let agent = agent_with_retry(CompletionRetryProfileFields::default());
    let context = RenderedRequestContext {
        request_doc_id: "doc-1".to_string(),
        request_commit_cid: "bafy-request-commit".to_string(),
        request_id: "req-1".to_string(),
        node_did: "did:key:agent".to_string(),
        requester_did: String::new(),
        agent_did: "general".to_string(),
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
fn service_tier_loop_config_follows_the_lean_model() {
    gents_lean_vocab::families::contracts_service_tier_loop_config::replay(|input| {
        let family = BackendProviderKind::parse_optional(Some(&input.family)).expect("Lean family");
        let tier: Option<gents_inference::resolved::ServiceTier> =
            serde_json::from_value(serde_json::to_value(&input.requested).unwrap())
                .expect("Lean tier");
        let wire = match input.openai_wire_api.as_str() {
            "responses" => gents_loop::openai_wire::OpenAiWireApi::Responses,
            "chat_completions" => gents_loop::openai_wire::OpenAiWireApi::ChatCompletions,
            other => panic!("unknown modeled OpenAI wire {other}"),
        };
        let mut agent = agent_with_retry(Default::default());
        agent.backend_provider_kind = family;
        agent.openai_wire_api = wire;
        agent.service_tier = tier;
        let config = loop_config(&agent, "preamble".into(), 0, CaptureScopeKind::Inference);
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
            .expect("provider body");
        gents_lean_vocab::families::contracts_service_tier_loop_config::ServiceTierWire {
            wire: body
                .get("service_tier")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned),
        }
    });
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

#[tokio::test]
async fn session_calibration_seeds_from_durable_inference_calls() {
    let (db, user, owner, binding) =
        crate::test_support::root::signed_test_node("completion-session-calibration").await;
    let node = db.node.clone();
    let agent = agent_with_binding(
        db.node_identity.clone(),
        &binding,
        CompletionRetryProfileFields::default(),
    );
    let access = gents_store::config_client::ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(&agent.agent_did).unwrap(),
        agent.agent_identity.as_ref(),
    )
    .unwrap();
    let other_binding = gents_store::identity::provision_agent_identity_authorized(
        &node,
        db.node_identity.as_ref(),
        &identity::Did::new(user.did()).unwrap(),
    )
    .await
    .unwrap();
    gents_inference::test_support::install_test_agent_binding_with_access(
        db.node_identity.did(),
        "Other calibration agent",
        &other_binding,
        &owner,
    )
    .await;
    let other =
        gents_store::identity::resolve_agent_identity(db.node_identity.as_ref(), &other_binding)
            .unwrap();
    let other_access = gents_store::config_client::ConfigAccess::local_managed_agent(
        node.clone(),
        identity::Did::new(other.did()).unwrap(),
        other.as_ref(),
    )
    .unwrap();
    let did = agent.node_did().to_string();
    let session = "session-calibration";
    let mut sessions = Vec::new();
    for session_id in [session, "other-session"] {
        let create = gents_session::request::lifecycle::build_signed_client_request(
            &owner,
            gents_session::request::lifecycle::RequestSpec::new(
                gents_protocol::request_admission::RequestPurpose::Normal,
                gents_session::request::lifecycle::RequestIdentity {
                    request_id: format!("seed-{session_id}"),
                    node_did: did.clone(),
                    requester_did: Some(user.did().into()),
                    agent_did: agent.agent_did.clone(),
                    session_id: session_id.into(),
                    content: "calibration".into(),
                    execution_origin:
                        gents_session::request::lifecycle::ExecutionOrigin::Interactive,
                    created_at: chrono::Utc::now()
                        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                },
                gents_protocol::request_admission::AgentRequestAdmissionRecord::client(user.did()),
            ),
            gents_session::request::lifecycle::RequestSigner::Identity(&user),
        )
        .await
        .unwrap();
        sessions.push(create.session_doc_id.unwrap());
    }
    owner
        .add_actor_relationship(
            "AgentSession",
            &sessions[0],
            "reader",
            &identity::Did::new(other.did()).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        context_calibration_for_session(&access, &agent, &did, session)
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
    let own = agent.agent_did.as_str();
    for (index, (agent_did, session_id, queued_at, prompt, cached, ratio)) in [
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
            other.did(),
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
        let escaped_agent_did = escape_graphql_string(agent_did);
        let escaped_session_id = escape_graphql_string(session_id);
        let seed = format!(
            r#"mutation {{
            create_InferenceCall(input: {{
                call_id: "calibration-{index}"
                runtime_instance_id: "runtime-calibration"
                request_id: "req-calibration"
                request_doc_id: "doc-calibration-{index}"
                session_id: "{escaped_session_id}"
                model_name: "{model}"
                call_seq: {index}
                backend_id: "backend-calibration"
                agent_did: "{escaped_agent_did}"
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
        let writer = if agent_did == other.did() {
            &other_access
        } else {
            &access
        };
        let session_doc_id = if session_id == session {
            &sessions[0]
        } else {
            &sessions[1]
        };
        writer
            .transact_write("test.calibration_call", |txn| {
                let seed = &seed;
                Box::pin(async move {
                    let response = txn.execute(seed).await?;
                    let doc_id = gents_store::graphql::created_doc_id(&response, "InferenceCall")?;
                    txn.add_resource_relationship(
                        "InferenceCall",
                        &doc_id,
                        "parent",
                        "sessions",
                        session_doc_id,
                    )
                    .await?;
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await
            .unwrap();
    }
    assert_eq!(
        context_calibration_for_session(&access, &agent, &did, session)
            .await
            .ratio_basis_points(),
        15_000,
        "the maximum of this agent's usable calls in the session, not another session's, \
         another agent's or an older build's"
    );
    assert_eq!(
        context_calibration_for_session(&access, &agent, &did, session)
            .await
            .anchor(),
        Some(crate::turn::provider_input::budget::ContextAnchor {
            input_tokens: 12_500,
            output_tokens: 10,
            estimated_input_tokens: 10_000,
        }),
        "its provider count anchors the next request; another agent's newer call does not"
    );
    let first = context_calibration_for_session(&access, &agent, &did, session).await;
    let second = context_calibration_for_session(&access, &agent, &did, session).await;
    first.observe(40_000, 10_000);
    first.invalidate_anchor();
    assert_eq!(
        (second.ratio_basis_points(), second.anchor().is_some()),
        (15_000, true),
        "each request seeds its own calibration; one request's observations never reach another's"
    );
    node.shutdown().await.expect("node shutdown");
}

#[test]
fn claude_subscription_requests_carry_the_session_key_and_hops_reach_the_policy() {
    let mut agent = agent_with_retry(CompletionRetryProfileFields::default());
    agent.backend_provider_kind = BackendProviderKind::ClaudeCliSubscription;
    let config = loop_config_for_request(&agent, "system".into(), &request(), None, 0, 3).unwrap();
    let params = config.additional_params.unwrap();
    assert_eq!(
        params[gents_inference::clients::claude_messages::SESSION_KEY_PARAM],
        "session-456"
    );
    assert_eq!(config.retry_policy.account_hops, 3);
}

#[test]
fn generated_reasoning_profile_params_preserve_the_provider_wire_contract() {
    gents_lean_vocab::families::runtime_completion_parameters_reasoning_profile_params::replay(
        |input| {
            let kind = BackendProviderKind::parse_optional(Some(&input.provider)).unwrap();
            let wire = if input.responses {
                OpenAiWireApi::Responses
            } else {
                OpenAiWireApi::ChatCompletions
            };
            let effort = input
                .effort
                .as_deref()
                .map(|value| serde_json::from_value(serde_json::json!(value)).unwrap());
            reasoning_profile_params(kind, wire, effort)
                .map(|value| gents_lean_vocab::compact_model_json(&value))
        },
    );
}

#[test]
fn generated_request_additional_params_keep_session_cache_affinity() {
    gents_lean_vocab::families::runtime_completion_parameters_request_additional_params::replay(
        |input| {
            let mut agent = agent_with_retry(Default::default());
            agent.backend_provider_kind =
                BackendProviderKind::parse_optional(Some(&input.provider)).unwrap();
            let mut request = request();
            request.session_id = input.session.clone();
            request.request_id = input.request.clone();
            request_additional_params(&agent, &request)
                .map(|value| gents_lean_vocab::compact_model_json(&value))
        },
    );
}

#[test]
fn generated_openai_cache_scope_params_use_the_session_before_the_request() {
    gents_lean_vocab::families::runtime_completion_parameters_openai_cache_scope_params::replay(
        |input| {
            let mut request = request();
            request.session_id = input.session.clone();
            request.request_id = input.request.clone();
            openai_cache_scope_params(&request)
                .map(|value| gents_lean_vocab::compact_model_json(&value))
        },
    );
}

#[test]
fn generated_session_calibration_seeds_compare_the_complete_observation() {
    use gents_lean_vocab::families::runtime_session_calibration_session_calibration_seed as family;
    family::replay(|input| {
        let estimator = "claude_messages_wire_json_bytes_div_4_v1";
        let rows = input
            .rows
            .iter()
            .map(|row| {
                let accounting = row.accounting.then(|| {
                    let mut value: serde_json::Value =
                        serde_json::from_str(&calibration_accounting(
                            if row.matching {
                                estimator
                            } else {
                                "another_estimator"
                            },
                            usize::try_from(row.estimated).unwrap(),
                            row.dispatched,
                        ))
                        .unwrap();
                    value["raw_estimated_input_tokens"] = serde_json::json!(row.raw);
                    value.to_string()
                });
                CalibrationCallRow {
                    prompt_tokens: row.input,
                    completion_tokens: row.output,
                    cached_input_tokens: row.cached,
                    cache_creation_input_tokens: row.created,
                    usage_completeness: Some(
                        if row.complete { "complete" } else { "partial" }.into(),
                    ),
                    context_accounting_json: accounting,
                }
            })
            .collect::<Vec<_>>();
        let seed = session_calibration_seed(&rows, estimator);
        family::Observation {
            observations: seed.observations,
            dispatched_ratio: seed.dispatched_ratio,
            anchor: seed.anchor.map(|anchor| family::Anchor {
                input_tokens: anchor.input_tokens,
                output_tokens: anchor.output_tokens,
                estimated_input_tokens: anchor.estimated_input_tokens as u64,
            }),
        }
    });
}

#[test]
fn generated_request_loop_configuration_preserves_retry_policy_and_deadline() {
    use chrono::TimeZone;
    use gents_lean_vocab::families::runtime_completion_config_loop_config_for_request as family;
    family::replay(|input| {
        let mut agent = agent_with_retry(CompletionRetryProfileFields {
            retry_max_transport: input.transport,
            retry_interactive_max: input.interactive,
            retry_max_resample: input.resample,
            retry_allow_repair: input.repair,
            retry_backoff_ms: input.delays.clone(),
        });
        agent.backend_provider_kind =
            BackendProviderKind::parse_optional(Some(&input.provider)).unwrap();
        agent.openai_wire_api = if input.responses {
            OpenAiWireApi::Responses
        } else {
            OpenAiWireApi::ChatCompletions
        };
        agent.sampling = SamplingConfig {
            temperature: input.temperature.map(|value| value as f64),
            top_k: input.top_k.map(|value| i64::try_from(value).unwrap()),
            max_tokens: input.max_tokens,
            ..Default::default()
        };
        let mut request = request();
        request.execution_origin = Some(input.origin.clone());
        request.session_id = input.session.clone();
        request.request_id = input.request.clone();
        request.deadline = input.deadline.map(|timestamp| {
            chrono::Utc
                .timestamp_opt(timestamp, 0)
                .unwrap()
                .to_rfc3339()
        });
        loop_config_for_request(
            &agent,
            "preamble".into(),
            &request,
            input.budget.map(AggregateTokenBudget::new),
            0,
            input.hops as u32,
        )
        .ok()
        .map(|config| family::Observation {
            delays: config
                .retry_policy
                .transport_backoff
                .iter()
                .map(|delay| delay.as_millis() as u64)
                .collect(),
            resample: u64::from(config.retry_policy.max_resample),
            empty: u64::from(config.retry_policy.max_empty_resample),
            repair: config.retry_policy.allow_repair,
            hops: u64::from(config.retry_policy.account_hops),
            deadline: config.deadline.map(|deadline| deadline.timestamp()),
            budget: config
                .aggregate_token_budget
                .map(|budget| budget.snapshot().unwrap().limit),
            params: config
                .additional_params
                .map(|value| gents_lean_vocab::compact_model_json(&value)),
        })
    });
}

#[test]
fn generated_reasoning_capabilities_override_untrusted_provider_parameters() {
    gents_lean_vocab::families::runtime_reasoning_capabilities_authoritative_reasoning_capabilities::replay(|input| {
        let kind = if input.messages { BackendProviderKind::ClaudeCliSubscription } else { BackendProviderKind::OpenAiCompatible };
        let params = input.params.as_ref().map(|params| serde_json::from_str(params).unwrap());
        let advertised = input.advertised.as_ref().map(|efforts| efforts.iter().map(|effort| {
            serde_json::from_value::<ReasoningEffort>(serde_json::json!(effort)).unwrap()
        }).collect::<Vec<_>>());
        authoritative_reasoning_capabilities(params, kind, advertised.as_deref()).map(|params| gents_lean_vocab::compact_model_json(&params))
    });
}
