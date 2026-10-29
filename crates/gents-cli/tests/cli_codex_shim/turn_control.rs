use super::*;
use crate::support::mocks::fake_llm::{tool_call_sse_with_id, ChatAction, FakeLlm};
use std::sync::Arc;

fn has_exact_user_text(request: &Value, text: &str) -> bool {
    request["messages"].as_array().is_some_and(|messages| {
        messages.iter().any(|message| {
            message["role"] == "user"
                && (message["content"].as_str() == Some(text)
                    || message["content"].as_array().is_some_and(|parts| {
                        parts.iter().any(|part| part["text"].as_str() == Some(text))
                    }))
        })
    })
}

/// A prompt delivered to a caused session reaches the provider behind the
/// runtime's `[message agent:… cause:…]` sender header.
fn has_session_message_body(request: &Value, body: &str) -> bool {
    request["messages"].as_array().is_some_and(|messages| {
        messages.iter().any(|message| {
            message["role"] == "user"
                && message["content"].as_str().is_some_and(|content| {
                    content.starts_with("[message agent:")
                        && content
                            .strip_suffix(body)
                            .is_some_and(|header| header.ends_with("]\n\n"))
                })
        })
    })
}

async fn wait_for_main_provider_request(
    captures: impl Fn() -> Vec<Value>,
    prompt: &str,
) -> Result<()> {
    tokio::time::timeout(Duration::from_secs(30), async {
        while !captures()
            .iter()
            .any(|request| has_exact_user_text(request, prompt))
        {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await
    .context("initial main provider request was not held")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_shim_turn_steer_queues_gents_request_on_active_turn() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-codex-shim-steer-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start_hanging(&model_name)?;

    let server_port = allocate_port()?;
    let graphql = graphql_url(server_port);
    let node_name = format!("cli-codex-shim-steer-{}", Uuid::new_v4().simple());
    let init = run_init_json(
        &home_dir,
        &[
            "--node-name",
            &node_name,
            "--model-name",
            &model_name,
            "--inference-url",
            mock_endpoint.endpoint(),
        ],
    )?;
    let node_did = node_did_from_init(&init)?;
    let shim_port = allocate_port()?;
    let shim_port_string = shim_port.to_string();
    let mut serve = spawn_server_with_env(
        &home_dir,
        server_port,
        &[
            "--codex-shim-port",
            &shim_port_string,
            "--codex-shim-poll-ms",
            "100",
            "--codex-shim-timeout-secs",
            "60",
        ],
        &[],
    )?;
    wait_for_port(server_port, &mut serve)?;
    wait_for_port(shim_port, &mut serve)?;
    serve
        .capturing(wait_for_runtime_ready(
            &graphql,
            &node_did,
            Duration::from_secs(30),
        ))
        .await?;

    let (mut ws, _) = serve
        .capturing(async {
            connect_async(format!("ws://127.0.0.1:{shim_port}/"))
                .await
                .context("connecting to codex-shim websocket")
        })
        .await?;
    initialize_config_and_thread(&mut ws, &home_dir).await?;
    let thread_id = start_thread(&mut ws, &home_dir).await?;

    let initial_prompt = format!("hold the turn open {}", Uuid::new_v4().simple());
    send_client_request(
        &mut ws,
        codex::ClientRequest::TurnStart {
            request_id: request_id(201),
            params: codex::TurnStartParams {
                thread_id: thread_id.clone(),
                input: vec![codex::UserInput::Text {
                    text: initial_prompt.clone(),
                    text_elements: Vec::new(),
                }],
                ..Default::default()
            },
        },
    )
    .await?;
    let turn_start: codex::TurnStartResponse =
        read_typed_response(&mut ws, request_id(201)).await?;
    let started = read_turn_started(&mut ws).await?;
    assert_eq!(started.turn.id, turn_start.turn.id);
    wait_for_main_provider_request(|| mock_endpoint.captured_chat_requests(), &initial_prompt)
        .await?;

    send_client_request(
        &mut ws,
        codex::ClientRequest::TurnSteer {
            request_id: request_id(202),
            params: codex::TurnSteerParams {
                thread_id: thread_id.clone(),
                input: vec![codex::UserInput::Text {
                    text: "wrong expected turn".to_string(),
                    text_elements: Vec::new(),
                }],
                responsesapi_client_metadata: None,
                expected_turn_id: "stale-turn".to_string(),
            },
        },
    )
    .await?;
    let error = read_error_response(&mut ws, request_id(202)).await?;
    assert_eq!(
        error.message,
        format!(
            "expected active turn id `stale-turn` but found `{}`",
            turn_start.turn.id
        )
    );

    let steer_prompt = format!("steer while active {}", Uuid::new_v4().simple());
    send_client_request(
        &mut ws,
        codex::ClientRequest::TurnSteer {
            request_id: request_id(203),
            params: codex::TurnSteerParams {
                thread_id: thread_id.clone(),
                input: vec![codex::UserInput::Text {
                    text: steer_prompt.clone(),
                    text_elements: Vec::new(),
                }],
                responsesapi_client_metadata: None,
                expected_turn_id: turn_start.turn.id.clone(),
            },
        },
    )
    .await?;
    let steer: codex::TurnSteerResponse = read_typed_response(&mut ws, request_id(203)).await?;
    assert_eq!(steer.turn_id, turn_start.turn.id);

    let (steering_request_id, session_id, input) =
        wait_for_request_input(&graphql, &node_did, &steer_prompt).await?;
    assert_eq!(session_id, thread_id);
    assert_eq!(
        input.queue.as_ref().expect("steering queue").source,
        gents_protocol::request_input::QueueSource::User
    );
    assert_eq!(
        input.queue.as_ref().expect("steering queue").policy,
        gents_protocol::request_input::QueuePolicy::Append
    );
    assert_eq!(
        input
            .queue
            .as_ref()
            .expect("steering queue")
            .queued_after_request_id
            .as_deref(),
        Some(turn_start.turn.id.as_str())
    );
    assert_ne!(steering_request_id, turn_start.turn.id);

    let second_steer_prompt = format!("second steer while active {}", Uuid::new_v4().simple());
    send_client_request(
        &mut ws,
        codex::ClientRequest::TurnSteer {
            request_id: request_id(205),
            params: codex::TurnSteerParams {
                thread_id: thread_id.clone(),
                input: vec![codex::UserInput::Text {
                    text: second_steer_prompt.clone(),
                    text_elements: Vec::new(),
                }],
                responsesapi_client_metadata: None,
                expected_turn_id: turn_start.turn.id.clone(),
            },
        },
    )
    .await?;
    let second_steer: codex::TurnSteerResponse =
        read_typed_response(&mut ws, request_id(205)).await?;
    assert_eq!(second_steer.turn_id, turn_start.turn.id);

    let (second_steering_request_id, second_session_id, second_input) =
        wait_for_request_input(&graphql, &node_did, &second_steer_prompt).await?;
    assert_eq!(second_session_id, thread_id);
    assert_eq!(
        second_input.queue.as_ref().expect("steering queue").queued_after_request_id.as_deref(),
        Some(steering_request_id.as_str()),
        "second steering request should queue after the current GENTS tail, not after the root turn"
    );

    send_client_request(
        &mut ws,
        codex::ClientRequest::TurnInterrupt {
            request_id: request_id(204),
            params: codex::TurnInterruptParams {
                thread_id,
                turn_id: turn_start.turn.id,
            },
        },
    )
    .await?;
    let _: codex::TurnInterruptResponse = read_typed_response(&mut ws, request_id(204)).await?;
    wait_for_request_lifecycle_state(
        &graphql,
        &steering_request_id,
        &["interrupted"],
        Duration::from_secs(15),
    )
    .await?;
    wait_for_request_lifecycle_state(
        &graphql,
        &second_steering_request_id,
        &["interrupted"],
        Duration::from_secs(15),
    )
    .await?;

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_shim_interrupt_completes_with_running_background_tool() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-codex-shim-bg-interrupt-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start_hanging(&model_name)?;

    let server_port = allocate_port()?;
    let graphql = graphql_url(server_port);
    let node_name = format!("cli-codex-shim-bg-interrupt-{}", Uuid::new_v4().simple());
    let init = run_init_json(
        &home_dir,
        &[
            "--node-name",
            &node_name,
            "--model-name",
            &model_name,
            "--inference-url",
            mock_endpoint.endpoint(),
        ],
    )?;
    let node_did = node_did_from_init(&init)?;
    let shim_port = allocate_port()?;
    let shim_port_string = shim_port.to_string();
    let mut serve = spawn_server_with_env(
        &home_dir,
        server_port,
        &[
            "--codex-shim-port",
            &shim_port_string,
            "--codex-shim-poll-ms",
            "50",
            "--codex-shim-timeout-secs",
            "60",
        ],
        &[],
    )?;
    wait_for_port(server_port, &mut serve)?;
    wait_for_port(shim_port, &mut serve)?;
    serve
        .capturing(wait_for_runtime_ready(
            &graphql,
            &node_did,
            Duration::from_secs(30),
        ))
        .await?;

    let (mut ws, _) = serve
        .capturing(async {
            connect_async(format!("ws://127.0.0.1:{shim_port}/"))
                .await
                .context("connecting to codex-shim websocket")
        })
        .await?;
    initialize_config_and_thread(&mut ws, &home_dir).await?;
    let thread_id = start_thread(&mut ws, &home_dir).await?;

    let prompt = format!(
        "start background interrupt repro {}",
        Uuid::new_v4().simple()
    );
    send_client_request(
        &mut ws,
        codex::ClientRequest::TurnStart {
            request_id: request_id(220),
            params: codex::TurnStartParams {
                thread_id: thread_id.clone(),
                input: vec![codex::UserInput::Text {
                    text: prompt.clone(),
                    text_elements: Vec::new(),
                }],
                ..Default::default()
            },
        },
    )
    .await?;
    let turn_start: codex::TurnStartResponse =
        read_typed_response(&mut ws, request_id(220)).await?;
    let started = read_turn_started(&mut ws).await?;
    assert_eq!(started.turn.id, turn_start.turn.id);

    let (gents_request_id, session_id, _agent_id) =
        wait_for_request(&graphql, &node_did, &prompt).await?;
    assert_eq!(session_id, thread_id);
    let tool_call_key = format!("{session_id}:codex-bg-interrupt");
    seed_running_background_tool(&graphql, &gents_request_id, &session_id, &tool_call_key).await?;

    let started_process = tokio::time::timeout(
        Duration::from_secs(15),
        read_background_command_started(&mut ws, &tool_call_key),
    )
    .await
    .context("timed out waiting for shim to project running background tool")??;
    assert_eq!(started_process, tool_call_key);

    send_client_request(
        &mut ws,
        codex::ClientRequest::TurnInterrupt {
            request_id: request_id(221),
            params: codex::TurnInterruptParams {
                thread_id: thread_id.clone(),
                turn_id: turn_start.turn.id.clone(),
            },
        },
    )
    .await?;
    let interrupted_turn = tokio::time::timeout(
        Duration::from_secs(15),
        read_interrupt_response_and_completed_turn(&mut ws, request_id(221)),
    )
    .await
    .context("timed out waiting for interrupted turn with running background tool")??;
    assert_eq!(interrupted_turn.status, codex::TurnStatus::Interrupted);

    wait_for_request_lifecycle_state(
        &graphql,
        &gents_request_id,
        &["interrupted"],
        Duration::from_secs(15),
    )
    .await?;

    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadList {
            request_id: request_id(222),
            params: codex::ThreadListParams {
                cursor: None,
                limit: None,
                sort_key: None,
                sort_direction: None,
                model_providers: None,
                source_kinds: None,
                archived: None,
                cwd: None,
                use_state_db_only: true,
                search_term: None,
            },
        },
    )
    .await?;
    let _: codex::ThreadListResponse = tokio::time::timeout(
        Duration::from_secs(15),
        read_typed_response(&mut ws, request_id(222)),
    )
    .await
    .context("shim stopped answering after interrupting background-tool turn")??;

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_shim_turn_steer_drains_queued_request_before_completing_turn() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-codex-shim-steer-drain-{}", Uuid::new_v4().simple());
    let initial_prompt = format!("first active turn {}", Uuid::new_v4().simple());
    let steer_prompt = format!("queued steering {}", Uuid::new_v4().simple());
    let first_reply = format!("first-drain-{}", Uuid::new_v4().simple());
    let second_reply = format!("second-drain-{}", Uuid::new_v4().simple());
    let first_response_gate = Arc::new(tokio::sync::Semaphore::new(0));
    let responder_gate = first_response_gate.clone();
    let responder_initial = initial_prompt.clone();
    let responder_steer = steer_prompt.clone();
    let responder_first = first_reply.clone();
    let responder_second = second_reply.clone();
    let mock_endpoint = FakeLlm::start(
        &model_name,
        None,
        Arc::new(move |request| {
            if has_exact_user_text(request, &responder_steer) {
                ChatAction::Sse(completion_text_sse(&responder_second))
            } else if has_exact_user_text(request, &responder_initial) {
                ChatAction::WaitThenSse(
                    responder_gate.clone(),
                    completion_text_sse(&responder_first),
                )
            } else {
                ChatAction::Sse(completion_text_sse("steer-drain-title"))
            }
        }),
    )?;

    let server_port = allocate_port()?;
    let graphql = graphql_url(server_port);
    let node_name = format!("cli-codex-shim-steer-drain-{}", Uuid::new_v4().simple());
    let init = run_init_json(
        &home_dir,
        &[
            "--node-name",
            &node_name,
            "--model-name",
            &model_name,
            "--inference-url",
            mock_endpoint.endpoint(),
        ],
    )?;
    let node_did = node_did_from_init(&init)?;
    let shim_port = allocate_port()?;
    let shim_port_string = shim_port.to_string();
    let mut serve = spawn_server_with_env(
        &home_dir,
        server_port,
        &[
            "--codex-shim-port",
            &shim_port_string,
            "--codex-shim-poll-ms",
            "50",
            "--codex-shim-timeout-secs",
            "60",
        ],
        &[],
    )?;
    wait_for_port(server_port, &mut serve)?;
    wait_for_port(shim_port, &mut serve)?;
    serve
        .capturing(wait_for_runtime_ready(
            &graphql,
            &node_did,
            Duration::from_secs(30),
        ))
        .await?;

    let (mut ws, _) = serve
        .capturing(async {
            connect_async(format!("ws://127.0.0.1:{shim_port}/"))
                .await
                .context("connecting to codex-shim websocket")
        })
        .await?;
    initialize_config_and_thread(&mut ws, &home_dir).await?;
    let thread_id = start_thread(&mut ws, &home_dir).await?;

    send_client_request(
        &mut ws,
        codex::ClientRequest::TurnStart {
            request_id: request_id(301),
            params: codex::TurnStartParams {
                thread_id: thread_id.clone(),
                input: vec![codex::UserInput::Text {
                    text: initial_prompt.clone(),
                    text_elements: Vec::new(),
                }],
                ..Default::default()
            },
        },
    )
    .await?;
    let turn_start: codex::TurnStartResponse =
        read_typed_response(&mut ws, request_id(301)).await?;
    let started = read_turn_started(&mut ws).await?;
    assert_eq!(started.turn.id, turn_start.turn.id);
    wait_for_main_provider_request(|| mock_endpoint.captured_chat_requests(), &initial_prompt)
        .await?;

    send_client_request(
        &mut ws,
        codex::ClientRequest::TurnSteer {
            request_id: request_id(302),
            params: codex::TurnSteerParams {
                thread_id: thread_id.clone(),
                input: vec![codex::UserInput::Text {
                    text: steer_prompt.clone(),
                    text_elements: Vec::new(),
                }],
                responsesapi_client_metadata: None,
                expected_turn_id: turn_start.turn.id.clone(),
            },
        },
    )
    .await?;
    let steer: codex::TurnSteerResponse = read_typed_response(&mut ws, request_id(302)).await?;
    assert_eq!(steer.turn_id, turn_start.turn.id);
    wait_for_request_input(&graphql, &node_did, &steer_prompt).await?;
    first_response_gate.add_permits(1);

    let capture = read_turn_capture(&mut ws).await?;
    assert_eq!(capture.turn.status, codex::TurnStatus::Completed);
    assert!(
        capture.text.contains(&first_reply),
        "turn completed before streaming first reply {first_reply}; text:\n{}",
        capture.text
    );
    assert!(
        capture.text.contains(&second_reply),
        "turn completed before draining steering reply {second_reply}; text:\n{}",
        capture.text
    );

    let (initial_request_id, initial_session_id, _agent_id) =
        wait_for_request(&graphql, &node_did, &initial_prompt).await?;
    assert_eq!(initial_session_id, thread_id);
    let (steering_request_id, steering_session_id, input) =
        wait_for_request_input(&graphql, &node_did, &steer_prompt).await?;
    assert_eq!(steering_session_id, thread_id);
    assert_ne!(steering_request_id, turn_start.turn.id);
    assert_eq!(
        input.queue.as_ref().expect("steering queue").source,
        gents_protocol::request_input::QueueSource::User
    );
    assert_eq!(
        input
            .queue
            .as_ref()
            .expect("steering queue")
            .queued_after_request_id
            .as_deref(),
        Some(turn_start.turn.id.as_str())
    );

    assert_eq!(
        input.queue.as_ref().unwrap().delivery,
        gents_protocol::request_input::QueueDelivery::Steer
    );
    assert_eq!(
        capture
            .completed_user_inputs
            .iter()
            .filter(|items| items.iter().any(
                |item| matches!(item, codex::UserInput::Text { text, .. } if text == &steer_prompt)
            ))
            .count(),
        1,
        "consumed input is announced exactly once on the original Codex turn"
    );
    let rows = graphql_query(&graphql, &format!(r#"{{ AgentRequest(filter: {{ node_did: {{_eq:"{}"}}, session_id: {{_eq:"{}"}}, request_id: {{_in:["{}","{}"]}} }}) {{ _docID request_id lifecycle_state superseded_by_request_doc_id failure_reason }} }}"#,
        escape_graphql_string(&node_did), escape_graphql_string(&thread_id), escape_graphql_string(&initial_request_id), escape_graphql_string(&steering_request_id))).await?;
    let rows = rows["data"]["AgentRequest"]
        .as_array()
        .context("request rows")?;
    let active = rows
        .iter()
        .find(|row| row["request_id"] == initial_request_id)
        .context("original active request")?;
    let consumed = rows
        .iter()
        .find(|row| row["request_id"] == steering_request_id)
        .context("consumed steering request")?;
    assert_eq!(active["lifecycle_state"], "completed");
    assert_eq!(consumed["lifecycle_state"], "superseded");
    assert_eq!(consumed["failure_reason"], gents::lifecycle::FOLDED_REASON);
    assert_eq!(consumed["superseded_by_request_doc_id"], active["_docID"]);
    let captures = graphql_query(&graphql, &format!(r#"{{ RenderedRequest(filter: {{ request_id: {{_in:["{}","{}"]}} }}) {{ request_id request_doc_id }} }}"#, escape_graphql_string(&initial_request_id), escape_graphql_string(&steering_request_id))).await?;
    let captures = captures["data"]["RenderedRequest"]
        .as_array()
        .context("rendered requests")?;
    assert!(captures.len() >= 2);
    assert!(
        captures
            .iter()
            .all(|capture| capture["request_doc_id"] == active["_docID"]),
        "steering must not start a second physical inference request"
    );

    let captured_requests = mock_endpoint.captured_chat_requests();
    assert!(
        captured_requests
            .iter()
            .any(|request| request_contains_role_text(request, "user", &initial_prompt)),
        "mock endpoint did not receive the initial prompt; captured={captured_requests:?}"
    );
    assert!(
        captured_requests
            .iter()
            .any(|request| request_contains_role_text(request, "user", &steer_prompt)),
        "mock endpoint did not receive the steering prompt; captured={captured_requests:?}"
    );

    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadRead {
            request_id: request_id(303),
            params: codex::ThreadReadParams {
                thread_id: thread_id.clone(),
                include_turns: true,
            },
        },
    )
    .await?;
    let thread_history: codex::ThreadReadResponse =
        read_typed_response(&mut ws, request_id(303)).await?;
    assert_eq!(
        thread_history.thread.turns.len(),
        1,
        "queued steering should reload as one Codex turn"
    );
    let history_turn = &thread_history.thread.turns[0];
    assert_eq!(history_turn.id, turn_start.turn.id);
    assert_turn_has_user_text(history_turn, &initial_prompt);
    assert_turn_has_agent_text(history_turn, &first_reply);
    assert_turn_has_user_text(history_turn, &steer_prompt);
    assert_turn_has_agent_text(history_turn, &second_reply);

    Ok(())
}

async fn wait_for_real_caused_request(graphql: &str, parent: &str) -> Result<(String, String)> {
    let (parent_doc, _, _) = exact_request_binding(graphql, parent).await?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let response = graphql_query(graphql, &format!(
            r#"{{ AgentRequest(filter: {{caused_by_parent_request_doc_id: {{_eq: "{}"}}}}) {{request_id session_id lifecycle_state admission_signature}} }}"#,
            escape_graphql_string(&parent_doc),
        )).await?;
        for row in response["data"]["AgentRequest"]
            .as_array()
            .into_iter()
            .flatten()
        {
            if row["lifecycle_state"] == "processing"
                && row["admission_signature"]
                    .as_str()
                    .is_some_and(|signature| !signature.is_empty())
            {
                let session = row["session_id"].as_str().context("caused session id")?;
                let session_rows = graphql_query(graphql, &format!(
                    r#"{{ AgentSession(filter: {{session_id: {{_eq: "{}"}}}}) {{provenance}} }}"#,
                    escape_graphql_string(session),
                )).await?;
                if session_rows["data"]["AgentSession"]
                    .as_array()
                    .is_some_and(|rows| {
                        rows.iter().any(|row| {
                            row["provenance"]["parent_request_doc_id"] == parent_doc
                                && row["provenance"]["creation_receipt"].is_object()
                        })
                    })
                {
                    return Ok((
                        row["request_id"]
                            .as_str()
                            .context("caused request id")?
                            .to_owned(),
                        session.to_owned(),
                    ));
                }
            }
        }
        anyhow::ensure!(
            tokio::time::Instant::now() < deadline,
            "signed caused request did not reach processing: {response}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_shim_turn_interrupt_on_caused_thread_interrupts_its_own_request() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-codex-shim-caused-{}", Uuid::new_v4().simple());
    const PARENT: &str = "start caused work fixture";
    const CHILD: &str = "start grandchild fixture";
    const GRANDCHILD: &str = "hold grandchild fixture";
    let mock_endpoint = FakeLlm::start(
        &model_name,
        None,
        Arc::new(|request| {
            let has_tool_result = request["messages"]
                .as_array()
                .is_some_and(|messages| messages.iter().any(|message| message["role"] == "tool"));
            if has_session_message_body(request, GRANDCHILD) || has_tool_result {
                ChatAction::Hang
            } else if has_session_message_body(request, CHILD) {
                ChatAction::Sse(tool_call_sse_with_id(
                    "child-agent-new",
                    "agent_new",
                    &json!({"agent":"fixture-child", "prompt":GRANDCHILD}).to_string(),
                ))
            } else if has_exact_user_text(request, PARENT) {
                ChatAction::Sse(tool_call_sse_with_id(
                    "parent-agent-new",
                    "agent_new",
                    &json!({"agent":"fixture-child", "prompt":CHILD}).to_string(),
                ))
            } else {
                ChatAction::Sse(completion_text_sse("fixture session"))
            }
        }),
    )?;

    let server_port = allocate_port()?;
    let graphql = graphql_url(server_port);
    let node_name = format!("cli-codex-shim-caused-{}", Uuid::new_v4().simple());
    let init = run_init_json(
        &home_dir,
        &[
            "--node-name",
            &node_name,
            "--model-name",
            &model_name,
            "--inference-url",
            mock_endpoint.endpoint(),
            "--max-concurrent",
            "4",
        ],
    )?;
    let node_did = node_did_from_init(&init)?;
    let shim_port = allocate_port()?;
    let shim_port_string = shim_port.to_string();
    let mut serve = spawn_server_with_env(
        &home_dir,
        server_port,
        &[
            "--codex-shim-port",
            &shim_port_string,
            "--codex-shim-poll-ms",
            "100",
            "--codex-shim-timeout-secs",
            "60",
        ],
        &[],
    )?;
    wait_for_port(server_port, &mut serve)?;
    wait_for_port(shim_port, &mut serve)?;
    serve
        .capturing(wait_for_runtime_ready(
            &graphql,
            &node_did,
            Duration::from_secs(30),
        ))
        .await?;

    let agent_id = init["default_agent_id"]
        .as_str()
        .context("default agent id")?;
    let agent = graphql_query(
        &graphql,
        &format!(
            r#"{{ Agent(filter: {{agent_id: {{_eq: "{}"}}}}) {{context_id}} }}"#,
            escape_graphql_string(agent_id)
        ),
    )
    .await?;
    let context_id = agent["data"]["Agent"][0]["context_id"]
        .as_str()
        .context("agent context")?;
    let context = graphql_query(
        &graphql,
        &format!(
            r#"{{ AgentContext(filter: {{context_id: {{_eq: "{}"}}}}) {{tools_id}} }}"#,
            escape_graphql_string(context_id)
        ),
    )
    .await?;
    let tools_id = context["data"]["AgentContext"][0]["tools_id"]
        .as_str()
        .context("agent tools")?;
    let mutation = format!(
        r#"mutation {{
        create_AgentTarget(input: {{target_id: "fixture-child", node_did: "{node}", target_node_did: "{node}", agent_id: "{agent}", name: "fixture-child"}}) {{_docID}}
        update_Tools(filter: {{node_did: {{_eq: "{node}"}}, tools_id: {{_eq: "{tools}"}}}}, input: {{agents: {{enabled: true, target_ids: ["fixture-child"]}}}}) {{_docID}}
    }}"#,
        node = escape_graphql_string(&node_did),
        agent = escape_graphql_string(agent_id),
        tools = escape_graphql_string(tools_id)
    );
    let configured = graphql_query(&graphql, &mutation).await?;
    doc_id_from_create(&configured, "add_AgentTarget")
        .context("creating the caused-work target")?;
    first_graphql_row(&configured, "update_Tools").context("enabling caused-work tools")?;
    wait_for_runtime_quiescence(&graphql, &node_did, 2, Duration::from_millis(500)).await?;

    let (mut ws, _) = serve
        .capturing(async {
            connect_async(format!("ws://127.0.0.1:{shim_port}/"))
                .await
                .context("connecting to codex-shim websocket")
        })
        .await?;
    initialize_config_and_thread(&mut ws, &home_dir).await?;
    let thread_id = start_thread(&mut ws, &home_dir).await?;

    let prompt = PARENT.to_owned();
    send_client_request(
        &mut ws,
        codex::ClientRequest::TurnStart {
            request_id: request_id(230),
            params: codex::TurnStartParams {
                thread_id: thread_id.clone(),
                input: vec![codex::UserInput::Text {
                    text: prompt.clone(),
                    text_elements: Vec::new(),
                }],
                ..Default::default()
            },
        },
    )
    .await?;
    let _: codex::TurnStartResponse = read_typed_response(&mut ws, request_id(230)).await?;
    let (parent_request_id, session_id, _) = wait_for_request(&graphql, &node_did, &prompt).await?;
    assert_eq!(session_id, thread_id);

    let (caused_request_id, caused_thread_id) =
        wait_for_real_caused_request(&graphql, &parent_request_id).await?;
    let (grandchild_request_id, grandchild_thread_id) =
        wait_for_real_caused_request(&graphql, &caused_request_id).await?;

    send_client_request(
        &mut ws,
        codex::ClientRequest::TurnInterrupt {
            request_id: request_id(232),
            params: codex::TurnInterruptParams {
                thread_id: grandchild_thread_id,
                turn_id: grandchild_request_id,
            },
        },
    )
    .await?;
    let refused = read_error_response(&mut ws, request_id(232)).await?;
    assert_eq!(
        refused.message,
        "only the session that started this session or a same-node agent for a Task-triggered session may interrupt it"
    );

    send_client_request(
        &mut ws,
        codex::ClientRequest::TurnInterrupt {
            request_id: request_id(231),
            params: codex::TurnInterruptParams {
                thread_id: caused_thread_id.clone(),
                turn_id: caused_request_id.clone(),
            },
        },
    )
    .await?;
    let _: codex::TurnInterruptResponse = tokio::time::timeout(
        Duration::from_secs(15),
        read_typed_response(&mut ws, request_id(231)),
    )
    .await
    .context("timed out waiting for the caused-thread interrupt response")??;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    loop {
        let response = graphql_query(
            &graphql,
            &format!(
                r#"{{ AgentRequest(filter: {{ request_id: {{ _in: ["{}", "{}"] }} }}) {{ request_id interrupt_requested_at lifecycle_state }} }}"#,
                escape_graphql_string(&caused_request_id),
                escape_graphql_string(&parent_request_id),
            ),
        )
        .await?;
        let rows = response
            .pointer("/data/AgentRequest")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let marked = |id: &str| {
            rows.iter().any(|row| {
                row["request_id"] == id
                    && (row["interrupt_requested_at"]
                        .as_str()
                        .is_some_and(|at| !at.is_empty())
                        || row["lifecycle_state"] == "interrupted")
            })
        };
        if marked(&caused_request_id) {
            assert!(
                !marked(&parent_request_id),
                "interrupting a caused thread must not interrupt the thread that caused it"
            );
            break;
        }
        anyhow::ensure!(
            tokio::time::Instant::now() < deadline,
            "caused thread's own request was never interrupted: {rows:?}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Ok(())
}
