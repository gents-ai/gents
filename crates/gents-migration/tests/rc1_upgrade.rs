use gents_migration::ensure_migrations;
use gents_protocol::schemas;

mod common;
use common::fresh_node;

#[tokio::test]
async fn rc1_upgrade_preserves_rows_field_ids_and_restarts() {
    let node = fresh_node().await;
    let profile = schemas::INFERENCE_PROFILE.replace("    service_tier: String\n", "");
    let tool = schemas::AGENT_TOOL_CALL
        .replace("    job_pin: JSON\n", "")
        .replace("    job_progress: JSON\n", "")
        .replace("    job_step_state: JSON\n", "");
    node.add_schema(&format!("{profile}\n{tool}"))
        .await
        .unwrap();
    let names = [
        schemas::INFERENCE_PROFILE_NAME,
        schemas::AGENT_TOOL_CALL_NAME,
    ];
    let before = names.map(|name| node.get_collection(name).unwrap().unwrap());
    assert_eq!(
        before[0].version_id,
        "bafyreiat2uhx24kbgbm54lz7nt56vtjjsfttkiwop2ntii7qn2xtceehxi"
    );
    assert_eq!(
        before[1].version_id,
        "bafyreiexmbjxfqyg6yhjst6r746spzge27f6mhh2moyrkawbmvtvba4mbi"
    );
    let created = node.execute(r#"mutation {
        create_InferenceProfile(input: {profile_id: "retained", node_did: "owner", context_window: 65536, max_output_tokens: 4096}) {_docID}
        create_AgentToolCall(input: {tool_call_key: "retained", request_id: "request", status: "completed", latency_ms: 37}) {_docID}
    }"#).await;
    assert!(!created.has_errors(), "{created:?}");
    let identities = created.data.unwrap();
    let report = ensure_migrations(node.as_ref()).await.unwrap();
    assert_eq!(report.steps_applied, 2);
    for (name, previous) in names.into_iter().zip(before) {
        let current = node.get_collection(name).unwrap().unwrap();
        for field in previous.fields {
            let retained = current
                .fields
                .iter()
                .find(|value| value.name == field.name)
                .unwrap();
            assert_eq!(retained.id, field.id, "{name}.{} moved", field.name);
        }
    }
    let read = node
        .execute(
            r#"{
        InferenceProfile {_docID profile_id context_window max_output_tokens service_tier}
        AgentToolCall {_docID tool_call_key status latency_ms job_pin job_progress job_step_state}
    }"#,
        )
        .await;
    assert!(!read.has_errors(), "{read:?}");
    let data = read.data.unwrap();
    assert_eq!(
        data["InferenceProfile"][0]["_docID"],
        identities["create_InferenceProfile"]["_docID"]
    );
    assert_eq!(
        data["AgentToolCall"][0]["_docID"],
        identities["create_AgentToolCall"]["_docID"]
    );
    assert_eq!(data["InferenceProfile"][0]["context_window"], 65536);
    assert_eq!(data["InferenceProfile"][0]["max_output_tokens"], 4096);
    assert_eq!(data["AgentToolCall"][0]["latency_ms"], 37);
    assert_eq!(data["AgentToolCall"][0]["status"], "completed");
    let rerun = ensure_migrations(node.as_ref()).await.unwrap();
    assert_eq!(rerun.steps_applied, 0);
    assert_eq!(rerun.steps_already_current, 2);
    node.shutdown().await;
}
