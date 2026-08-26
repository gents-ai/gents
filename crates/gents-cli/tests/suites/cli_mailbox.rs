use crate::support::*;

use std::fs;
use std::time::Duration;

use anyhow::{Context, Result};
use serde_json::Value;
use uuid::Uuid;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mailbox_list_scopes_to_caller_and_dismisses_owned_item() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;
    let model_name = format!("mock-mailbox-cli-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockModelEndpoint::start(&model_name)?;
    let port = allocate_port()?;
    let graphql = graphql_url(port);
    let init = run_init_json(
        &home_dir,
        &[
            "--agent-name",
            "cli-mailbox",
            "--model-name",
            &model_name,
            mock_endpoint.endpoint(),
        ],
    )?;
    let agent_did = agent_did_from_init(&init)?;
    let mut serve = spawn_server(&home_dir, port)?;
    wait_for_port(port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    let mutation = format!(
        r#"mutation {{ create_MailboxItem(input: {{
            item_key: "runtime:request-doc:ask:1", requester_did: "{did}",
            agent_did: "{did}", status: "open", kind: "ask", action: "ack",
            title: "CLI attention", summary: "check from the terminal", payload: null,
            source_kind: "runtime", source_id: "request-doc", session_id: null,
            request_id: null, graph_run_id: null, cause_doc_id: null,
            target_agent_did: "{did}", target_behavior_id: "default",
            expected_collection: null, parent_item_id: null, deadline_at: null,
            created_at: "2026-08-25T12:00:00Z", updated_at: "2026-08-25T12:00:00Z",
            resolved_at: null, resolved_doc_id: null
        }}) {{ _docID }} }}"#,
        did = escape_graphql_string(&agent_did),
    );
    let created = graphql_query(&graphql, &mutation).await?;
    let doc_id = gents_protocol::graphql::extract_mutation_doc_id(&created, "MailboxItem")
        .with_context(|| format!("created mailbox doc id: {created}"))?;

    let listed = run_cli_json(&home_dir, &["mailbox", "list", "--graphql", &graphql])?;
    let rows = listed.as_array().context("mailbox list array")?;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0]["title"], Value::String("CLI attention".into()));
    assert_eq!(rows[0]["requester_did"], Value::String(agent_did));

    let dismissed = run_cli_json(
        &home_dir,
        &["mailbox", "dismiss", &doc_id, "--graphql", &graphql],
    )?;
    assert_eq!(dismissed["status"], "dismissed");
    let open = run_cli_json(&home_dir, &["mailbox", "list", "--graphql", &graphql])?;
    assert_eq!(open.as_array().map(Vec::len), Some(0));

    serve.child.kill().ok();
    Ok(())
}
