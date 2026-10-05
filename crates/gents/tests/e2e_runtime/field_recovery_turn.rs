use crate::support::{
    accepted_turn::{boot_prepared_accepted_turn, prepare_accepted_turn, AcceptedTurnSpec},
    fixtures::configure_behavior_tools,
    live_inference::wait_for_request_terminal,
    streaming_backend::StreamChunk,
    test_db,
};
use gents::document_config::{DatastoreTools, QueryToolDecl, SurfaceToolDecl, Tools};
use gents::{AgentIdentity, Collection, DatastoreToolSurfaceDocument};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{sync::Arc, time::Duration};

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn writer_recovers_complete_large_task_contract_in_provider_input() {
    let db = test_db("field-recovery-writer").await;
    let did = db.node_identity.did().to_owned();
    let access = gents::config_client::ConfigAccess::Local(db.node.clone());
    access
        .add_schema("type ReaderWork { instructions: String owned_files: String }")
        .await
        .unwrap();
    let instructions = "é".repeat(2436);
    let owned_files = format!("[\"{}\"]", "x".repeat(2920));
    let mutation=format!("mutation {{add_ReaderWork(input: {{instructions: \"{}\", owned_files: \"{}\"}}) {{_docID}}}}", gents::graphql::escape_graphql_string(&instructions),gents::graphql::escape_graphql_string(&owned_files));
    let response = access
        .write("test.field_recovery.work", &mutation)
        .await
        .unwrap();
    let doc = response["data"]["add_ReaderWork"][0]["_docID"]
        .as_str()
        .unwrap();
    let mut chunks = vec![StreamChunk::tool_call(
        "contract-summary",
        "read_work",
        json!({}).to_string(),
    )];
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
            chunks.push(StreamChunk::tool_call(
                format!("{field}-{offset}"),
                "read_work",
                json!({"fields":[field],"field_page":page}).to_string(),
            ));
        }
    }
    let behavior = "contract-writer";
    let prepared = prepare_accepted_turn(
        &db,
        AcceptedTurnSpec {
            backend_id: "field-recovery",
            model: "field-recovery-model",
            parent_behavior_id: behavior,
            configured_behavior_ids: &[behavior],
            request_id: "field-recovery-request",
            session_id: "field-recovery-session",
            prompt: "Read the complete writer task contract.",
            accepted_chunks: chunks,
            child_plans: vec![],
            valid_until: None,
            subagent_depth: None,
            request_setup: None,
        },
    )
    .await;
    configure_behavior_tools(
        &db.node,
        &did,
        behavior,
        None,
        Tools {
            tools_id: "writer-tools".into(),
            agent_did: did.clone(),
            datastore: Some(DatastoreTools {
                datastore_tool_surface_ids: Some(vec!["writer-contract".into()]),
                ..Default::default()
            }),
            ..Default::default()
        },
        vec![(
            Collection::DatastoreToolSurface,
            serde_json::to_value(DatastoreToolSurfaceDocument {
                surface_id: "writer-contract".into(),
                agent_did: did.clone(),
                display_name: None,
                enabled: true,
                entries: Some(vec![SurfaceToolDecl::Query(QueryToolDecl {
                    tool_name: "read_work".into(),
                    collection: "ReaderWork".into(),
                    description: "Read the exact writer contract.".into(),
                    fields: vec!["instructions".into(), "owned_files".into()],
                    filter_fields: vec![],
                })]),
                created_at: None,
                tags: vec![],
            })
            .unwrap(),
        )],
    )
    .await;
    let identity: Arc<dyn AgentIdentity> = db.node_identity.clone();
    let agent = gents::Gents::from_default_behavior_documents(
        db.node.clone(),
        identity,
        gents::DocumentRuntimeOptions::default(),
    )
    .await
    .unwrap();
    let runtime = boot_prepared_accepted_turn(&db, prepared, agent).await;
    assert_eq!(
        wait_for_request_terminal(&db.node, "field-recovery-request", Duration::from_secs(60))
            .await,
        "completed"
    );
    let bodies = runtime.backend.observed_completion_bodies();
    let body = bodies
        .iter()
        .rev()
        .find(|body| {
            body["messages"]
                .as_array()
                .is_some_and(|messages| messages.iter().any(|message| message["role"] == "tool"))
        })
        .expect("provider continuation received tool results");
    for (field, expected) in [("instructions", instructions), ("owned_files", owned_files)] {
        let mut pages = Vec::new();
        for message in body["messages"].as_array().unwrap() {
            if message["role"] != "tool" {
                continue;
            }
            let Some(content) = message["content"].as_str() else {
                continue;
            };
            let Ok(result) = serde_json::from_str::<Value>(content) else {
                continue;
            };
            if result["field_page"]["field"] == field {
                pages.push(result["field_page"].clone());
            }
        }
        pages.sort_by_key(|page| page["offset_bytes"].as_u64().unwrap());
        let recovered = pages
            .iter()
            .map(|page| page["text"].as_str().unwrap())
            .collect::<String>();
        assert_eq!(recovered, expected, "{field}: {body:#}");
    }
    runtime.shutdown().await;
}
