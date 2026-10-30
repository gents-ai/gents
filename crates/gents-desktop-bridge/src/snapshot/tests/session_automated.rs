use super::*;
use crate::types::RequestOriginView;
use gents_protocol::request_lifecycle::RequestLifecycleState;

fn session() -> AgentSession {
    AgentSession {
        session_id: "sess-1".into(),
        node_did: "did:test:amy".into(),
        requester_did: None,
        agent_id: "default".into(),
        created_at: "2026-04-21T12:00:00Z".into(),
        closed_at: None,
        archived_at: None,
        archive_revision: 0,
        stopped_at: None,
        title: None,
        tags: Vec::new(),
        provenance: None,
        observation: None,
    }
}

fn request(id: &str, session_id: &str, second: u32) -> AgentRequestRow {
    AgentRequestRow {
        purpose: Some(gents_protocol::request_admission::RequestPurpose::Normal),
        doc_id: Some(format!("doc-{id}")),
        request_id: id.into(),
        node_did: Some("did:test:amy".into()),
        agent_id: Some("default".into()),
        session_id: Some(session_id.into()),
        content: Some(format!("{id} text")),
        lifecycle_state: Some(RequestLifecycleState::Completed),
        execution_origin: Some("interactive".into()),
        created_at: Some(format!("2026-04-21T12:00:{second:02}Z")),
        ..Default::default()
    }
}

fn input(value: serde_json::Value) -> Option<gents_protocol::request_input::RequestInput> {
    Some(serde_json::from_value(value).expect("request input"))
}

fn automated_store() -> ClientStore {
    let person = request("person", "sess-1", 0);
    let sender = request("sender", "sess-other", 1);
    let message = AgentRequestRow {
        requester_did: Some("did:test:bob".into()),
        caused_by_parent_request_id: Some("sender".into()),
        caused_by_parent_request_doc_id: Some("doc-sender".into()),
        caused_by_parent_tool_call_id: Some("call-1".into()),
        caused_by_parent_tool_call_doc_id: Some("doc-call-1".into()),
        ..request("message", "sess-1", 2)
    };
    let trigger = AgentRequestRow {
        caused_by_trigger_id: Some("nightly-report".into()),
        caused_by_trigger_kind: Some("schedule".into()),
        execution_origin: Some("scheduled".into()),
        ..request("trigger", "sess-1", 3)
    };
    let wake = AgentRequestRow {
        input: input(serde_json::json!({
            "queue": {
                "source": "background_completion",
                "policy": "coalesce",
                "key": "background_completion:sess-1",
                "background_completion_wake_version": 1
            }
        })),
        content: Some(gents::background_completion::BACKGROUND_COMPLETION_WAKE_PROMPT.into()),
        ..request("wake", "sess-1", 4)
    };
    let goal = AgentRequestRow {
        caused_by_trigger_id: Some("goal-1".into()),
        caused_by_trigger_kind: Some("goal".into()),
        input: input(serde_json::json!({
            "queue": { "source": "goal", "policy": "coalesce", "key": "goal:abc" },
            "goal_continuation": { "sequence": 3, "wrapup": false }
        })),
        ..request("goal", "sess-1", 5)
    };
    let mut rows = ClientStoreRows {
        sessions: vec![session()],
        requests: vec![person, sender, message, trigger, wake, goal],
        ..ClientStoreRows::default()
    };
    for (key, request_doc, sequence, role, text) in [
        (
            "authored:doc-person:prompt",
            "doc-person",
            1,
            MessageRole::User,
            "person text",
        ),
        (
            "answer-person",
            "doc-person",
            2,
            MessageRole::Assistant,
            "person answer",
        ),
        (
            "authored:doc-message:prompt",
            "doc-message",
            3,
            MessageRole::User,
            "please review the diff\nit is in src/",
        ),
        (
            "answer-message",
            "doc-message",
            4,
            MessageRole::Assistant,
            "reviewed",
        ),
        (
            "authored:doc-trigger:prompt",
            "doc-trigger",
            5,
            MessageRole::User,
            "write the nightly report",
        ),
        (
            "background-completion-notification:child-1:agent",
            "doc-wake",
            6,
            MessageRole::User,
            "<agent-notification child_request_id=\"child-1\">done</agent-notification>",
        ),
        (
            "authored:doc-wake:prompt",
            "doc-wake",
            7,
            MessageRole::User,
            gents::background_completion::BACKGROUND_COMPLETION_WAKE_PROMPT,
        ),
        (
            "authored:doc-goal:prompt",
            "doc-goal",
            8,
            MessageRole::User,
            "You are running under the durable goal controller",
        ),
        (
            "answer-goal",
            "doc-goal",
            9,
            MessageRole::Assistant,
            "goal answer",
        ),
    ] {
        push_canonical_text_message(
            &mut rows,
            key,
            "sess-1",
            Some(request_doc),
            sequence,
            role,
            text,
        );
    }
    ClientStore::from_rows(rows)
}

#[test]
fn external_inputs_keep_their_sender_and_internal_inputs_remain_inspectable() {
    let snapshot =
        build_session_snapshot_from_store(&automated_store(), "sess-1", None).expect("snapshot");
    let items = snapshot
        .timeline_items
        .iter()
        .map(|item| match item {
            RenderedTimelineItem::UserMessage { content, .. } => {
                format!("user:{}", content.as_deref().unwrap_or_default())
            }
            RenderedTimelineItem::AssistantMessage { content, .. } => {
                format!("assistant:{}", content.as_deref().unwrap_or_default())
            }
            RenderedTimelineItem::AutomatedInput { origin, .. } => match origin {
                RequestOriginView::SessionMessage { .. } => "automated:sessionMessage".into(),
                RequestOriginView::Trigger { .. } => "automated:trigger".into(),
                RequestOriginView::GoalContinuation { .. } => "automated:goal".into(),
                RequestOriginView::BackgroundCompletion => "automated:background".into(),
            },
            other => format!("{other:?}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(
        items,
        [
            "user:person text",
            "assistant:person answer",
            "automated:sessionMessage",
            "assistant:reviewed",
            "automated:trigger",
            "automated:background",
            "automated:background",
            "automated:goal",
            "assistant:goal answer",
        ]
    );

    let automated = snapshot
        .timeline_items
        .iter()
        .filter_map(|item| match item {
            RenderedTimelineItem::AutomatedInput {
                origin, content, ..
            } => Some((origin.clone(), content.clone().unwrap_or_default())),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        automated[0],
        (
            RequestOriginView::SessionMessage {
                display_body: None,
                sender_node_did: Some("did:test:amy".into()),
                sender_session_id: Some("sess-other".into()),
                sender_request_id: Some("sender".into()),
            },
            "please review the diff\nit is in src/".into()
        )
    );
    assert_eq!(
        automated[1].0,
        RequestOriginView::Trigger {
            trigger_id: "nightly-report".into(),
            trigger_kind: Some("schedule".into()),
        }
    );
    assert_eq!(automated.len(), 5);
    assert_eq!(snapshot.messages.len(), 9);
    assert_eq!(
        snapshot
            .messages
            .iter()
            .filter(|message| message.runtime_control && message.role.as_deref() == Some("user"))
            .count(),
        3
    );
}

#[test]
fn a_session_message_from_an_unreplicated_session_names_its_sending_agent() {
    let mut rows = automated_store().to_rows();
    rows.requests.retain(|row| row.request_id != "sender");
    let snapshot = build_session_snapshot_from_store(&ClientStore::from_rows(rows), "sess-1", None)
        .expect("snapshot");
    assert!(snapshot.timeline_items.iter().any(|item| matches!(
        item,
        RenderedTimelineItem::AutomatedInput {
            origin: RequestOriginView::SessionMessage {
                display_body: None,
                sender_node_did,
                sender_session_id: None,
                sender_request_id,
            },
            ..
        } if sender_node_did.as_deref() == Some("did:test:bob")
            && sender_request_id.as_deref() == Some("sender")
    )));
}

#[test]
fn an_unexecuted_trigger_request_is_pending_with_its_origin() {
    let rows = ClientStoreRows {
        sessions: vec![session()],
        requests: vec![AgentRequestRow {
            lifecycle_state: Some(RequestLifecycleState::Pending),
            caused_by_trigger_id: Some("on-push".into()),
            caused_by_trigger_kind: Some("event".into()),
            ..request("trigger", "sess-1", 0)
        }],
        ..ClientStoreRows::default()
    };
    let snapshot = build_session_snapshot_from_store(&ClientStore::from_rows(rows), "sess-1", None)
        .expect("snapshot");
    assert!(matches!(
        snapshot.timeline_items.as_slice(),
        [RenderedTimelineItem::PendingUserTurn {
            origin: Some(RequestOriginView::Trigger { trigger_id, .. }),
            ..
        }] if trigger_id == "on-push"
    ));
}

#[test]
fn automated_inputs_keep_request_identity_including_internal_controls() {
    let snapshot =
        build_session_snapshot_from_store(&automated_store(), "sess-1", None).expect("snapshot");
    let identities = snapshot
        .timeline_items
        .iter()
        .filter_map(|item| match item {
            RenderedTimelineItem::AutomatedInput {
                input_request_id, ..
            } => Some(input_request_id.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        identities,
        [
            Some("message".to_owned()),
            Some("trigger".to_owned()),
            None,
            Some("wake".to_owned()),
            Some("goal".to_owned())
        ]
    );
    let control_identities = snapshot
        .messages
        .iter()
        .filter(|message| message.runtime_control && message.role.as_deref() == Some("user"))
        .map(|message| message.input_request_id.clone())
        .collect::<Vec<_>>();
    assert_eq!(
        control_identities,
        [None, Some("wake".to_owned()), Some("goal".to_owned())]
    );
}

#[test]
fn received_message_body_uses_readable_parent_identity_without_changing_signed_input() {
    let mut rows = automated_store().to_rows();
    let message = rows
        .requests
        .iter_mut()
        .find(|row| row.request_id == "message")
        .unwrap();
    message.requester_did = Some("did:test:amy".into());
    message.content = Some("[message agent:default session:sess-other node:did:test:amy · at:2026-04-21T12:00:02Z · cause:sender]\n\nReview the layout.".into());
    let message = message.clone();
    let store = ClientStore::from_rows(rows);
    let origin = crate::snapshot::session::request_origin_view(&store, &message).unwrap();
    assert!(
        matches!(origin, RequestOriginView::SessionMessage { display_body: Some(body), .. } if body == "Review the layout.")
    );
    assert!(message
        .content
        .as_deref()
        .unwrap()
        .starts_with("[message agent:"));
}

#[test]
fn active_background_request_keeps_interrupt_and_queue_visible_after_public_completion() {
    use gents_protocol::session::{SessionObservation, SessionRequestObservation};
    let person = AgentRequestRow {
        caused_by_trigger_id: Some("nightly".into()),
        caused_by_trigger_kind: Some("schedule".into()),
        ..request("person", "sess-1", 0)
    };
    let wake = AgentRequestRow {
        lifecycle_state: Some(RequestLifecycleState::Processing),
        input: input(serde_json::json!({"queue": {
            "source":"background_completion", "policy":"coalesce",
            "key":"background_completion:sess-1", "background_completion_wake_version":1
        }})),
        content: Some(gents::background_completion::BACKGROUND_COMPLETION_WAKE_PROMPT.into()),
        ..request("wake", "sess-1", 1)
    };
    let queued = AgentRequestRow {
        lifecycle_state: Some(RequestLifecycleState::Pending),
        input: input(serde_json::json!({"queue": {
            "source":"user", "policy":"append", "queued_after_request_id":"wake"
        }})),
        ..request("queued", "sess-1", 2)
    };
    let mut header = session();
    header.observation = Some(SessionObservation {
        reply_preview: None,
        last_activity_at: "2026-04-21T12:00:01Z".into(),
        preview: Some("person text".into()),
        latest_request: Some(SessionRequestObservation {
            request_id: person.request_id.clone(),
            request_doc_id: person.doc_id.clone().unwrap(),
            lifecycle_state: RequestLifecycleState::Completed,
        }),
    });
    let store = ClientStore::from_rows(ClientStoreRows {
        sessions: vec![header.clone()],
        requests: vec![person, wake, queued],
        ..Default::default()
    });
    let snapshot = build_session_snapshot_from_store(&store, "sess-1", Some("person")).unwrap();
    assert_eq!(snapshot.latest_request_id.as_deref(), Some("wake"));
    assert_eq!(snapshot.turn_state.as_deref(), Some("running"));
    assert_eq!(
        snapshot
            .queued_turns
            .iter()
            .map(|row| row.request_id.as_str())
            .collect::<Vec<_>>(),
        vec!["queued"]
    );
    assert!(snapshot.pending_turn.is_none());
    let serialized = serde_json::to_string(&snapshot.timeline_items).unwrap();
    assert!(!serialized.contains(gents::background_completion::BACKGROUND_COMPLETION_WAKE_PROMPT));
    let summaries = session_summaries(&[header], &store.requests, "did:test:amy", &[], &[]);
    assert_eq!(summaries[0].latest_request_id.as_deref(), Some("wake"));
    assert_eq!(summaries[0].turn_state.as_deref(), Some("running"));
    assert_eq!(summaries[0].trigger_id.as_deref(), Some("nightly"));
    assert_eq!(summaries[0].trigger_kind.as_deref(), Some("schedule"));
}

#[test]
fn queued_and_folded_observations_retain_the_active_owners_trigger() {
    use gents_protocol::session::{SessionObservation, SessionRequestObservation};
    for folded in [false, true] {
        let owner = AgentRequestRow {
            lifecycle_state: Some(RequestLifecycleState::Processing),
            caused_by_trigger_id: Some("nightly".into()),
            caused_by_trigger_kind: Some("schedule".into()),
            ..request("owner", "sess-1", 0)
        };
        let mut observed = AgentRequestRow {
            caused_by_trigger_id: Some("earlier-trigger".into()),
            caused_by_trigger_kind: Some("event".into()),
            lifecycle_state: Some(RequestLifecycleState::Pending),
            input: input(serde_json::json!({"queue": {
                "source":"user", "policy":"append", "queued_after_request_id":"owner"
            }})),
            ..request("observed", "sess-1", 1)
        };
        if folded {
            observed.lifecycle_state = Some(RequestLifecycleState::Superseded);
            observed.superseded_by_request = Some("owner".into());
            observed.superseded_by_request_doc_id = Some("doc-owner".into());
            observed.failure_reason = Some(gents::request::lifecycle::FOLDED_REASON.into());
        }
        let mut header = session();
        header.observation = Some(SessionObservation {
            reply_preview: None,
            last_activity_at: "2026-04-21T12:00:01Z".into(),
            preview: None,
            latest_request: Some(SessionRequestObservation {
                request_id: observed.request_id.clone(),
                request_doc_id: observed.doc_id.clone().unwrap(),
                lifecycle_state: observed.lifecycle_state.unwrap(),
            }),
        });
        let summaries = session_summaries(&[header], &[owner, observed], "did:test:amy", &[], &[]);
        assert_eq!(summaries[0].latest_request_id.as_deref(), Some("owner"));
        assert_eq!(summaries[0].trigger_id.as_deref(), Some("nightly"));
        assert_eq!(summaries[0].trigger_kind.as_deref(), Some("schedule"));
    }
}
