//! A plugin job's result reaches the parent as a bounded notification and is
//! paged in full through `job_calls::result`.

use std::time::Duration;

use gents::plugin::data::StampContext;
use gents::plugin::job_calls::{result as read_result, JobCaller};
use gents::tools::background::job::job::JobState;
use gents::tools::background::status::ProcessControlScope;
use gents::ConfigAccess;
use gents_protocol::job::JobResultRequest;
use serde_json::{json, Value};

use crate::plugin_job_support::{
    boot_job_harness, effects, guest_pin, job_call, job_row, result, spawn, wait_job, JobHarness,
    PlanEntry, PLUGIN,
};
use crate::support::streaming_backend::{StreamPlan, StreamResponse};
use crate::support::test_db;

const TOOL_CALL: &str = "job-s";
const CHILD_PROMPT: &str = "surface child a";

fn entry(when: Value, output: gents::tools::background::job::job::JobStepOutput) -> PlanEntry {
    PlanEntry {
        when,
        output,
        control: None,
    }
}

/// Spawns slot `a`, then completes with `value` once it does.
async fn completed_harness(name: &str, value: Value) -> (JobHarness, String) {
    let plan = [
        entry(
            json!({"step": 0}),
            effects(
                Some(guest_pin("job-s-handle")),
                vec![spawn("a", CHILD_PROMPT)],
            ),
        ),
        entry(json!({"children": {"a": "completed"}}), result(value)),
        entry(
            json!({"children": {"a": "live"}}),
            effects(None, Vec::new()),
        ),
    ];
    let plugin_home = tempfile::tempdir().expect("plugin home");
    let h = boot_job_harness(
        test_db(name).await,
        plugin_home.keep(),
        &format!("{name}-session"),
        vec![job_call(TOOL_CALL, &plan)],
        vec![StreamPlan::current_authored_user(
            CHILD_PROMPT,
            vec![StreamResponse::completes(CHILD_PROMPT, ["child a done"])],
        )],
    )
    .await;
    wait_job(
        &h,
        TOOL_CALL,
        |view| view.state == JobState::Completed,
        Duration::from_secs(30),
    )
    .await;
    let row = job_row(&h, TOOL_CALL).await;
    (h, row)
}

fn caller(h: &JobHarness, session_id: &str, request_doc_id: &str, row_doc_id: &str) -> JobCaller {
    let node_did = h.db.node_identity.did().to_string();
    JobCaller::new(
        h.db.node.clone(),
        ProcessControlScope {
            request_id: format!("{session_id}-parent"),
            session_id: session_id.into(),
            node_did: node_did.clone(),
            requester_did: Some(node_did.clone()),
        },
        StampContext::ToolOutput {
            node_did,
            session_id: session_id.into(),
            request_doc_id: request_doc_id.into(),
            tool_call_doc_id: row_doc_id.into(),
        },
    )
}

async fn parent_request_doc_id(h: &JobHarness) -> String {
    let response = ConfigAccess::local(h.db.node.clone())
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}-parent" }} }}, limit: 1) {{ _docID }} }}"#,
            gents::store::graphql::escape_graphql_string(&h.session_id)
        ))
        .await
        .expect("read the parent request");
    response["data"]["AgentRequest"][0]["_docID"]
        .as_str()
        .expect("parent request doc id")
        .to_owned()
}

async fn session_texts(h: &JobHarness) -> Vec<String> {
    let node = h.db.node.as_ref();
    let session_id = gents::store::graphql::escape_graphql_string(&h.session_id);
    let response = node
        .execute(&format!(
            r#"{{ AgentMessage(filter: {{ session_id: {{ _eq: "{session_id}" }} }}, order: {{ sequence: ASC }}) {{ _docID node_did requester_did }} }}"#
        ))
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let headers = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentMessage"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut texts = Vec::new();
    for header in headers {
        let (_, message) = gents::session::load_canonical_message_from_node(
            node,
            header["_docID"].as_str().expect("message _docID"),
            header["node_did"].as_str().expect("message node_did"),
            header["requester_did"].as_str(),
        )
        .await
        .expect("reconstruct the message");
        if let gents::inference::llm::message::Message::User { content } = message {
            texts.extend(content.into_iter().filter_map(|item| match item {
                gents::inference::llm::message::UserContent::Text(text) => Some(text.text),
                _ => None,
            }));
        }
    }
    texts
}

/// The text of the parent session's completion notification for the job.
async fn notification(h: &JobHarness) -> String {
    let marker = format!("<tool-completion tool_call_id=\"{TOOL_CALL}\"");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(text) = session_texts(h)
            .await
            .into_iter()
            .find(|text| text.contains(&marker))
        {
            return text;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "no completion notification for {TOOL_CALL}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn job_result_over_4000_bytes_is_truncated_in_the_notification_and_paged_in_full() {
    let (h, row_doc_id) =
        completed_harness("plugin-job-surface-page", json!("x".repeat(5000))).await;

    let text = notification(&h).await;
    let body = text
        .split_once("<result>")
        .and_then(|(_, rest)| rest.split_once("</result>"))
        .unwrap_or_else(|| panic!("no <result> body in {text}"))
        .0;
    let shown = body
        .strip_suffix("...")
        .unwrap_or_else(|| panic!("the cut result must end with the marker: {body}"));
    assert!(shown.len() <= 4000, "{} bytes shown", shown.len());

    let request_doc_id = parent_request_doc_id(&h).await;
    let caller = caller(&h, &h.session_id, &request_doc_id, &row_doc_id);
    let page = |offset: u64, limit: u64| {
        let request = JobResultRequest {
            row_doc_id: row_doc_id.clone(),
            offset,
            limit,
        };
        let caller = &caller;
        async move {
            read_result(caller, PLUGIN, &request, usize::MAX)
                .await
                .expect("page the job result")
        }
    };
    let full = page(0, u64::MAX).await;
    assert!(!full.truncated, "{full:?}");
    assert_eq!(full.total_bytes as usize, full.result.len());
    assert!(full.result.len() > 4000, "{} bytes", full.result.len());
    assert!(full.result.contains(&"x".repeat(5000)), "{}", full.result);

    let first = page(0, full.total_bytes / 2).await;
    let second = page(first.offset + first.result.len() as u64, u64::MAX).await;
    assert_eq!(format!("{}{}", first.result, second.result), full.result);
    h.runtime.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn job_calls_result_refuses_a_row_outside_process_control_scope() {
    let (h, row_doc_id) = completed_harness("plugin-job-surface-scope", json!("done")).await;
    let request_doc_id = parent_request_doc_id(&h).await;

    let caller = caller(&h, "another-session", &request_doc_id, &row_doc_id);
    let error = read_result(
        &caller,
        PLUGIN,
        &JobResultRequest {
            row_doc_id: row_doc_id.clone(),
            offset: 0,
            limit: u64::MAX,
        },
        usize::MAX,
    )
    .await
    .expect_err("a row of another session is refused")
    .to_string();
    assert!(error.contains(&row_doc_id), "{error}");
    assert!(error.contains("is not in this call's scope"), "{error}");
    h.runtime.shutdown().await;
}
