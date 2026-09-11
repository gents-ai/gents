use std::sync::Arc;
use std::time::Duration;

use gents::document_config::{DatastoreTools, SurfaceToolDecl, Tools};
use gents::graphql::escape_graphql_string;
use gents::mailbox::{canonical_mailbox_write_decl, list_mailbox_items, MailboxStatus};
use gents::{AgentIdentity, Collection, DatastoreToolSurfaceDocument};

use super::steward_loop_live::{bind_d4f_backend, boot_d4f_agent, wait_for_request_terminal};
use crate::support::fixtures::{configure_behavior_tools, test_identity};
use crate::support::test_db;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "live: set GENTS_D4F_LIVE=1 and pass --ignored"]
async fn real_model_files_a_stamped_mailbox_item_through_granted_surface() {
    assert!(
        std::env::var("GENTS_D4F_LIVE").as_deref() == Ok("1"),
        "set GENTS_D4F_LIVE=1 and pass --ignored to run mailbox live qualification"
    );

    let db = test_db("mailbox-real-inference").await;
    let identity: Arc<dyn AgentIdentity> = Arc::new(test_identity("mailbox-real-inference"));
    let (agent_did, behavior_id) = bind_d4f_backend(db.node.as_ref(), identity.as_ref()).await;
    let surface_id = "mailbox-live-surface";
    configure_behavior_tools(
        db.node.as_ref(),
        &agent_did,
        &behavior_id,
        None,
        Tools {
            tools_id: "mailbox-live-tools".into(),
            agent_did: agent_did.clone(),
            datastore: Some(DatastoreTools {
                datastore_tool_surface_ids: Some(vec![surface_id.into()]),
                ..Default::default()
            }),
            ..Default::default()
        },
        vec![(
            Collection::DatastoreToolSurface,
            serde_json::to_value(DatastoreToolSurfaceDocument {
                surface_id: surface_id.into(),
                agent_did: agent_did.clone(),
                display_name: Some("Mailbox live surface".into()),
                enabled: true,
                entries: Some(vec![
                    SurfaceToolDecl::Create(canonical_mailbox_write_decl()),
                ]),
                created_at: None,
                tags: Vec::new(),
            })
            .unwrap(),
        )],
    )
    .await;

    db.node
        .add_schema("type MailboxLiveAgentAttention { owner: String @immutable }")
        .await
        .unwrap();
    let source = db
        .node
        .execute(&format!(
            r#"mutation {{ create_MailboxLiveAgentAttention(input: {{ owner: "{}" }}) {{ _docID }} }}"#,
            escape_graphql_string(identity.did())
        ))
        .await;
    assert!(!source.has_errors(), "{:?}", source.errors);
    let source_lookup = db
        .node
        .execute(&format!(
            r#"{{ MailboxLiveAgentAttention(filter: {{ owner: {{ _eq: "{}" }} }}, limit: 1) {{ _docID }} }}"#,
            escape_graphql_string(identity.did())
        ))
        .await;
    assert!(!source_lookup.has_errors(), "{:?}", source_lookup.errors);
    let source_id = source_lookup.data.as_ref().unwrap()["MailboxLiveAgentAttention"]
        .as_array()
        .and_then(|rows| rows.first())
        .and_then(|row| row.get("_docID"))
        .and_then(serde_json::Value::as_str)
        .expect("created mailbox live source id")
        .to_string();

    let _agent = boot_d4f_agent(&db, Arc::clone(&identity))
        .await
        .expect("boot mailbox live agent");
    let request_id = "request-mailbox-live";
    let session_id = "session-mailbox-live";
    let now = chrono::Utc::now().to_rfc3339();
    let prompt = format!(
        "You must call file_mailbox_item exactly once before answering. Use kind=ask, action=ack, title='Mailbox live verified', source_kind=agent, source_id='{source_id}'. Then answer MAILBOX_FILED."
    );
    let mutation = format!(
        r#"mutation {{ create_AgentRequest(input: {{
            request_id: "{request_id}", agent_did: "{agent_did}",
            requester_did: "{requester}", behavior_id: "{behavior_id}",
            session_id: "{session_id}", content: "{content}",
            lifecycle_state: "pending", execution_origin: "interactive",
            created_at: "{now}", retry_count: 0, max_retries: 2
        }}) {{ _docID }} }}"#,
        requester = escape_graphql_string(identity.did()),
        content = escape_graphql_string(&prompt),
    );
    let response = db.node.execute(&mutation).await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    assert_eq!(
        wait_for_request_terminal(db.node.as_ref(), request_id, Duration::from_secs(120)).await,
        "completed"
    );

    let items = list_mailbox_items(db.node.as_ref(), identity.did(), Some(MailboxStatus::Open))
        .await
        .unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].title, "Mailbox live verified");
    assert_eq!(items[0].requester_did, identity.did());
    assert_eq!(items[0].agent_did, agent_did);
    assert_eq!(items[0].target_behavior_id, behavior_id);
    assert_eq!(items[0].source_kind, "agent");
    assert_eq!(items[0].source_id, source_id);

    let tool_calls = db.node.execute(&format!(
        r#"{{ AgentToolCall(filter: {{ request_id: {{ _eq: "{request_id}" }}, tool_name: {{ _eq: "file_mailbox_item" }} }}) {{ lifecycle_state }} }}"#
    )).await;
    assert!(!tool_calls.has_errors(), "{:?}", tool_calls.errors);
    assert_eq!(
        tool_calls.data.as_ref().unwrap()["AgentToolCall"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}
