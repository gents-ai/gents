use super::fixtures::{
    collection_values, runtime_test_node, wait_for_active_peer, wait_for_peer_info, wait_for_value,
};
use super::seeding::seed_agent_target_return_artifacts;
use super::*;

#[tokio::test]
async fn embedded_agent_target_host_replays_only_return_projection() {
    let sender_test = runtime_test_node().await;
    let receiver_test = runtime_test_node().await;
    let sender = Arc::clone(&sender_test.node);
    let receiver = Arc::clone(&receiver_test.node);
    let sender_admin = EmbeddedRemoteP2pAdmin::new(Arc::clone(&sender));
    let receiver_admin = EmbeddedRemoteP2pAdmin::new(Arc::clone(&receiver));
    let sender_addresses = wait_for_peer_info(&sender_admin).await;
    let receiver_addresses = wait_for_peer_info(&receiver_admin).await;
    let (match_request_id, match_session_id, match_header_doc_id, match_sequence) =
        seed_agent_target_return_artifacts(&sender, "match", Some("did:key:coord")).await;
    let (unrelated_request_id, _unrelated_session_id, _unrelated_header, _unrelated_sequence) =
        seed_agent_target_return_artifacts(&sender, "unrelated", None).await;
    let (foreign_request_id, _foreign_session_id, _foreign_header, _foreign_sequence) =
        seed_agent_target_return_artifacts(&sender, "foreign", Some("did:key:other")).await;
    let template =
        resolve_template(AGENT_TARGET_HOST_TEMPLATE).expect("agent-target-host template");
    let collections = template
        .collections
        .iter()
        .map(|collection| (*collection).to_string())
        .collect::<Vec<_>>();
    sender_admin
        .connect(&receiver_addresses)
        .await
        .expect("connect sender to receiver");
    wait_for_active_peer(&sender_admin).await;
    wait_for_active_peer(&receiver_admin).await;
    receiver_admin
        .add_replicator(&sender_addresses, &collections, &PairingFilters::default())
        .await
        .expect("authorize only the agent-target return projection");
    let filters = scope_filter(
        &template.scope,
        template.collections,
        "did:key:coord",
        "did:key:host",
    );
    sender_admin
        .add_replicator(&receiver_addresses, &collections, &filters)
        .await
        .expect("install requester-scoped return replicator");
    wait_for_value(&receiver, "AgentRequest", "request_id", &match_request_id).await;
    wait_for_value(&receiver, "AgentMessage", "_docID", &match_header_doc_id).await;
    assert_eq!(
        collection_values(&receiver, "AgentRequest", "request_id").await,
        BTreeSet::from([match_request_id.clone()]),
        "only the requester-routed return request must replicate"
    );
    let sender_terminal = sender . execute (& format ! (r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{}" }} }}, limit: 1) {{ _docID lifecycle_state terminal_output }} }}"# , escape_graphql_string (& match_request_id) ,)) . await ;
    let sender_terminal_row = &sender_terminal.data.as_ref().unwrap()["AgentRequest"][0];
    let match_request_doc_id = sender_terminal_row["_docID"]
        .as_str()
        .expect("sender request doc id")
        .to_string();
    assert_eq!(sender_terminal_row["lifecycle_state"], "completed");
    assert_eq!(
        sender_terminal_row["terminal_output"],
        serde_json::to_value(gents_protocol::output::TerminalOutput::Message {
            message_doc_id: match_header_doc_id.clone(),
        })
        .unwrap()
    );
    let receiver_message_rows = crate::session::canonical_rows::decode_transcript_message_row(
        &receiver
            .execute(&format!(
                r#"{{ AgentMessage(filter: {{ _docID: {{ _eq: "{}" }} }}, limit: 1) {{ {} }} }}"#,
                escape_graphql_string(&match_header_doc_id),
                crate::session::canonical_rows::AGENT_MESSAGE_FIELDS,
            ))
            .await
            .data
            .as_ref()
            .and_then(|data| data.get("AgentMessage"))
            .and_then(|rows| rows.as_array())
            .and_then(|rows| rows.first())
            .cloned()
            .expect("replicated canonical AgentMessage"),
    )
    .expect("decode replicated canonical AgentMessage");
    assert_eq!(
        receiver_message_rows.doc_id, match_header_doc_id,
        "AgentMessage replication must preserve the physical header identity"
    );
    assert_eq!(receiver_message_rows.message.session_id, match_session_id);
    assert_eq!(receiver_message_rows.message.sequence, match_sequence);
    wait_for_value(
        &receiver,
        "AgentOutputSegment",
        "request_doc_id",
        &match_request_doc_id,
    )
    .await;
    assert_eq!(
        collection_values(&receiver, "AgentOutputSegment", "request_doc_id").await,
        BTreeSet::from([match_request_doc_id.clone()]),
        "only the routed return projection's segments must replicate"
    );
    let agent_response_rows = receiver.execute("{ AgentResponse { response_key } }").await;
    assert!(
        agent_response_rows.has_errors(),
        "AgentResponse is a retired collection: receiver queries must reject it"
    );
    let mut projected = None;
    for _ in 0..120 {
        match crate::session::load_canonical_message_from_node(
            &receiver,
            &match_header_doc_id,
            "did:key:host",
            Some("did:key:coord"),
        )
        .await
        {
            Ok((_, message)) => {
                projected = Some(message);
                break;
            }
            Err(error)
                if error
                    .downcast_ref::<gents_protocol::output::ReconstructionError>()
                    .is_some_and(|error| error.is_incomplete()) =>
            {
                tokio::time::sleep(Duration::from_millis(250)).await;
                continue;
            }
            Err(error) => {
                panic!("reconstructing projected child result failed: {error:#}");
            }
        }
    }
    assert_eq!(
        projected.expect("projected child result reconstructed"),
        gents_protocol::message::Message::assistant("child result"),
        "receiver must project the exact child result"
    );
    assert!(
        !collection_values(&receiver, "AgentRequest", "request_id")
            .await
            .contains(&unrelated_request_id),
        "unrouted host-local artifact leaked to the receiver"
    );
    assert!(
        !collection_values(&receiver, "AgentRequest", "request_id")
            .await
            .contains(&foreign_request_id),
        "foreign-requester artifact leaked to the receiver"
    );
    wait_for_value(&receiver, "AgentSession", "session_id", &match_session_id).await;
    assert_eq!(
        collection_values(&receiver, "AgentSession", "session_id").await,
        BTreeSet::from([match_session_id.clone()]),
        "only the requester's caused session crosses the return leg"
    );
    sender.shutdown().await;
    receiver.shutdown().await;
}
