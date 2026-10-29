use gents_protocol::schemas;

mod common;
use common::fresh_node;

#[tokio::test]
async fn probe_request_activity_versions() {
    let fresh = fresh_node().await;
    fresh.add_collection(schemas::AGENT_REQUEST).await.unwrap();
    let fresh_id = fresh.get_collection("AgentRequest").unwrap().unwrap().version_id;

    let old = fresh_node().await;
    let previous = schemas::AGENT_REQUEST.replace("    execution_observation: JSON\n", "");
    old.add_collection(&previous).await.unwrap();
    let old_id = old.get_collection("AgentRequest").unwrap().unwrap().version_id;
    old.patch_collection("AgentRequest", r#"[{"op":"add","path":"/AgentRequest/Fields/-","value":{"Name":"execution_observation","Kind":"JSON"}}]"#).await.unwrap();
    let patched_id = old.get_collection("AgentRequest").unwrap().unwrap().version_id;
    eprintln!("fresh={fresh_id} old={old_id} patched={patched_id}");
    assert_ne!(fresh_id, old_id);
}
