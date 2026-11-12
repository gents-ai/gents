use std::time::Duration;

use gents_documents::{AgentTools, FileToolMode, FileTools, HostTools, InferenceProfile, Tools};
use gents_store::collection::Collection;
use gents_store::graphql::escape_graphql_string;
use gents_tools::surface::ToolCeiling;
use serde_json::json;

use crate::support::fixtures::{agent_target, configure_agent_tools_as};
use crate::support::live_inference::{
    boot_live_agent_with_ceiling, live_target, wait_for_request_terminal, wait_for_rows,
    LiveAgentSpec, LiveUser,
};
use crate::support::test_db;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn live_child_without_an_execution_limit_completes_a_six_step_read() {
    let target = live_target().await;
    let db = test_db("live-session-default-limit").await;
    let live = LiveUser::install(&db, &target).await;
    let parent = live.agent(&target, LiveAgentSpec {
        display_name: "Read coordinator",
        inference_profile_id: "live-limit-parent",
        system_prompt: "Start the requested worker with agent_new, passing its instructions exactly. Then acknowledge the delegation.",
        description: None,
        default_for_node: true,
    }).await;
    let child = live.agent(&target, LiveAgentSpec {
        display_name: "Read worker",
        inference_profile_id: "live-limit-child",
        system_prompt: "Follow a file chain with read_file. Read one file at a time, then read the next path named in its contents. Never list the directory or guess a path. Complete after the final file.",
        description: None,
        default_for_node: false,
    }).await;
    live.apply(
        "test.live_child_without_execution_limit",
        vec![(
            Collection::InferenceProfile,
            serde_json::to_value(InferenceProfile {
                profile_id: "live-limit-child".into(),
                execution_id: None,
                ..target.profile(&live.node_did)
            })
            .unwrap(),
        )],
    )
    .await;
    let workspace = tempfile::tempdir().unwrap();
    let paths: Vec<String> = std::iter::once("start.txt".to_string())
        .chain((1..6).map(|_| format!("{}.txt", uuid::Uuid::new_v4())))
        .collect();
    let mut markers = Vec::new();
    for (index, path) in paths.iter().enumerate() {
        let marker = format!("step-result-{}", uuid::Uuid::new_v4());
        let instruction = paths.get(index + 1).map_or_else(
            || "This is the final file. Complete the task now.".to_string(),
            |next| format!("Next call read_file with path {next}."),
        );
        tokio::fs::write(
            workspace.path().join(path),
            format!("{marker}\n{instruction}\n"),
        )
        .await
        .unwrap();
        markers.push(marker);
    }
    configure_agent_tools_as(
        &live.access,
        &live.node_did,
        &child,
        None,
        Tools {
            tools_id: "live-limit-child-tools".into(),
            node_did: live.node_did.clone(),
            host: Some(HostTools {
                root: Some(workspace.path().display().to_string()),
                files: Some(FileTools {
                    mode: FileToolMode::ReadOnly,
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        },
        Vec::new(),
    )
    .await;
    configure_agent_tools_as(
        &live.access,
        &live.node_did,
        &parent,
        None,
        Tools {
            tools_id: "live-limit-parent-tools".into(),
            node_did: live.node_did.clone(),
            agents: Some(AgentTools {
                enabled: Some(true),
                target_ids: vec!["chain-worker".into()],
                ..Default::default()
            }),
            ..Default::default()
        },
        vec![(
            Collection::AgentTarget,
            serde_json::to_value(agent_target(
                &live.node_did,
                "chain-worker",
                &live.node_did,
                &child,
            ))
            .unwrap(),
        )],
    )
    .await;
    let runtime = boot_live_agent_with_ceiling(
        &db,
        db.node_identity.clone(),
        ToolCeiling::readonly_at(workspace.path()),
    )
    .await
    .unwrap();
    const PARENT_REQUEST: &str = "live-six-step-parent";
    let worker_prompt = "Read start.txt with read_file, then follow the next filename given inside each file, one read at a time. Complete only after reading the final file. Do not list directories.";
    live.request(&parent, PARENT_REQUEST, "live-six-step-parent-session", &format!(
        "Call agent_new with these arguments, preserving the prompt: {}. Then acknowledge the delegation.",
        json!({"agent":"chain-worker", "prompt":worker_prompt}),
    )).await;
    assert_eq!(
        wait_for_request_terminal(&live, PARENT_REQUEST, Duration::from_secs(120)).await,
        "completed"
    );
    let query = format!(
        r#"{{ AgentRequest(filter: {{ caused_by_parent_request_id: {{ _eq: "{}" }}, agent_did: {{ _eq: "{}" }} }}) {{ request_id session_id lifecycle_state requester_did admission_signer_did }} }}"#,
        escape_graphql_string(PARENT_REQUEST),
        escape_graphql_string(&child),
    );
    let rows = wait_for_rows(
        &live,
        "AgentRequest",
        &query,
        Duration::from_secs(120),
        |data| {
            data["AgentRequest"]
                .as_array()
                .is_some_and(|rows| rows.iter().any(|row| row["lifecycle_state"] == "completed"))
        },
    )
    .await;
    let request = rows["AgentRequest"]
        .as_array()
        .unwrap()
        .iter()
        .find(|row| row["lifecycle_state"] == "completed")
        .unwrap();
    assert_eq!(request["requester_did"], parent);
    assert_eq!(request["admission_signer_did"], parent);
    let query = format!(
        r#"{{ AgentToolCall(filter: {{ request_id: {{ _eq: "{}" }}, tool_name: {{ _eq: "read_file" }}, lifecycle_state: {{ _eq: "completed" }} }}) {{ _docID }} }}"#,
        escape_graphql_string(request["request_id"].as_str().unwrap()),
    );
    let calls = live.access.execute(&query).await.unwrap();
    let mut results = Vec::new();
    for call in calls["data"]["AgentToolCall"].as_array().unwrap() {
        let result = gents_session::tool_call::query::result::load_tool_call_result(
            &live.access,
            call["_docID"].as_str().unwrap(),
            &live.node_did,
            request["session_id"].as_str().unwrap(),
            Some(&parent),
        )
        .await
        .unwrap();
        results.push(gents_session::tool_call::query::result::render_tool_result(&result).unwrap());
    }
    for marker in markers {
        assert!(
            results.iter().any(|result| result.contains(&marker)),
            "child completed before reading {marker}"
        );
    }
    runtime.shutdown().await;
}
