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

/// The session snapshot shows a Goal only as the Agent's verified signed
/// observation shows it (Lean `SessionObservation.view`), never from the Goal
/// document the User can also read over the same signed runtime endpoint: no
/// Goal while the projection is missing, then the projected Goal, which a
/// later unprojected Goal change does not replace.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn session_snapshot_goal_is_only_the_verified_observation() {
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
    let snapshot_goal = async || {
        tokio::time::timeout(Duration::from_secs(30), async {
            loop {
                match crate::commands::session_snapshot(
                    &desktop.core,
                    "chat".into(),
                    Some(node_did.clone()),
                    None,
                    Some(40),
                )
                .await
                {
                    Ok(Some(snapshot)) => break snapshot.goal,
                    Ok(None) => {}
                    Err(error)
                        if error.code == crate::error::BridgeErrorCode::ProjectionPending => {}
                    Err(error) => panic!("session snapshot: {error}"),
                }
                tokio::time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("the session snapshot reached its runtime")
    };
    let user_reads_goal_status = async || {
        let access = gents::store::config_client::ConfigAccess::Graphql(
            desktop
                .core
                .operator_observation_graphql(&node_did)
                .unwrap(),
        );
        let rows = access
            .execute(&format!(
                r#"{{ Goal(filter: {{ node_did: {{ _eq: "{}" }}, session_id: {{ _eq: "chat" }} }}) {{ status }} }}"#,
                gents::store::graphql::escape_graphql_string(&node_did)
            ))
            .await
            .unwrap();
        rows["data"]["Goal"].clone()
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
    assert_eq!(
        user_reads_goal_status().await,
        json!([{"status": "active"}])
    );
    let unprojected = snapshot_goal().await;
    assert!(
        unprojected.is_none(),
        "a readable Goal document without a verified projection is not a shown Goal: {unprojected:?}"
    );

    gents::goal::project_session_goal(
        runtime.node(),
        runtime.runtime(),
        &agent,
        "chat",
        Some(&user_did),
    )
    .await
    .unwrap();
    let observed = snapshot_goal().await.expect("the verified Goal");
    assert_eq!(observed.goal_id, goal.goal_id);
    assert_eq!(observed.status.as_deref(), Some("active"));
    assert_eq!(
        observed.objective.as_deref(),
        Some("Finish the hosted work")
    );

    gents::goal::set_goal_from_access(
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
    assert_eq!(
        user_reads_goal_status().await,
        json!([{"status": "paused"}])
    );
    let still_observed = snapshot_goal().await.expect("the verified Goal");
    assert_eq!(
        still_observed.status.as_deref(),
        Some("active"),
        "an unprojected Goal change must not replace the verified observation"
    );

    desktop.shutdown().await;
}

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
