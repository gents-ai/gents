use std::collections::BTreeMap;
use std::time::Duration;

use gents_documents::{
    DatastoreToolSurfaceDocument, DatastoreTools, QueryToolDecl, SurfaceToolDecl, Tools,
};
use gents_store::{collection::Collection, identity::NodeIdentity};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::support::agent_requests::submit_user_request;
use crate::support::fixtures::configure_agent_tools_as;
use crate::support::live_inference::{
    bind_target, boot_live_agent, live_target, wait_for_request_terminal,
};
use crate::support::test_db;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn writer_recovers_complete_large_task_contract_through_live_tool_calls() {
    let target = live_target().await;
    let db = test_db("live-field-recovery-writer").await;
    let actors = bind_target(&db, &target).await;
    let access = &actors.fixture.access;
    const SDL: &str = "type ReaderWork { instructions: String owned_files: String }";
    let plan = gents_store::config_client::preview_schema_install(access, SDL)
        .await
        .unwrap();
    gents_store::config_client::apply_schema_install(access, SDL, &plan.artifact_digest)
        .await
        .unwrap();
    let instructions = "é".repeat(2436);
    let owned_files = format!("[\"{}\"]", "x".repeat(2920));
    let doc = gents_store::config_client::create_authored_document(
        access,
        "ReaderWork",
        &json!({"instructions":instructions,"owned_files":owned_files}),
        gents_store::config_client::DocumentSchemaSource::UserAuthored,
        SDL,
    )
    .await
    .unwrap();
    configure_agent_tools_as(
        access, actors.node_did(), actors.agent_did(),
        Some("Follow the requested tool calls. After reading every requested page, acknowledge briefly without quoting the large fields.".into()),
        Tools {
            tools_id: "live-writer-tools".into(), node_did: actors.node_did().into(),
            datastore: Some(DatastoreTools {
                datastore_tool_surface_ids: Some(vec!["live-writer-contract".into()]),
                ..Default::default()
            }), ..Default::default()
        },
        vec![(Collection::DatastoreToolSurface, serde_json::to_value(DatastoreToolSurfaceDocument {
            surface_id: "live-writer-contract".into(), node_did: actors.node_did().into(),
            display_name: None, enabled: true,
            entries: Some(vec![SurfaceToolDecl::Query(QueryToolDecl {
                tool_name: "read_work".into(), collection: "ReaderWork".into(),
                description: "Read the writer contract in bounded UTF-8 pages.".into(),
                fields: vec!["instructions".into(), "owned_files".into()], filter_fields: vec![],
            })]), created_at: None, tags: vec![],
        }).unwrap())],
    ).await;
    let mut calls = vec![json!({})];
    for (field, text) in [
        ("instructions", &instructions),
        ("owned_files", &owned_files),
    ] {
        let hash = format!("sha256:{:x}", Sha256::digest(text.as_bytes()));
        for offset in (0..text.len()).step_by(2000) {
            let mut page = json!({"doc_id":doc,"field":field,"offset_bytes":offset});
            if offset > 0 {
                page["expected_hash"] = json!(hash);
            }
            calls.push(json!({"fields":[field],"field_page":page}));
        }
    }
    let runtime = boot_live_agent(&db, db.node_identity.clone())
        .await
        .unwrap();
    const SESSION: &str = "live-field-recovery";
    const REQUEST: &str = "live-field-recovery-request";
    submit_user_request(actors.node_did(), &actors.fixture, SESSION, REQUEST,
        &format!("Read the complete writer contract. Call read_work with each of these argument objects in order, without omitting any page: {}. Then acknowledge briefly.", serde_json::to_string(&calls).unwrap()),
        Default::default(),
    ).await;
    assert_eq!(
        wait_for_request_terminal(access, REQUEST, Duration::from_secs(120)).await,
        "completed"
    );
    let rows = access.execute("{ AgentToolCall(filter: {request_id: {_eq: \"live-field-recovery-request\"}, tool_name: {_eq: \"read_work\"}, lifecycle_state: {_eq: \"completed\"}}) {_docID} }").await.unwrap();
    let mut pages: BTreeMap<String, BTreeMap<u64, String>> = BTreeMap::new();
    for row in rows["data"]["AgentToolCall"].as_array().unwrap() {
        let result = gents_session::tool_call::query::result::load_tool_call_result(
            access,
            row["_docID"].as_str().unwrap(),
            actors.node_did(),
            SESSION,
            Some(actors.fixture.user.did()),
        )
        .await
        .unwrap();
        let value: Value = serde_json::from_str(
            &gents_session::tool_call::query::result::render_tool_result(&result).unwrap(),
        )
        .unwrap();
        let page = &value["field_page"];
        if let Some(field) = page["field"].as_str() {
            pages.entry(field.into()).or_default().insert(
                page["offset_bytes"].as_u64().unwrap(),
                page["text"].as_str().unwrap().into(),
            );
        }
    }
    for (field, expected) in [("instructions", instructions), ("owned_files", owned_files)] {
        let recovered = pages
            .get(field)
            .unwrap_or_else(|| panic!("no pages delivered for {field}"))
            .values()
            .cloned()
            .collect::<String>();
        assert_eq!(
            recovered, expected,
            "the persisted tool results must contain the complete {field}"
        );
    }
    runtime.shutdown().await;
}
