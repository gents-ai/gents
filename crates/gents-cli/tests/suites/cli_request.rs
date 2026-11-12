use crate::support::*;

use std::fs;
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::Value;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn request_show_expanded_view_surfaces_background_tools_and_child_lineage() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let model_name = format!("mock-show-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let port = allocate_port()?;
    let agent_name = format!("cli-show-{}", Uuid::new_v4().simple());
    let graphql = graphql_url(port);
    let init = run_init_json(
        &home_dir,
        &[
            "--node-name",
            &agent_name,
            "--model-name",
            &model_name,
            "--inference-url",
            mock_endpoint.endpoint(),
        ],
    )?;
    let node_did = node_did_from_init(&init)?;
    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &node_did, Duration::from_secs(30)).await?;

    let session_id = format!("show-session-{}", Uuid::new_v4().simple());
    let parent_request_id = format!("show-parent-{}", Uuid::new_v4().simple());
    let child_request_id = format!("show-child-{}", Uuid::new_v4().simple());
    let child_session_id = format!("show-child-session-{}", Uuid::new_v4().simple());
    let tool_call_id = format!("show-tool-{}", Uuid::new_v4().simple());
    let tool_call_key = format!("{session_id}:{tool_call_id}");
    let parent_create = graphql_query(
        &graphql,
        &format!(
            r#"mutation {{
                create_AgentRequest(input: {{purpose: "normal", 
                    request_id: "{parent_request_id}",
                    node_did: "{node_did}",
                    agent_did: "parent-agent",
                    session_id: "{session_id}",
                    content: "parent request that started a session",
                    lifecycle_state: "processing",
                    backend_id: "studios-cluster",
                    execution_origin: "operatorCli",
                    created_at: "2026-05-20T10:00:00Z",
                    claimed_at: "2026-05-20T10:00:01Z",
                    deadline: "2026-05-20T10:05:00Z",
                    retry_count: 0,
                    request_hop: 0
                }}) {{ _docID }}
            }}"#
        ),
    )
    .await?;
    let parent_doc_id = doc_id_from_create(&parent_create, "add_AgentRequest")?;
    let tool_create = graphql_query(
        &graphql,
        &format!(
            r#"mutation {{
                create_AgentToolCall(input: {{
                    tool_call_key: "{tool_call_key}",
                    node_did: "{node_did}",
                    request_id: "{parent_request_id}",
                    request_doc_id: "{parent_doc_id}",
                    session_id: "{session_id}",
                    message_sequence: 1,
                    tool_name: "agent_new",
                    tool_call_id: "{tool_call_id}",
                    status: "called",
                    lifecycle_state: "running",
                    started_at: "2026-05-20T10:00:02Z",
                    deadline_at: "2026-05-20T10:05:00Z",
                    await_mode: "background"
                }}) {{ _docID }}
            }}"#,
            parent_doc_id = escape_graphql_string(&parent_doc_id),
        ),
    )
    .await?;
    let tool_call_doc_id = doc_id_from_create(&tool_create, "add_AgentToolCall")?;
    let child_create = graphql_query(
        &graphql,
        &format!(
            r#"mutation {{
                create_AgentRequest(input: {{purpose: "normal", 
                    request_id: "{child_request_id}",
                    node_did: "{node_did}",
                    requester_did: "{node_did}",
                    agent_did: "child-agent",
                    session_id: "{child_session_id}",
                    content: "request caused by the agent_new tool call",
                    lifecycle_state: "processing",
                    created_at: "2026-05-20T10:00:03Z",
                    claimed_at: "2026-05-20T10:00:04Z",
                    retry_count: 0,
                    request_hop: 1,
                    caused_by_parent_request_id: "{parent_request_id}",
                    caused_by_parent_request_doc_id: "{parent_doc_id}",
                    caused_by_parent_tool_call_id: "{tool_call_id}",
                    caused_by_parent_tool_call_doc_id: "{tool_call_doc_id}"
                }}) {{ _docID }}
            }}"#,
            parent_doc_id = escape_graphql_string(&parent_doc_id),
            tool_call_doc_id = escape_graphql_string(&tool_call_doc_id),
        ),
    )
    .await?;
    let child_doc_id = doc_id_from_create(&child_create, "add_AgentRequest")?;
    create_started_session(
        &graphql,
        &identity_from_init(&init)?,
        gents_protocol::session::AgentSession {
            session_id: child_session_id.clone(),
            node_did: node_did.clone(),
            requester_did: Some(node_did.clone()),
            agent_did: "child-agent".to_owned(),
            created_at: "2026-05-20T10:00:03Z".to_owned(),
            closed_at: None,
            archived_at: None,
            archive_revision: 0,
            stopped_at: None,
            title: None,
            tags: Vec::new(),
            provenance: None,
            observation: None,
        },
        gents_protocol::session::SessionReference {
            node_did: node_did.clone(),
            session_id: session_id.clone(),
            requester_did: None,
        },
        &parent_doc_id,
        &child_doc_id,
        &tool_call_doc_id,
    )
    .await?;

    let json_output = run_cli_json(
        &home_dir,
        &[
            "utils",
            "debug",
            "request",
            "show",
            "--graphql",
            &graphql,
            "--output",
            "json",
            &parent_request_id,
        ],
    )?;
    assert_eq!(
        json_output
            .pointer("/request/request_id")
            .and_then(Value::as_str),
        Some(parent_request_id.as_str())
    );
    assert_eq!(
        json_output
            .pointer("/tool_calls/0/await_mode")
            .and_then(Value::as_str),
        Some("background")
    );
    assert_eq!(
        json_output
            .pointer("/tool_calls/0/cancel_cause")
            .and_then(Value::as_str),
        Some("unknown"),
        "request show must render an absent cancel_cause as unknown"
    );
    assert_eq!(
        json_output
            .pointer("/tool_calls/0/active_tool_call")
            .and_then(Value::as_bool),
        Some(true),
        "running tool call must be linked to liveness.active_tool_calls"
    );
    assert_eq!(
        json_output
            .pointer("/backgrounded_tools/0/tool_call_id")
            .and_then(Value::as_str),
        Some(tool_call_id.as_str())
    );
    assert_eq!(
        json_output
            .pointer("/child_requests/0/request_id")
            .and_then(Value::as_str),
        Some(child_request_id.as_str())
    );
    assert_eq!(
        json_output
            .pointer("/child_requests/0/agent_did")
            .and_then(Value::as_str),
        Some("child-agent")
    );
    assert_eq!(
        json_output
            .pointer("/child_requests/0/session_id")
            .and_then(Value::as_str),
        Some(child_session_id.as_str())
    );
    assert_eq!(
        json_output
            .pointer("/child_requests/0/caused_by_parent_tool_call_id")
            .and_then(Value::as_str),
        Some(tool_call_id.as_str())
    );

    let text_output = run_cli_text(
        &home_dir,
        &[
            "utils",
            "debug",
            "request",
            "show",
            "--graphql",
            &graphql,
            &parent_request_id,
        ],
    )?;
    assert!(text_output.contains("Transition history:"));
    assert!(text_output.contains("Backgrounded tools:"));
    assert!(text_output.contains("await_mode=background"));
    assert!(text_output.contains(&child_request_id));
    assert_eq!(json_output.get("blocked"), Some(&Value::Null));

    let limited_request_id = format!("show-limited-{}", Uuid::new_v4().simple());
    let failure = "provider usage limit reached (resets at 2030-01-01T00:00:00Z): The usage limit has been reached";
    graphql_query(
        &graphql,
        &format!(
            r#"mutation {{
                create_AgentRequest(input: {{purpose: "normal",
                    request_id: "{limited_request_id}", node_did: "{node_did}",
                    agent_did: "parent-agent", session_id: "{session_id}",
                    content: "limited", lifecycle_state: "failed",
                    failure_reason: "{failure}", created_at: "2026-05-20T10:01:00Z"
                }}) {{ _docID }}
                create_InferenceCall(input: {{
                    call_id: "{limited_request_id}-call", request_id: "{limited_request_id}",
                    call_seq: 1, backend_id: "studios-cluster", agent_did: "parent-agent",
                    node_did: "{node_did}", call_kind: "inference", attempt: 1,
                    call_state: "failed", failure_reason: "{failure}",
                    queued_at: "2026-05-20T10:01:00Z", started_at: "2026-05-20T10:01:00Z",
                    ended_at: "2026-05-20T10:01:01Z"
                }}) {{ _docID }}
            }}"#
        ),
    )
    .await?;
    let limited = run_cli_json(
        &home_dir,
        &[
            "utils",
            "debug",
            "request",
            "show",
            "--graphql",
            &graphql,
            "--output",
            "json",
            &limited_request_id,
        ],
    )?;
    assert_eq!(
        limited.pointer("/blocked/reason").and_then(Value::as_str),
        Some("usage_limit"),
        "{limited}"
    );
    assert_eq!(
        limited
            .pointer("/blocked/resets_at")
            .and_then(Value::as_str),
        Some("2030-01-01T00:00:00Z")
    );

    Ok(())
}

async fn create_started_session(
    graphql: &str,
    node: &gents_store::identity::KeyIdentity,
    session: gents_protocol::session::AgentSession,
    source: gents_protocol::session::SessionReference,
    parent_doc_id: &str,
    opening_doc_id: &str,
    tool_call_doc_id: &str,
) -> Result<()> {
    use gents_protocol::session::{
        SessionCreationCause, SessionCreationReceipt, SessionCreationStarter, SessionProvenance,
    };
    use gents_store::identity::NodeIdentity as _;

    let mut provenance = SessionProvenance {
        parent_request_doc_id: Some(parent_doc_id.to_owned()),
        opening_request_doc_id: Some(opening_doc_id.to_owned()),
        started_by: Some(source.clone()),
        ..Default::default()
    };
    let mut input = serde_json::to_value(gents_protocol::session::AgentSession {
        provenance: Some(provenance.clone()),
        ..session.clone()
    })?;
    input["started_by_node_did"] = source.node_did.clone().into();
    input["started_by_session_id"] = source.session_id.clone().into();
    let created = graphql_query(
        graphql,
        &format!(
            "mutation {{ create_AgentSession(input: {}) {{ _docID }} }}",
            gents_protocol::graphql::graphql_input_literal(&input)?
        ),
    )
    .await?;
    let session_doc_id = doc_id_from_create(&created, "add_AgentSession")?;
    let mut receipt = SessionCreationReceipt {
        node_did: session.node_did,
        session_id: session.session_id,
        session_doc_id: session_doc_id.clone(),
        requester_did: session.requester_did,
        agent_did: session.agent_did,
        opening_request_doc_id: opening_doc_id.to_owned(),
        cause: SessionCreationCause::AgentNew,
        cause_binding: tool_call_doc_id.to_owned(),
        starter: Some(SessionCreationStarter {
            source,
            parent_request_doc_id: parent_doc_id.to_owned(),
        }),
        signature: Vec::new(),
    };
    receipt.signature = node.sign(&receipt.signing_payload()).await?;
    provenance.creation_receipt = Some(receipt);
    graphql_query(
        graphql,
        &format!(
            r#"mutation {{ update_AgentSession(docID: "{}", input: {}) {{ _docID }} }}"#,
            escape_graphql_string(&session_doc_id),
            gents_protocol::graphql::graphql_input_literal(
                &serde_json::json!({ "provenance": provenance })
            )?
        ),
    )
    .await?;
    Ok(())
}
