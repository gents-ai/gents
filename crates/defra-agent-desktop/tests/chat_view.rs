use anyhow::Result;
use chrono::{Duration, Utc};
use defra_agent_desktop::client::{
    ClientCore, ClientCoreOptions, ClientStore, ClientStoreRows, DesktopPaths,
};
use defra_agent_desktop::state::ShellState;
use defra_agent_desktop::views::chat::{
    build_conversation_buckets, build_deployment_entries, markdown_theme_names, prepare_state,
    send_disabled, turn_state_label,
};
use defra_agent_protocol::client_protocol::ClientTurnState;
use defra_agent_protocol::row::{AgentConversationRow, AgentPrincipalRow};
use syntect::highlighting::ThemeSet;

#[test]
fn conversation_grouping_splits_today_yesterday_and_earlier() {
    let now = Utc::now();
    let today = now.to_rfc3339();
    let yesterday = (now - Duration::days(1)).to_rfc3339();
    let earlier = (now - Duration::days(3)).to_rfc3339();
    let rows = vec![
        AgentConversationRow {
            session_id: "session-today".to_string(),
            agent_name: None,
            agent_did: Some("did:defra:amy".to_string()),
            behavior_id: None,
            title: Some("Today".to_string()),
            preview_text: None,
            status: None,
            created_at: Some(today.clone()),
            updated_at: Some(today),
            latest_request_id: None,
        },
        AgentConversationRow {
            session_id: "session-yesterday".to_string(),
            agent_name: None,
            agent_did: Some("did:defra:amy".to_string()),
            behavior_id: None,
            title: Some("Yesterday".to_string()),
            preview_text: None,
            status: None,
            created_at: Some(yesterday.clone()),
            updated_at: Some(yesterday),
            latest_request_id: None,
        },
        AgentConversationRow {
            session_id: "session-earlier".to_string(),
            agent_name: None,
            agent_did: Some("did:defra:amy".to_string()),
            behavior_id: None,
            title: Some("Earlier".to_string()),
            preview_text: None,
            status: None,
            created_at: Some(earlier.clone()),
            updated_at: Some(earlier),
            latest_request_id: None,
        },
    ];
    let refs: Vec<_> = rows.iter().collect();

    let buckets = build_conversation_buckets(&refs, now);

    assert_eq!(buckets.len(), 3);
    assert_eq!(buckets[0].label, "TODAY");
    assert_eq!(buckets[1].label, "YESTERDAY");
    assert_eq!(buckets[2].label, "EARLIER");
}

#[test]
fn conversation_grouping_meta_surfaces_behavior_binding() {
    let now = Utc::now();
    let rows = vec![AgentConversationRow {
        session_id: "session-1".to_string(),
        agent_name: None,
        agent_did: Some("did:defra:amy".to_string()),
        behavior_id: Some("amy-default".to_string()),
        title: Some("Today".to_string()),
        preview_text: None,
        status: None,
        created_at: Some(now.to_rfc3339()),
        updated_at: Some(now.to_rfc3339()),
        latest_request_id: None,
    }];
    let refs: Vec<_> = rows.iter().collect();

    let buckets = build_conversation_buckets(&refs, now);

    assert_eq!(buckets.len(), 1);
    assert_eq!(buckets[0].entries[0].meta, "behavior amy-default");
}

#[test]
fn send_disabled_for_non_terminal_turn_and_empty_inputs() {
    assert!(send_disabled(true, Some("did:defra:amy"), "", None));
    assert!(send_disabled(
        true,
        Some("did:defra:amy"),
        "hello",
        Some(ClientTurnState::Streaming),
    ));
    assert!(!send_disabled(
        true,
        Some("did:defra:amy"),
        "hello",
        Some(ClientTurnState::Completed),
    ));
}

#[test]
fn deployment_tree_carries_peer_and_agent_mapping() {
    let store = ClientStore::from_rows(ClientStoreRows {
        agent_principals: vec![
            AgentPrincipalRow {
                agent_did: "did:defra:zulu".to_string(),
                display_name: Some("Zulu".to_string()),
                default_behavior_id: None,
                enabled: Some(true),
                created_at: None,
                created_by: None,
            },
            AgentPrincipalRow {
                agent_did: "did:defra:alpha".to_string(),
                display_name: Some("Alpha".to_string()),
                default_behavior_id: None,
                enabled: Some(true),
                created_at: None,
                created_by: None,
            },
        ],
        ..ClientStoreRows::default()
    });
    let peer_statuses = vec![
        defra_agent_desktop::client::ClientPeerStatus {
            peer_id: "peer-zulu".to_string(),
            label: "Zulu Bay".to_string(),
            agent_did: "did:defra:zulu".to_string(),
            addr: "endpoint-zulu".to_string(),
            dial_succeeded: true,
            last_error: None,
        },
        defra_agent_desktop::client::ClientPeerStatus {
            peer_id: "peer-alpha".to_string(),
            label: "Alpha Bay".to_string(),
            agent_did: "did:defra:alpha".to_string(),
            addr: "endpoint-alpha".to_string(),
            dial_succeeded: false,
            last_error: Some("dial failed".to_string()),
        },
    ];

    let entries = build_deployment_entries(&peer_statuses, &store);

    assert_eq!(entries.len(), 2);
    assert_eq!(entries[0].peer_id, "peer-alpha");
    assert_eq!(entries[0].agent_label, "Alpha");
    assert_eq!(entries[1].peer_id, "peer-zulu");
    assert_eq!(entries[1].agent_did, "did:defra:zulu");
}

#[test]
fn configured_markdown_themes_exist_in_syntect_defaults() {
    let themes = ThemeSet::load_defaults();
    let (light, dark) = markdown_theme_names();

    assert!(themes.themes.contains_key(light));
    assert!(themes.themes.contains_key(dark));
}

#[test]
fn chat_prepare_state_leaves_selection_repair_to_controller_sync() {
    let store = ClientStore::from_rows(ClientStoreRows {
        agent_principals: vec![AgentPrincipalRow {
            agent_did: "did:defra:amy".to_string(),
            display_name: Some("Amy".to_string()),
            default_behavior_id: Some("amy-default".to_string()),
            enabled: Some(true),
            created_at: None,
            created_by: None,
        }],
        conversations: vec![
            AgentConversationRow {
                session_id: "session-older".to_string(),
                agent_name: Some("Amy".to_string()),
                agent_did: Some("did:defra:amy".to_string()),
                behavior_id: Some("amy-default".to_string()),
                title: Some("Older".to_string()),
                preview_text: Some("older".to_string()),
                status: Some("active".to_string()),
                created_at: Some("2026-04-14T00:00:00Z".to_string()),
                updated_at: Some("2026-04-14T00:01:00Z".to_string()),
                latest_request_id: Some("req-older".to_string()),
            },
            AgentConversationRow {
                session_id: "session-latest".to_string(),
                agent_name: Some("Amy".to_string()),
                agent_did: Some("did:defra:amy".to_string()),
                behavior_id: Some("amy-default".to_string()),
                title: Some("Latest".to_string()),
                preview_text: Some("latest".to_string()),
                status: Some("active".to_string()),
                created_at: Some("2026-04-14T00:00:00Z".to_string()),
                updated_at: Some("2026-04-14T00:05:00Z".to_string()),
                latest_request_id: Some("req-latest".to_string()),
            },
        ],
        ..ClientStoreRows::default()
    });
    let mut state = ShellState::default();
    state.chat.shell.selected_peer_id = Some("peer-missing".to_string());
    state.chat.shell.selected_agent_did = Some("did:defra:missing".to_string());
    state.chat.shell.selected_session_id = Some("session-missing".to_string());

    prepare_state(&mut state, None, Some(&store));

    assert_eq!(
        state.chat.shell.selected_peer_id.as_deref(),
        Some("peer-missing")
    );
    assert_eq!(
        state.chat.shell.selected_agent_did.as_deref(),
        Some("did:defra:missing")
    );
    assert_eq!(
        state.chat.shell.selected_session_id.as_deref(),
        Some("session-missing")
    );
    assert_eq!(state.status.active_agent, "missing");
}

#[test]
fn chat_prepare_state_preserves_selected_pending_session_until_observed() {
    let store = ClientStore::from_rows(ClientStoreRows {
        agent_principals: vec![AgentPrincipalRow {
            agent_did: "did:defra:amy".to_string(),
            display_name: Some("Amy".to_string()),
            default_behavior_id: Some("amy-default".to_string()),
            enabled: Some(true),
            created_at: None,
            created_by: None,
        }],
        ..ClientStoreRows::default()
    });
    let mut state = ShellState::default();
    state.chat.shell.selected_agent_did = Some("did:defra:amy".to_string());
    state.chat.shell.selected_session_id = Some("session-pending".to_string());

    prepare_state(&mut state, None, Some(&store));

    assert_eq!(
        state.chat.shell.selected_agent_did.as_deref(),
        Some("did:defra:amy")
    );
    assert_eq!(
        state.chat.shell.selected_session_id.as_deref(),
        Some("session-pending")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn chat_sidebar_and_turn_projection_stay_coherent_after_submit() -> Result<()> {
    let tempdir = tempfile::tempdir()?;
    let core = ClientCore::start_with_paths_and_options(
        DesktopPaths::from_root(tempdir.path()),
        ClientCoreOptions::local_only(),
    )
    .await?;

    let created = core
        .create_conversation("did:defra:amy", Some("amy-code"))
        .await?;
    core.submit_request(&created.session_id, "did:defra:amy", "hello operator", None)
        .await?;

    let snapshot = core.store().snapshot();
    let conversations = snapshot.conversation_rows("did:defra:amy");
    let buckets = build_conversation_buckets(&conversations, Utc::now());

    assert_eq!(buckets.len(), 1);
    assert_eq!(buckets[0].entries.len(), 1);
    assert_eq!(buckets[0].entries[0].title, "hello operator");
    assert_eq!(
        snapshot.derive_turn(&created.session_id),
        Some(ClientTurnState::WaitingForClaim)
    );
    assert_eq!(
        turn_state_label(snapshot.derive_turn(&created.session_id)),
        "waiting for claim"
    );
    assert_eq!(snapshot.requests_for_session(&created.session_id).len(), 1);
    core.shutdown().await?;
    Ok(())
}
