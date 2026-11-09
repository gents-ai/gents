use gents::store::graphql::escape_graphql_string;
use gents::write_manual_agent_request;
use serde_json::Value;

use crate::support::{managed_agent, test_db};

fn actor(did: &str) -> identity::Did {
    identity::Did::new(did.to_owned()).expect("fixture creator DID")
}

/// Manual requests are Agent-owned rows; they are read as that Agent.
fn agent_access(
    db: &crate::support::TestDb,
    agent: &dyn gents::NodeIdentity,
) -> gents::ConfigAccess {
    gents::ConfigAccess::local_managed_agent(db.node.clone(), actor(agent.did()), agent)
        .expect("managed Agent access")
}

async fn fetch_manual_row(access: &gents::ConfigAccess, doc_id: &str) -> Value {
    let escaped = escape_graphql_string(doc_id);
    let query = format!(
        r#"{{
            AgentRequest(filter: {{ _docID: {{ _eq: "{escaped}" }} }}, limit: 1) {{
                content
                lifecycle_state
                execution_origin
                caused_by_trigger_id
                caused_by_trigger_kind
            }}
        }}"#
    );
    let resp = access
        .execute(&query)
        .await
        .expect("fetch_manual_row query");
    resp.get("data")
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|v| v.as_array())
        .and_then(|rows| rows.first())
        .cloned()
        .expect("exactly one AgentRequest for the given doc_id")
}

async fn count_manual_agent_requests(access: &gents::ConfigAccess) -> usize {
    let query = r#"{
        AgentRequest(filter: { caused_by_trigger_kind: { _eq: "manual" } }) {
            _docID
        }
    }"#;
    let resp = access
        .execute(query)
        .await
        .expect("count_manual_agent_requests query");
    resp.get("data")
        .and_then(|d| d.get("AgentRequest"))
        .and_then(|v| v.as_array())
        .map(|rows| rows.len())
        .unwrap_or(0)
}

#[tokio::test]
async fn manual_run_materializes_agent_request_with_lineage() {
    let db = test_db("manual-run-lineage").await;
    let agent = managed_agent(&db).await;

    let doc_id = write_manual_agent_request(
        &db.node,
        actor(agent.did()),
        agent.as_ref(),
        gents::request::lifecycle::ManualRequestTarget {
            node_did: db.node_identity.did(),
            agent_did: agent.did(),
            task_id: "task-manual-lineage",
        },
        "manual body",
        serde_json::json!({}),
    )
    .await
    .expect("write_manual_agent_request should succeed on a fresh node");
    assert!(!doc_id.is_empty(), "helper must return a non-empty doc id");

    let row = fetch_manual_row(&agent_access(&db, agent.as_ref()), &doc_id).await;

    assert!(
        row["caused_by_trigger_id"].is_null(),
        "caused_by_trigger_id must be null for manual runs; got {:?}",
        row["caused_by_trigger_id"]
    );
    assert_eq!(
        row["caused_by_trigger_kind"].as_str(),
        Some("manual"),
        "caused_by_trigger_kind must be \"manual\""
    );
    assert_eq!(
        row["execution_origin"].as_str(),
        Some("interactive"),
        "execution_origin must be \"interactive\" for manual runs"
    );
    assert_eq!(
        row["lifecycle_state"].as_str(),
        Some("pending"),
        "manual runs must land at lifecycle_state=pending"
    );
    assert_eq!(
        row["content"].as_str(),
        Some("manual body"),
        "rendered prompt template must land in content"
    );
}

#[tokio::test]
async fn manual_run_renders_args_scope() {
    let db = test_db("manual-run-args-scope").await;
    let agent = managed_agent(&db).await;

    let doc_id = write_manual_agent_request(
        &db.node,
        actor(agent.did()),
        agent.as_ref(),
        gents::request::lifecycle::ManualRequestTarget {
            node_did: db.node_identity.did(),
            agent_did: agent.did(),
            task_id: "task-args",
        },
        "hi {{ args.name }}",
        serde_json::json!({"name": "Amy"}),
    )
    .await
    .expect("write_manual_agent_request should render args.* templates");

    let row = fetch_manual_row(&agent_access(&db, agent.as_ref()), &doc_id).await;
    assert_eq!(
        row["content"].as_str(),
        Some("hi Amy"),
        "args.* substitution must produce the rendered prompt in content"
    );
}

#[tokio::test]
async fn manual_submissions_materialize_distinct_requests() {
    let db = test_db("manual-run-parallel").await;
    let agent = managed_agent(&db).await;

    let first = write_manual_agent_request(
        &db.node,
        actor(agent.did()),
        agent.as_ref(),
        gents::request::lifecycle::ManualRequestTarget {
            node_did: db.node_identity.did(),
            agent_did: agent.did(),
            task_id: "task-parallel",
        },
        "one",
        serde_json::json!({}),
    )
    .await
    .expect("first manual run should materialize");
    assert_eq!(
        count_manual_agent_requests(&agent_access(&db, agent.as_ref())).await,
        1,
        "sanity: one manual AgentRequest after the first call"
    );

    let second = write_manual_agent_request(
        &db.node,
        actor(agent.did()),
        agent.as_ref(),
        gents::request::lifecycle::ManualRequestTarget {
            node_did: db.node_identity.did(),
            agent_did: agent.did(),
            task_id: "task-parallel",
        },
        "two",
        serde_json::json!({}),
    )
    .await
    .expect("second manual submission should materialize");
    assert_ne!(
        first, second,
        "second manual run must produce a fresh doc_id, not alias the first"
    );
    assert_eq!(
        count_manual_agent_requests(&agent_access(&db, agent.as_ref())).await,
        2,
        "two back-to-back manual submissions must yield two AgentRequest rows"
    );

    for (label, doc_id) in [("first", &first), ("second", &second)] {
        let row = fetch_manual_row(&agent_access(&db, agent.as_ref()), doc_id).await;
        assert!(
            row["caused_by_trigger_id"].is_null(),
            "{label}: direct manual submission must not invent a configured trigger id"
        );
        assert_eq!(
            row["caused_by_trigger_kind"].as_str(),
            Some("manual"),
            "{label}: trigger_kind must remain \"manual\" for direct manual submission"
        );
    }
}

#[tokio::test]
async fn manual_run_exposes_manual_event_and_node_scope() {
    let db = test_db("manual-run-event-scope").await;
    let agent = managed_agent(&db).await;

    let doc_id = write_manual_agent_request(
        &db.node,
        actor(agent.did()),
        agent.as_ref(),
        gents::request::lifecycle::ManualRequestTarget {
            node_did: db.node_identity.did(),
            agent_did: agent.did(),
            task_id: "task-event-scope",
        },
        "kind={{ event.trigger_kind }} id={{ event.trigger_id }} fired={{ event.fired_at }} node={{ node.node_did }}",
        serde_json::json!({}),
    )
    .await
    .expect("manual writer must resolve the manual event scope");

    let row = fetch_manual_row(&agent_access(&db, agent.as_ref()), &doc_id).await;
    let content = row["content"]
        .as_str()
        .expect("content must be present")
        .to_string();
    assert!(
        content.starts_with("kind=manual id=none fired="),
        "the manual event scope must expose trigger_kind=manual and a null \
         trigger_id (rendered by minijinja as \"none\"), got: {content:?}"
    );
    assert!(
        content.contains(&format!("node={}", db.node_identity.did())),
        "the node scope must expose the local node DID, got: {content:?}"
    );
}

#[tokio::test]
async fn manual_run_render_failure_materializes_no_request() {
    let db = test_db("manual-run-render-err").await;
    let agent = managed_agent(&db).await;

    let error = write_manual_agent_request(
        &db.node,
        actor(agent.did()),
        agent.as_ref(),
        gents::request::lifecycle::ManualRequestTarget {
            node_did: db.node_identity.did(),
            agent_did: agent.did(),
            task_id: "task-render-err",
        },
        "oops {{ args.missing_field }}",
        serde_json::json!({}),
    )
    .await
    .expect_err("strict-undefined args reference must fail the manual write");
    assert!(
        error.to_string().contains("render"),
        "the failure reason must name the render path: {error:#}"
    );

    assert_eq!(
        count_manual_agent_requests(&agent_access(&db, agent.as_ref())).await,
        0,
        "a failed manual render must not leave a claimable AgentRequest row"
    );

    let doc_id = write_manual_agent_request(
        &db.node,
        actor(agent.did()),
        agent.as_ref(),
        gents::request::lifecycle::ManualRequestTarget {
            node_did: db.node_identity.did(),
            agent_did: agent.did(),
            task_id: "task-render-err",
        },
        "recovered {{ args.field }}",
        serde_json::json!({"field": "value"}),
    )
    .await
    .expect("a well-formed retry must materialize");

    let row = fetch_manual_row(&agent_access(&db, agent.as_ref()), &doc_id).await;
    assert_eq!(
        row["content"].as_str(),
        Some("recovered value"),
        "the retry must render with the supplied args"
    );
    assert_eq!(
        count_manual_agent_requests(&agent_access(&db, agent.as_ref())).await,
        1,
        "exactly the recovered request may exist after the failed attempt"
    );
}
