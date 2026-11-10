use std::time::Duration;

use bytes::Bytes;
use futures::{SinkExt, StreamExt};
use gents::goal::{GoalStatus, SessionGoalObservationState};
use gents::NodeIdentity as _;
use serde_json::{json, Value};
use tokio_util::codec::LengthDelimitedCodec;
use tokio_util::sync::CancellationToken;

use super::support::served_desktop;
use crate::host::{protocol, ClientHost};

/// The Agent's signed Goal projection reaches the session's User through the
/// editor host wire (`desktop_session_goal`, a User-signed HTTP read of the
/// runtime) as the three-state `SessionGoalState`, through start, pause and
/// resume. A signed actor who is not the session's User reads no session.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn enrolled_user_observes_agent_goal_transitions_over_the_host_wire() {
    let desktop = served_desktop().await;
    let runtime = &desktop.runtime;
    let node_did = runtime.node_did().to_owned();
    let user_did = desktop.core.node_identity().did().to_owned();
    runtime
        .agent_session(&desktop.agent, "chat", &user_did)
        .await;
    let agent_access = runtime.agent_access(&desktop.agent).await;
    let agent =
        gents::store::identity::resolve_agent_identity(runtime.runtime().as_ref(), &desktop.agent)
            .unwrap();
    let project = || {
        gents::goal::project_session_goal(
            runtime.node(),
            runtime.runtime(),
            &agent,
            "chat",
            Some(&user_did),
        )
    };

    let (client, server) = tokio::io::duplex(64 * 1024);
    let stop = CancellationToken::new();
    let host = std::sync::Arc::new(ClientHost::new(desktop.core.clone()));
    let task = tokio::spawn(host.serve(server, stop.clone()));
    let mut client = LengthDelimitedCodec::builder()
        .max_frame_length(protocol::MAX_FRAME_BYTES)
        .new_framed(client);
    client
        .send(Bytes::from(
            serde_json::to_vec(&json!({"kind": "hello", "version": protocol::PROTOCOL_VERSION}))
                .unwrap(),
        ))
        .await
        .unwrap();
    let hello: Value = serde_json::from_slice(&client.next().await.unwrap().unwrap()).unwrap();
    assert_eq!(hello["version"], protocol::PROTOCOL_VERSION);
    let mut id = 0u32;
    let mut read_goal = async || -> Value {
        id += 1;
        client
            .send(Bytes::from(
                serde_json::to_vec(&json!({"kind": "call", "id": id, "command": {
                    "name": "desktop_session_goal",
                    "args": {"nodeDid": node_did, "sessionId": "chat"},
                }}))
                .unwrap(),
            ))
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                let frame = client.next().await.unwrap().unwrap();
                let response: Value = serde_json::from_slice(&frame).unwrap();
                if response["kind"] == "response" {
                    assert_eq!(response["id"], id);
                    assert!(response["error"].is_null(), "{response}");
                    break response["result"].clone();
                }
            }
        })
        .await
        .unwrap()
    };

    let goal = gents::goal::set_goal_from_access(
        &agent_access,
        &node_did,
        "chat",
        Some("Finish the hosted work"),
        None,
        None,
        None,
    )
    .await
    .unwrap();
    project().await.unwrap();
    let assert_observed = |view: &Value, status: &str| {
        assert_eq!(view["kind"], "observed", "{view}");
        assert_eq!(view["next"], "control_goal", "{view}");
        let observation = &view["observation"];
        assert_eq!(observation["nodeDid"], node_did.as_str(), "{view}");
        assert_eq!(observation["sessionId"], "chat", "{view}");
        assert_eq!(
            observation["goal"]["goalId"],
            goal.goal_id.as_str(),
            "{view}"
        );
        assert_eq!(observation["goal"]["status"], status, "{view}");
    };
    assert_observed(&read_goal().await, "active");

    let paused = gents::goal::set_goal_from_access(
        &agent_access,
        &node_did,
        "chat",
        None,
        Some(GoalStatus::Paused),
        None,
        None,
    )
    .await
    .unwrap();
    project().await.unwrap();
    assert_observed(&read_goal().await, "paused");

    assert!(gents::goal::update_goal_fields_if_status_from_access(
        &agent_access,
        &paused,
        GoalStatus::Paused,
        r#"status: "active""#,
    )
    .await
    .unwrap());
    project().await.unwrap();
    assert_observed(&read_goal().await, "active");

    let home = tempfile::tempdir().unwrap();
    let read_as = async |name: &str| {
        let actor =
            gents::KeyIdentity::load_or_create(home.path().join(format!("{name}.key")), None)
                .unwrap();
        let access = gents::store::config_client::ConfigAccess::Graphql(
            gents::store::config_client::GraphqlEndpoint::as_principal(
                runtime.graphql(),
                actor.did(),
            )
            .with_client_signing_capability(actor.client_signing_capability().unwrap())
            .unwrap(),
        );
        gents::goal::load_client_session_goal_observation_state(&access, &actor, &node_did, "chat")
            .await
    };
    // An unenrolled signed actor is refused outright, never shown a state.
    let outsider = read_as("outsider").await;
    assert!(
        outsider.is_err(),
        "an unenrolled actor must not read the runtime: {outsider:?}"
    );
    // Another enrolled User of the runtime holds no relation on this session.
    gents::store::identity::GrantSet::OperatorUser
        .grant(
            &runtime.runtime_access(),
            &::identity::Did::new(
                gents::KeyIdentity::load_or_create(home.path().join("other.key"), None)
                    .unwrap()
                    .did(),
            )
            .unwrap(),
        )
        .await
        .unwrap();
    let other = read_as("other").await;
    assert!(
        matches!(other, Ok(SessionGoalObservationState::NoReadableSession)),
        "another User must not read this session's Goal: {other:?}"
    );

    stop.cancel();
    drop(client);
    task.await.unwrap().unwrap();
    desktop.shutdown().await;
}
