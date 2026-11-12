//! The live producer guard for background-completion attribution: it drives
//! real background jobs through the runtime and asserts the desktop
//! projection of the rows they write. `background_origin_tests.rs` is the
//! cheap classifier guard over hand-written rows; this module owns the
//! producer-to-projection path, so a writer-side change (a different message
//! key, a lost queue source, a publication written as `RequestExecution`)
//! fails here rather than only in the editor.

use std::sync::Arc;
use std::time::Duration;

use gents::defra_node::EventName;
use gents::store::graphql::escape_graphql_string;
use gents::support::accepted_turn::{
    boot_prepared_accepted_turn, enqueue_managed_accepted_request, prepare_managed_accepted_turn,
    AcceptedTurnSpec,
};
use gents::support::fixtures::configure_agent_tools_as;
use gents::support::streaming_backend::{StreamChunk, StreamPlan, StreamResponse};
use gents::support::{managed_agent_fixture, test_db};
use gents::tools::background::completion::BACKGROUND_COMPLETION_WAKE_PROMPT;
use gents_desktop_core::client::TranscriptWindow;

use super::*;

const BACKEND_ID: &str = "bridge-background-projection-backend";
const MODEL: &str = "test-model";
const SESSION_ID: &str = "bridge-background-projection-session";
const QUOTED_MARKER: &str = "quoted by a person";
/// Long enough for two real processes and their notification writes on a
/// loaded machine; the loop waits on the node's update stream, never a sleep.
const OBSERVE_TIMEOUT: Duration = Duration::from_secs(60);

struct Observed {
    tool_doc_ids: Vec<String>,
    wake_request_doc_ids: Vec<String>,
    node_did: String,
    requester_did: Option<String>,
}

/// Waits on facts no mutation under test can forge: the terminal tool rows,
/// their tool-delivery publications, and a session request nobody submitted.
/// The message key and the wake's queue source are what the regression
/// asserts, so neither may gate the wait.
async fn observe(access: &gents::ConfigAccess, submitted_request_ids: &[&str]) -> Option<Observed> {
    let session = escape_graphql_string(SESSION_ID);
    let response = access
        .execute(&format!(
            r#"{{
                AgentToolCall(filter: {{ session_id: {{ _eq: "{session}" }} }}) {{
                    _docID tool_name lifecycle_state await_mode
                }}
                AgentMessage(filter: {{ session_id: {{ _eq: "{session}" }} }}, order: {{ sequence: ASC }}) {{
                    _docID message_key request_doc_id publication node_did requester_did
                }}
                AgentRequest(filter: {{ session_id: {{ _eq: "{session}" }} }}) {{
                    _docID request_id
                }}
            }}"#
        ))
        .await
        .expect("read background rows");
    let tool_rows = response["data"]["AgentToolCall"].as_array()?;
    let mut completed = None;
    let mut failed = None;
    for row in tool_rows {
        if row["tool_name"] != "bash" || row["await_mode"] != "background" {
            continue;
        }
        match row["lifecycle_state"].as_str() {
            Some("completed") => completed = row["_docID"].as_str().map(str::to_owned),
            Some("failed") => failed = row["_docID"].as_str().map(str::to_owned),
            _ => {}
        }
    }
    let (completed, failed) = (completed?, failed?);
    let messages = response["data"]["AgentMessage"].as_array()?;
    /* the publication, not the message key: the key is what one mutation
    under test breaks, so the wait must not read it */
    let delivered = |tool_doc_id: &str| {
        messages.iter().any(|row| {
            row["publication"]["kind"] == "tool_delivery"
                && row["publication"]["tool_call_doc_id"].as_str() == Some(tool_doc_id)
        })
    };
    if !delivered(&completed) || !delivered(&failed) {
        return None;
    }
    let wake_request_doc_ids = response["data"]["AgentRequest"]
        .as_array()?
        .iter()
        .filter_map(|row| {
            let request_id = row["request_id"].as_str()?;
            (!submitted_request_ids.contains(&request_id))
                .then(|| row["_docID"].as_str().map(str::to_owned))
                .flatten()
        })
        .filter(|doc_id| {
            messages.iter().any(|row| {
                row["message_key"].as_str() == Some(format!("authored:{doc_id}:prompt").as_str())
            })
        })
        .collect::<Vec<_>>();
    let scope = messages.first()?;
    (!wake_request_doc_ids.is_empty()).then_some(Observed {
        tool_doc_ids: vec![completed, failed],
        wake_request_doc_ids,
        node_did: scope["node_did"].as_str()?.to_owned(),
        requester_did: scope["requester_did"].as_str().map(str::to_owned),
    })
}

fn automated_background(
    item: &RenderedTimelineItem,
) -> Option<(&RequestOriginView, Option<&String>)> {
    match item {
        RenderedTimelineItem::AutomatedInput {
            origin: origin @ RequestOriginView::BackgroundCompletion { .. },
            content,
            ..
        } => Some((origin, content.as_ref())),
        _ => None,
    }
}

#[tokio::test]
async fn live_background_completions_project_as_tool_attributed_activity() {
    let db = test_db("bridge-background-projection").await;
    let fixture = managed_agent_fixture(&db).await;
    let agent_did = fixture.agent.did().to_owned();
    let request_id = "bridge-background-projection-request";
    let prompt = "bridge-background-projection-prompt";
    let prepared = prepare_managed_accepted_turn(
        &db,
        &fixture,
        AcceptedTurnSpec {
            backend_id: BACKEND_ID,
            model: MODEL,
            parent_agent_did: &agent_did,
            configured_agent_dids: &[&agent_did],
            request_id,
            session_id: SESSION_ID,
            prompt,
            accepted_chunks: vec![
                StreamChunk::tool_call(
                    "meta-bg-success",
                    "spawn_process",
                    r#"{"tool_name":"bash","args":{"command":"printf","args":["background done"]}}"#,
                ),
                StreamChunk::tool_call(
                    "meta-bg-failure",
                    "spawn_process",
                    r#"{"tool_name":"bash","args":{"command":"sh","args":["-c","exit 3"]}}"#,
                ),
            ],
            /* the two person-authored lookalikes run as ordinary turns, so
            each needs its own scripted reply */
            child_plans: vec![
                StreamPlan::current_authored_user(
                    BACKGROUND_COMPLETION_WAKE_PROMPT,
                    vec![StreamResponse::completes(
                        BACKGROUND_COMPLETION_WAKE_PROMPT,
                        ["person wake answer"],
                    )],
                ),
                StreamPlan::current_authored_user(
                    QUOTED_MARKER,
                    vec![StreamResponse::completes(
                        QUOTED_MARKER,
                        ["person quote answer"],
                    )],
                ),
            ],
            valid_until: None,
            request_hop: None,
            request_setup: None,
        },
    )
    .await;
    configure_agent_tools_as(
        &fixture.access,
        db.node_identity.did(),
        &agent_did,
        None,
        gents::documents::Tools {
            tools_id: format!("{agent_did}:tools"),
            node_did: db.node_identity.did().to_string(),
            host: Some(gents::documents::HostTools {
                bash: Some(gents::documents::BashTools {
                    mode: gents::BashMode::ReadOnly,
                    read_only_commands: Some(vec![
                        "printf".to_string(),
                        "sh".to_string(),
                        "sleep".to_string(),
                    ]),
                    background_enabled: true,
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        },
        Vec::new(),
    )
    .await;
    let identity: Arc<dyn gents::NodeIdentity> = db.node_identity.clone();
    let agent = gents::Gents::from_default_agent_documents(
        db.node.clone(),
        identity,
        gents::DocumentRuntimeOptions {
            backend_admission: Some(db.home.backend_admission()),
            tool_ceiling: gents::ToolCeiling::readonly(),
            ..Default::default()
        },
    )
    .await
    .expect("build background projection runtime");
    let runtime = boot_prepared_accepted_turn(&db, prepared, agent).await;

    /* lookalikes are submitted as ordinary Person requests before the wait so
    their rows share the window under test */
    for (id, text) in [
        ("lookalike-wake", BACKGROUND_COMPLETION_WAKE_PROMPT.to_owned()),
        (
            "lookalike-envelope",
            format!("<tool-completion tool_call_id=\"spawned:quoted\" tool_name=\"bash\" status=\"failed\">\n  <result>{QUOTED_MARKER}</result>\n</tool-completion>"),
        ),
    ] {
        enqueue_managed_accepted_request(&db, &fixture, id, SESSION_ID, &text).await;
    }

    let submitted = [request_id, "lookalike-wake", "lookalike-envelope"];
    let mut updates = db.node.subscribe(&[EventName::Update]);
    let observed = match tokio::time::timeout(OBSERVE_TIMEOUT, async {
        loop {
            if let Some(observed) = observe(&fixture.access, &submitted).await {
                return observed;
            }
            updates
                .recv()
                .await
                .expect("embedded-node update subscription closed");
        }
    })
    .await
    {
        Ok(observed) => observed,
        Err(_) => {
            let session = escape_graphql_string(SESSION_ID);
            let rows = fixture
                .access
                .execute(&format!(
                    r#"{{
                        AgentToolCall(filter: {{ session_id: {{ _eq: "{session}" }} }}) {{ tool_name lifecycle_state await_mode }}
                        AgentMessage(filter: {{ session_id: {{ _eq: "{session}" }} }}) {{ message_key publication }}
                    }}"#
                ))
                .await;
            panic!("background completions were not produced; rows={rows:?}");
        }
    };

    let page = gents_desktop_core::client::load_session_transcript_window_on(
        &fixture.access,
        SESSION_ID,
        Some(&observed.node_did),
        observed.requester_did.as_deref(),
        TranscriptWindow::Tip,
        Some(200),
    )
    .await
    .expect("tip transcript window");
    let tip = render_transcript_rows(&ClientStore::default(), &page, SESSION_ID);
    assert!(
        !tip.is_empty(),
        "the tip window projected no rows: headers={} segments={} tools={} requests={}",
        page.store.transcript_messages.len(),
        page.store.output_segments.len(),
        page.store.tool_calls.len(),
        page.requests.len()
    );

    let notifications = tip
        .iter()
        .filter_map(automated_background)
        .filter(|(origin, _)| {
            matches!(
                origin,
                RequestOriginView::BackgroundCompletion {
                    notification: true,
                    ..
                }
            )
        })
        .collect::<Vec<_>>();
    assert_eq!(
        notifications.len(),
        2,
        "both completion envelopes project as background notifications: {tip:#?}"
    );
    let mut observed_states = notifications
        .iter()
        .map(|(origin, content)| match origin {
            RequestOriginView::BackgroundCompletion {
                tool_call_doc_id,
                tool_name,
                status_kind,
                ..
            } => (
                tool_call_doc_id.clone(),
                tool_name.clone(),
                status_kind.clone(),
                content.map(String::as_str).unwrap_or_default().to_owned(),
            ),
            other => panic!("unexpected origin {other:?}"),
        })
        .collect::<Vec<_>>();
    observed_states.sort_by(|left, right| left.2.cmp(&right.2));
    let expected = [
        (
            Some(observed.tool_doc_ids[1].clone()),
            Some("bash".to_owned()),
            Some("error".to_owned()),
        ),
        (
            Some(observed.tool_doc_ids[0].clone()),
            Some("bash".to_owned()),
            Some("success".to_owned()),
        ),
    ];
    for (observed_state, expected) in observed_states.iter().zip(expected) {
        assert_eq!(
            (
                observed_state.0.clone(),
                observed_state.1.clone(),
                observed_state.2.clone()
            ),
            expected,
            "completion envelope is bound to its owning tool call: {tip:#?}"
        );
        assert!(
            observed_state.3.contains("<tool-completion "),
            "the envelope text is retained verbatim: {observed_state:?}"
        );
    }
    /* the editor must never need the body: its own status attribute and the
    projected state are read from different owners */
    assert!(observed_states
        .iter()
        .any(|state| state.2.as_deref() == Some("error") && state.3.contains("status=\"failed\"")));

    let wake_items = tip
        .iter()
        .filter_map(automated_background)
        .filter(|(origin, _)| {
            matches!(
                origin,
                RequestOriginView::BackgroundCompletion {
                    notification: false,
                    ..
                }
            )
        })
        .count();
    assert!(
        wake_items >= 1,
        "the wake prompt projects as an automated input, never a user message: {tip:#?}"
    );
    assert!(
        !observed.wake_request_doc_ids.is_empty(),
        "a wake request was observed"
    );

    let user_messages = tip
        .iter()
        .filter_map(|item| match item {
            RenderedTimelineItem::UserMessage { content, .. } => content.as_deref(),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert!(
        user_messages.contains(&BACKGROUND_COMPLETION_WAKE_PROMPT),
        "a person's byte-identical wake text stays a user message: {user_messages:#?}"
    );
    assert!(
        user_messages
            .iter()
            .any(|text| text.contains(QUOTED_MARKER)),
        "a person quoting a full envelope stays a user message: {user_messages:#?}"
    );

    let background_tools = tip
        .iter()
        .filter_map(|item| match item {
            RenderedTimelineItem::ToolGroup { tools, .. } => Some(tools),
            _ => None,
        })
        .flatten()
        .filter(|tool| tool.await_mode.as_deref() == Some("background"))
        .map(|tool| (tool.tool_name.clone(), tool.status_kind.clone()))
        .collect::<Vec<_>>();
    assert!(
        background_tools.contains(&("bash".to_owned(), "success".to_owned()))
            && background_tools.contains(&("bash".to_owned(), "error".to_owned())),
        "the originating tool calls still render as tool activity: {background_tools:#?}"
    );

    /* the reload path: the same rows read through the historical window
    query keep their origin, and the attribution never depends on whether the
    owning tool row shares the page */
    let before = gents_desktop_core::client::load_session_transcript_window_on(
        &fixture.access,
        SESSION_ID,
        Some(&observed.node_did),
        observed.requester_did.as_deref(),
        TranscriptWindow::Before(i64::MAX),
        Some(200),
    )
    .await
    .expect("historical transcript window");
    let historical = render_transcript_rows(&ClientStore::default(), &before, SESSION_ID);
    let historical_notifications = historical
        .iter()
        .filter_map(automated_background)
        .filter(|(origin, _)| {
            matches!(
                origin,
                RequestOriginView::BackgroundCompletion {
                    notification: true,
                    ..
                }
            )
        })
        .count();
    assert_eq!(
        historical_notifications, 2,
        "a reloaded historical page keeps the notification origin: {historical:#?}"
    );

    /* the model's transcript is untouched: both runtime messages remain
    reconstructible canonical history */
    let session = escape_graphql_string(SESSION_ID);
    let headers = fixture
        .access
        .execute(&format!(
            r#"{{ AgentMessage(filter: {{ session_id: {{ _eq: "{session}" }} }}, order: {{ sequence: ASC }}) {{ _docID node_did requester_did }} }}"#
        ))
        .await
        .expect("read canonical headers");
    let mut canonical = Vec::new();
    for header in headers["data"]["AgentMessage"]
        .as_array()
        .expect("canonical headers")
    {
        let (_, message) = gents::session::load_canonical_message(
            &fixture.access,
            header["_docID"].as_str().expect("header document"),
            header["node_did"].as_str().expect("header node"),
            header["requester_did"].as_str(),
        )
        .await
        .expect("reconstruct canonical message");
        if let gents_protocol::message::Message::User { content } = message {
            canonical.extend(content.into_iter().filter_map(|item| match item {
                gents_protocol::message::UserContent::Text(text) => Some(text.text),
                _ => None,
            }));
        }
    }
    assert_eq!(
        canonical
            .iter()
            .filter(|text| text.contains("<tool-completion "))
            .count(),
        3,
        "both notifications and the person's quote stay in model context: {canonical:#?}"
    );
    assert!(
        canonical
            .iter()
            .filter(|text| *text == BACKGROUND_COMPLETION_WAKE_PROMPT)
            .count()
            >= 2,
        "the runtime wake and the person's identical text both stay in model context: {canonical:#?}"
    );

    runtime.shutdown().await;
}
