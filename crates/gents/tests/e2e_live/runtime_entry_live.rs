use std::time::Duration;

use gents::DocumentRuntimeOptions;
use gents_protocol::output::TerminalOutput;
use gents_store::collection::Collection;
use gents_store::config_client::ConfigAccess;
use gents_store::graphql::escape_graphql_string;
use serde_json::json;

use crate::support::live_inference::{
    bind_target, boot_live_agent, live_target, wait_for_request_terminal, wait_for_rows, LiveUser,
};
use crate::support::test_db;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admitted_live_oneshot_completes_with_its_selected_terminal_message() {
    let target = live_target().await;
    let db = test_db("live-oneshot-admitted").await;
    let actors = bind_target(&db, &target).await;
    gents_inference::backends::registry::set_backend_probe_status_as(
        &actors.fixture.access,
        actors.node_did(),
        target.backend_id(),
        "healthy",
    )
    .await
    .unwrap();
    let configured = gents::Gents::from_default_agent_documents(
        db.node.clone(),
        db.node_identity.clone(),
        DocumentRuntimeOptions {
            backend_admission: Some(db.home.backend_admission()),
            tool_ceiling: gents_tools::surface::ToolCeiling::meta_only(),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let agent = configured
        .agents()
        .iter()
        .find(|agent| agent.agent.agent_did == actors.agent_did())
        .unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(120),
        gents::run_openai_oneshot_admitted(
            db.node.clone(),
            agent,
            "Acknowledge this one-shot request in a short sentence. Do not call tools.",
            &db.home.backend_admission(),
        ),
    )
    .await
    .expect("live one-shot deadline")
    .unwrap();
    assert!(!result.response_text.trim().is_empty());
    let projection = actors.fixture.access.execute(&format!(
        r#"{{ AgentRequest(filter: {{session_id: {{_eq: "{0}"}}}}) {{_docID node_did requester_did lifecycle_state terminal_output}} AgentSession(filter: {{session_id: {{_eq: "{0}"}}}}) {{observation}} }}"#,
        escape_graphql_string(&result.session_id),
    )).await.unwrap();
    let request = projection["data"]["AgentRequest"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["lifecycle_state"] == "completed")
        .expect("the admitted request completed");
    let terminal: TerminalOutput =
        serde_json::from_value(request["terminal_output"].clone()).unwrap();
    let TerminalOutput::Message { message_doc_id } = terminal else {
        panic!("the completed request selects a terminal message")
    };
    let (header, message) = gents_session::session::load_canonical_message(
        &actors.fixture.access,
        &message_doc_id,
        request["node_did"].as_str().unwrap(),
        request["requester_did"].as_str(),
    )
    .await
    .unwrap();
    assert_eq!(header.request_doc_id.as_deref(), request["_docID"].as_str());
    assert!(matches!(
        &message,
        gents_protocol::message::Message::Assistant { .. }
    ));
    assert!(!gents_protocol::transcript::present_message(&message)
        .body_markdown
        .trim()
        .is_empty());
    assert_eq!(
        projection["data"]["AgentSession"][0]["observation"]["latest_request"]["lifecycle_state"],
        "completed"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scheduled_live_fire_completes_and_persists_trigger_writeback() {
    let target = live_target().await;
    let db = test_db("live-scheduled-fire").await;
    let actors = bind_target(&db, &target).await;
    let live = LiveUser::install(&db, &target).await;
    const TASK: &str = "live-scheduled-task";
    const TRIGGER: &str = "live-scheduled-trigger";
    const PROMPT: &str = "Acknowledge this scheduled task briefly. Do not call tools.";
    live.apply("test.live_schedule.configure", vec![
        (Collection::Task, json!({"task_id":TASK,"node_did":actors.node_did(),"agent_did":actors.agent_did(),"prompt_template":PROMPT})),
        (Collection::Schedule, json!({"schedule_id":TRIGGER,"node_did":actors.node_did(),"cadence":{"kind":"interval","interval_secs":60}})),
        (Collection::Trigger, json!({"trigger_id":TRIGGER,"node_did":actors.node_did(),"task_id":TASK,"source":{"kind":"schedule","schedule_id":TRIGGER},"enabled":true,"concurrency":"serial"})),
    ]).await;
    let runtime = boot_live_agent(&db, db.node_identity.clone())
        .await
        .unwrap();
    let due = (chrono::Utc::now() - chrono::Duration::seconds(2)).to_rfc3339();
    let runtime_access =
        ConfigAccess::local_as(db.node.clone(), db.node_identity.did().parse().unwrap());
    runtime_access.write("test.live_schedule.due", &format!(
        r#"mutation {{update_Trigger(filter: {{node_did: {{_eq: "{}"}}, trigger_id: {{_eq: "{TRIGGER}"}}}}, input: {{next_run_at: "{}"}}) {{_docID}}}}"#,
        escape_graphql_string(actors.node_did()), escape_graphql_string(&due),
    )).await.unwrap();
    let request_query = format!(
        r#"{{AgentRequest(filter: {{caused_by_trigger_id: {{_eq: "{TRIGGER}"}}, caused_by_trigger_kind: {{_eq: "schedule"}}}}) {{request_id agent_did content execution_origin}}}}"#
    );
    let requests = wait_for_rows(
        &actors.fixture.access,
        "AgentRequest",
        &request_query,
        Duration::from_secs(30),
        |data| {
            data["AgentRequest"]
                .as_array()
                .is_some_and(|rows| !rows.is_empty())
        },
    )
    .await;
    let request = &requests["AgentRequest"][0];
    assert_eq!(request["agent_did"], actors.agent_did());
    assert_eq!(request["execution_origin"], "scheduled");
    assert!(request["content"].as_str().unwrap().contains(PROMPT));
    assert_eq!(
        wait_for_request_terminal(
            &actors.fixture.access,
            request["request_id"].as_str().unwrap(),
            Duration::from_secs(120)
        )
        .await,
        "completed"
    );
    let trigger_query = format!(
        r#"{{Trigger(filter: {{trigger_id: {{_eq: "{TRIGGER}"}}}}) {{task_id last_status last_error next_run_at}}}}"#
    );
    let triggers = wait_for_rows(
        &actors.fixture.access,
        "Trigger",
        &trigger_query,
        Duration::from_secs(30),
        |data| {
            data["Trigger"]
                .as_array()
                .is_some_and(|rows| rows.iter().any(|row| row["last_status"] == "fired"))
        },
    )
    .await;
    let trigger = &triggers["Trigger"][0];
    assert_eq!(trigger["task_id"], TASK);
    assert!(trigger["last_error"]
        .as_str()
        .unwrap_or_default()
        .is_empty());
    assert_ne!(trigger["next_run_at"], due);
    runtime.shutdown().await;
}
