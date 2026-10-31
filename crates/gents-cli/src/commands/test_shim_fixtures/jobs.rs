use std::sync::Arc;

use defra_node::EmbeddedNode;
use gents::store::config_client::ConfigAccess;
use gents::store::graphql::ensure_no_errors;
use serde_json::json;

use super::tool_admission::seed_canonical_tool_call;
use super::tool_completion::seed_background_receipt;

pub(crate) async fn seed_job_row(
    node: &Arc<EmbeddedNode>,
    node_did: &str,
    session: &str,
    call: &str,
    handle: &str,
    lifecycle: &str,
    state: gents_protocol::job::JobState,
) -> gents_protocol::job::JobView {
    use gents_protocol::job::{JobBudget, JobCeiling, JobOrigin, JobPin, JobProgress};
    let mut pin = JobPin {
        script_id: "fixture-script".into(),
        source_digest: "sha256:source".into(),
        origin: JobOrigin::Installed,
        engine_digest: "sha256:engine".into(),
        plugin_ref: "fixture/workflow".into(),
        agent_budget: 4,
        handle: handle.into(),
        cwd: "/fixture/cwd".into(),
        adopted_at: 1_780_000_000_000,
        args_digest: "sha256:args".into(),
        chain_root_doc_id: String::new(),
        resume_of: None,
        replayed_from: None,
        ceiling_at_pin: JobCeiling {
            targets: vec!["general".into()],
            max_live: 2,
            max_spawns: 8,
            max_messages_per_slot: 4,
            result_bytes: 65_536,
            max_wall_secs: 600,
        },
    };
    let progress = JobProgress {
        title: format!("Job {call}"),
        state,
        pause_reason: None,
        phases: Vec::new(),
        active: Vec::new(),
        spawned: 2,
        budget: JobBudget {
            total: 4,
            spent: 2,
            remaining: 2,
        },
        last_event: "seeded".into(),
        last_event_at: None,
        question: None,
        observed_terminal_children: 0,
        revision: 1,
    };
    let ended = if lifecycle == "running" {
        ""
    } else {
        r#"completed_at: "2026-06-01T10:05:00Z","#
    };
    let request = seed_signed_request(
        node,
        node_did,
        session,
        &format!("request-{call}"),
        "2026-06-01T10:00:00Z",
    )
    .await;
    let doc_id = seed_canonical_tool_call(
        node, &request, call, "workflow", lifecycle, "{}", None, None, None, None,
    )
    .await;
    pin.chain_root_doc_id = doc_id.clone();
    let escaped_doc_id = gents::store::graphql::escape_graphql_string(&doc_id);
    let mutation = format!(
        r#"mutation($job_pin: JSON, $job_progress: JSON) {{ update_AgentToolCall(filter: {{ _docID: {{ _eq: "{escaped_doc_id}" }} }}, input: {{
            await_mode: "background", status: "{lifecycle}", deadline_at: "2126-06-01T10:00:00Z", {ended}
            job_pin: $job_pin, job_progress: $job_progress
        }}) {{ _docID }} }}"#
    );
    let variables = json!({ "job_pin": pin, "job_progress": progress });
    ConfigAccess::transact_local_write(node, None, "test.grok.workflow.job", |txn| {
        let (mutation, variables) = (&mutation, &variables);
        Box::pin(async move { txn.execute_with_variables(mutation, variables).await })
    })
    .await
    .unwrap();
    if lifecycle == "running" {
        seed_background_receipt(node, &request, &doc_id, call).await;
    }
    gents::tools::background::job::view::load_job_view(
        &ConfigAccess::local(node.clone()),
        None,
        gents::tools::background::job::view::JobKey::Address {
            session_id: session.into(),
            tool_call_id: call.into(),
        },
    )
    .await
    .unwrap()
}

/// A pending `local-self` request authored through the production request
/// builder and signed by `node_did`'s registered identity, so readers that
/// verify the owning request's admission accept the rows seeded under it.
async fn seed_signed_request(
    node: &EmbeddedNode,
    node_did: &str,
    session_id: &str,
    request_id: &str,
    created_at: &str,
) -> gents_protocol::row::AgentRequestRow {
    use gents_protocol::request_admission::{AgentRequestAdmissionRecord, RequestPurpose};
    let create = gents::build_signed_request(
        gents::RequestSpec::new(
            RequestPurpose::Normal,
            gents::RequestIdentity {
                request_id: request_id.into(),
                node_did: node_did.into(),
                requester_did: None,
                agent_id: "test".into(),
                session_id: session_id.into(),
                content: "test".into(),
                execution_origin: gents::request::lifecycle::ExecutionOrigin::Interactive,
                created_at: created_at.into(),
            },
            AgentRequestAdmissionRecord::local_self(node_did),
        ),
        gents::RequestSigner::RegisteredTarget,
    )
    .await
    .unwrap();
    let created = node.execute(&create.graphql_mutation().unwrap()).await;
    ensure_no_errors(&created, "seed signed request").unwrap();
    let row = node
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}) {{ {} }} }}"#,
            gents::store::graphql::escape_graphql_string(request_id),
            gents::SIGNED_REQUEST_FIELDS,
        ))
        .await;
    ensure_no_errors(&row, "seeded signed request row").unwrap();
    serde_json::from_value(row.data.unwrap()["AgentRequest"][0].clone()).unwrap()
}
