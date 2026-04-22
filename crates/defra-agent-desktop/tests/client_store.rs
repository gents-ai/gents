use std::time::Duration;

use anyhow::{Context, Result};
use defra_agent_desktop::client::{
    ClientCore, ClientCoreOptions, ClientStore, ClientStoreRows, DesktopPaths,
};
use defra_agent_protocol::client_protocol::ClientTurnState;
use defra_agent_protocol::row::{
    AgentConversationRow, AgentPrincipalRow, AgentRequestRow, AgentResponseRow, AgentRuntimeRow,
};
use tokio::time::{sleep, timeout};

#[test]
fn store_indexes_conversations_and_runtimes() {
    let store = ClientStore::from_rows(ClientStoreRows {
        agent_principals: vec![AgentPrincipalRow {
            agent_did: "did:defra:amy".to_string(),
            display_name: Some("Amy".to_string()),
            default_behavior_id: None,
            enabled: Some(true),
            created_at: None,
            created_by: None,
        }],
        conversations: vec![
            AgentConversationRow {
                session_id: "session-2".to_string(),
                agent_name: None,
                agent_did: Some("did:defra:amy".to_string()),
                behavior_id: None,
                title: Some("Second".to_string()),
                preview_text: None,
                status: None,
                created_at: Some("2026-04-14T00:00:00Z".to_string()),
                updated_at: Some("2026-04-14T00:02:00Z".to_string()),
                latest_request_id: None,
            },
            AgentConversationRow {
                session_id: "session-1".to_string(),
                agent_name: None,
                agent_did: Some("did:defra:amy".to_string()),
                behavior_id: None,
                title: Some("First".to_string()),
                preview_text: None,
                status: None,
                created_at: Some("2026-04-14T00:00:00Z".to_string()),
                updated_at: Some("2026-04-14T00:03:00Z".to_string()),
                latest_request_id: None,
            },
        ],
        runtimes: vec![AgentRuntimeRow {
            agent_did: "did:defra:amy".to_string(),
            process_state: Some("online".to_string()),
            reconcile_phase: None,
            active_generation: None,
            router_generation: None,
            default_behavior_id: None,
            runnable_behavior_count: Some(1),
            unavailable_behavior_count: Some(0),
            last_reconcile_result: None,
            last_reconcile_error: None,
            last_reconcile_completed_at: None,
            updated_at: Some("2026-04-14T00:05:00Z".to_string()),
        }],
        ..ClientStoreRows::default()
    });

    let conversations = store.conversation_rows("did:defra:amy");
    assert_eq!(conversations.len(), 2);
    assert_eq!(conversations[0].session_id, "session-1");
    assert_eq!(
        store
            .latest_runtime("did:defra:amy")
            .and_then(|runtime| runtime.process_state.as_deref()),
        Some("online")
    );
}

#[test]
fn store_derives_turn_from_retry_chain_tip() {
    let store = ClientStore::from_rows(ClientStoreRows {
        requests: vec![
            AgentRequestRow {
                request_id: "req-1".to_string(),
                agent_did: Some("did:defra:amy".to_string()),
                behavior_id: None,
                session_id: Some("session-1".to_string()),
                retry_parent_request: None,
                retry_root_request: None,
                superseded_by_request: Some("req-2".to_string()),
                content: None,
                status: Some("pending".to_string()),
                lifecycle_state: Some("completed".to_string()),
                backend_id: None,
                execution_origin: None,
                failure_reason: None,
                created_at: Some("2026-04-14T00:00:00Z".to_string()),
                claimed_at: None,
                deadline: None,
                retry_count: None,
                max_retries: None,
                caused_by_trigger_id: None,
                caused_by_trigger_kind: None,
            },
            AgentRequestRow {
                request_id: "req-2".to_string(),
                agent_did: Some("did:defra:amy".to_string()),
                behavior_id: None,
                session_id: Some("session-1".to_string()),
                retry_parent_request: Some("req-1".to_string()),
                retry_root_request: Some("req-1".to_string()),
                superseded_by_request: None,
                content: None,
                status: Some("processing".to_string()),
                lifecycle_state: Some("processing".to_string()),
                backend_id: None,
                execution_origin: None,
                failure_reason: None,
                created_at: Some("2026-04-14T00:01:00Z".to_string()),
                claimed_at: None,
                deadline: None,
                retry_count: None,
                max_retries: None,
                caused_by_trigger_id: None,
                caused_by_trigger_kind: None,
            },
        ],
        responses: vec![
            AgentResponseRow {
                response_key: "resp-1".to_string(),
                request_id: Some("req-2".to_string()),
                agent_did: None,
                behavior_id: None,
                session_id: Some("session-1".to_string()),
                content: None,
                reasoning: None,
                status: Some("streaming".to_string()),
                error_message: None,
                token_count: None,
                progress_seq: Some(1),
                materialized_message_sequence: None,
                materialized_at: None,
                created_at: Some("2026-04-14T00:01:01Z".to_string()),
                completed_at: None,
            },
            AgentResponseRow {
                response_key: "resp-2".to_string(),
                request_id: Some("req-2".to_string()),
                agent_did: None,
                behavior_id: None,
                session_id: Some("session-1".to_string()),
                content: None,
                reasoning: None,
                status: Some("completed".to_string()),
                error_message: None,
                token_count: None,
                progress_seq: Some(2),
                materialized_message_sequence: None,
                materialized_at: None,
                created_at: Some("2026-04-14T00:01:02Z".to_string()),
                completed_at: Some("2026-04-14T00:01:03Z".to_string()),
            },
        ],
        ..ClientStoreRows::default()
    });

    assert_eq!(
        store.derive_turn("session-1"),
        Some(ClientTurnState::Completed)
    );
}

#[test]
fn store_derives_turn_from_conversation_latest_request_not_random_request_id_order() {
    let store = ClientStore::from_rows(ClientStoreRows {
        conversations: vec![AgentConversationRow {
            session_id: "session-1".to_string(),
            agent_name: None,
            agent_did: Some("did:defra:amy".to_string()),
            behavior_id: None,
            title: Some("Turn ordering".to_string()),
            preview_text: None,
            status: None,
            created_at: Some("2026-04-14T00:00:00Z".to_string()),
            updated_at: Some("2026-04-14T00:03:00Z".to_string()),
            latest_request_id: Some("req-a-complete".to_string()),
        }],
        requests: vec![
            AgentRequestRow {
                request_id: "req-z-still-processing".to_string(),
                agent_did: Some("did:defra:amy".to_string()),
                behavior_id: None,
                session_id: Some("session-1".to_string()),
                retry_parent_request: None,
                retry_root_request: None,
                superseded_by_request: None,
                content: None,
                status: Some("processing".to_string()),
                lifecycle_state: Some("processing".to_string()),
                backend_id: None,
                execution_origin: None,
                failure_reason: None,
                created_at: Some("2026-04-14T00:01:00Z".to_string()),
                claimed_at: None,
                deadline: None,
                retry_count: None,
                max_retries: None,
                caused_by_trigger_id: None,
                caused_by_trigger_kind: None,
            },
            AgentRequestRow {
                request_id: "req-a-complete".to_string(),
                agent_did: Some("did:defra:amy".to_string()),
                behavior_id: None,
                session_id: Some("session-1".to_string()),
                retry_parent_request: None,
                retry_root_request: None,
                superseded_by_request: None,
                content: None,
                status: Some("completed".to_string()),
                lifecycle_state: Some("completed".to_string()),
                backend_id: None,
                execution_origin: None,
                failure_reason: None,
                created_at: Some("2026-04-14T00:02:00Z".to_string()),
                claimed_at: None,
                deadline: None,
                retry_count: None,
                max_retries: None,
                caused_by_trigger_id: None,
                caused_by_trigger_kind: None,
            },
        ],
        responses: vec![AgentResponseRow {
            response_key: "resp-a-complete".to_string(),
            request_id: Some("req-a-complete".to_string()),
            agent_did: None,
            behavior_id: None,
            session_id: Some("session-1".to_string()),
            content: Some("done".to_string()),
            reasoning: None,
            status: Some("completed".to_string()),
            error_message: None,
            token_count: None,
            progress_seq: Some(1),
            materialized_message_sequence: None,
            materialized_at: None,
            created_at: Some("2026-04-14T00:02:01Z".to_string()),
            completed_at: Some("2026-04-14T00:02:02Z".to_string()),
        }],
        ..ClientStoreRows::default()
    });

    assert_eq!(
        store.derive_turn("session-1"),
        Some(ClientTurnState::Completed)
    );
}

#[test]
fn focused_request_id_defaults_to_none() {
    let (observed_store, _rx) =
        defra_agent_desktop::client::ObservedStore::new(ClientStore::default());
    assert!(observed_store.focused_request_id().is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn observer_loads_initial_snapshot_and_ticks_on_update() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let paths = DesktopPaths::from_root(tempdir.path());
    let core =
        ClientCore::start_with_paths_and_options(paths, ClientCoreOptions::local_only()).await?;
    let store = core.store();
    let mut updates = core.store_updates();

    assert_eq!(store.snapshot().agent_principals.len(), 0);

    let response = core
        .node()
        .execute(
            r#"mutation {
                add_AgentPrincipal(input: {
                    agent_did: "did:defra:test-agent"
                    display_name: "Test Agent"
                    enabled: true
                }) { agent_did }
            }"#,
        )
        .await;
    assert!(
        !response.has_errors(),
        "agent principal mutation should succeed"
    );

    let baseline = *updates.borrow_and_update();
    timeout(Duration::from_secs(5), async {
        loop {
            updates.changed().await.context("watch channel closed")?;
            if *updates.borrow() > baseline {
                return Ok::<(), anyhow::Error>(());
            }
        }
    })
    .await
    .context("timed out waiting for store update")??;

    timeout(Duration::from_secs(5), async {
        loop {
            if store.snapshot().agent_principals.len() == 1 {
                return Ok::<(), anyhow::Error>(());
            }
            sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .context("timed out waiting for refreshed snapshot")??;

    assert_eq!(
        store.snapshot().agent_principals[0].agent_did,
        "did:defra:test-agent"
    );
    core.shutdown().await?;
    Ok(())
}
