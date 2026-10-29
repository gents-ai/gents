use std::sync::Arc;

use gents::NodeIdentity as _;
use gents_desktop_core::client::{ClientCore, ClientCoreOptions, DesktopPaths, PeerRecord};
use gents_protocol::process_control::{OperatorProcessControlRequest, ProcessControlOutcome};
use serde_json::json;
use wiremock::{matchers, Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn desktop_process_control_uses_hosted_operator_bearer_and_handles_statuses() {
    let server = MockServer::start().await;
    let redirect_target = MockServer::start().await;
    let directory = tempfile::tempdir().unwrap();
    let paths = DesktopPaths::from_root(directory.path().join("desktop"));
    paths.ensure_root_dirs().await.unwrap();
    let home = directory.path().join("runtime");
    tokio::fs::create_dir_all(&home).await.unwrap();
    let key = home.join("node.key");
    let identity = gents::KeyIdentity::load_or_create(&key, None).unwrap();
    let did = identity.did().to_owned();
    let graphql = format!("{}/prefix/api/v0/graphql", server.uri());
    tokio::fs::write(
        home.join("init.json"),
        json!({
            "node_name": "Local", "node_did": did, "key_path": key
        })
        .to_string(),
    )
    .await
    .unwrap();
    tokio::fs::write(
        home.join("runtime.json"),
        json!({
            "graphql": graphql, "node_name": "Local", "node_did": did, "p2p_transport": "iroh"
        })
        .to_string(),
    )
    .await
    .unwrap();
    let mut peer = PeerRecord::local_standard("Local", "iroh://peer", &did, &graphql);
    peer.local_node_home = Some(home.to_string_lossy().into_owned());
    tokio::fs::write(
        paths.peer_directory_path(),
        json!({"peers": [peer]}).to_string(),
    )
    .await
    .unwrap();
    let core = Arc::new(
        ClientCore::start_with_paths_and_options(paths, ClientCoreOptions::local_only())
            .await
            .unwrap(),
    );
    let command = |id: &str| OperatorProcessControlRequest {
        node_did: did.clone(),
        session_id: "session".into(),
        tool_call_id: id.into(),
        tool_call_doc_id: format!("doc-{id}"),
    };
    for (id, response) in [
        (
            "success",
            ResponseTemplate::new(200).set_body_json(ProcessControlOutcome::Cancelled {
                live_execution_cancelled: true,
            }),
        ),
        (
            "conflict",
            ResponseTemplate::new(409).set_body_json(json!({"error": "owner unavailable"})),
        ),
        (
            "unverified",
            ResponseTemplate::new(409).set_body_json(ProcessControlOutcome::Unverified),
        ),
        (
            "lost",
            ResponseTemplate::new(409).set_body_json(ProcessControlOutcome::Lost),
        ),
        (
            "not-found",
            ResponseTemplate::new(404).set_body_json(ProcessControlOutcome::NotFound),
        ),
        (
            "redirect",
            ResponseTemplate::new(302)
                .insert_header("location", redirect_target.uri())
                .set_body_json(json!({"error": "redirect refused"})),
        ),
    ] {
        Mock::given(matchers::method("POST"))
            .and(matchers::path("/prefix/processes/control"))
            .and(matchers::header_regex("authorization", "^Bearer .+"))
            .and(matchers::body_json(command(id)))
            .respond_with(response)
            .expect(1)
            .mount(&server)
            .await;
    }
    assert_eq!(
        crate::process_control::process_control(&core, &command("success"))
            .await
            .unwrap(),
        ProcessControlOutcome::Cancelled {
            live_execution_cancelled: true
        }
    );
    assert_eq!(
        crate::process_control::process_control(&core, &command("conflict"))
            .await
            .unwrap_err(),
        "owner unavailable"
    );
    for (id, expected) in [
        ("unverified", ProcessControlOutcome::Unverified),
        ("lost", ProcessControlOutcome::Lost),
        ("not-found", ProcessControlOutcome::NotFound),
    ] {
        assert_eq!(
            crate::process_control::process_control(&core, &command(id))
                .await
                .unwrap(),
            expected
        );
    }
    assert_eq!(
        crate::process_control::process_control(&core, &command("redirect"))
            .await
            .unwrap_err(),
        "redirect refused"
    );
    assert!(redirect_target
        .received_requests()
        .await
        .unwrap()
        .is_empty());
    for request in server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.url.path() == "/prefix/processes/control")
    {
        assert!(request.url.query().is_none());
        assert!(request.url.username().is_empty());
        assert!(request.url.password().is_none());
    }
    core.shutdown().await.unwrap();
}
