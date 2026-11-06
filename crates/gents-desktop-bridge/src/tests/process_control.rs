use std::sync::Arc;

use gents::NodeIdentity as _;
use gents_desktop_core::client::{ClientCore, ClientCoreOptions, DesktopPaths, PeerRecord};
use gents_protocol::process_control::{
    OperatorProcessControlRequest, ProcessControlOutcome, ToolControlIntent, ToolControlReceipt,
    ToolControlReceiptOutcome,
};
use serde_json::json;
use wiremock::{matchers, Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn desktop_process_control_authenticates_client_and_requires_agent_receipt() {
    let server = MockServer::start().await;
    // Pooled servers reuse addresses across tests, so a stray request meant for
    // another test's recycled server would be indistinguishable from a followed
    // redirect.
    let redirect_target = MockServer::builder().start().await;
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
    let agent =
        gents::KeyIdentity::load_or_create(directory.path().join("agent.key"), None).unwrap();
    let client_did = core.node_identity().did().to_owned();
    assert_ne!(client_did, did);
    for (id, outcome) in [
        (
            "success",
            ProcessControlOutcome::Cancelled {
                live_execution_cancelled: true,
            },
        ),
        (
            "forged",
            ProcessControlOutcome::Cancelled {
                live_execution_cancelled: true,
            },
        ),
        ("unverified", ProcessControlOutcome::Unverified),
        ("lost", ProcessControlOutcome::Lost),
        ("not-found", ProcessControlOutcome::NotFound),
    ] {
        let intent = format!("intent-{id}");
        let mut receipt = ToolControlReceipt {
            version: 1,
            intent_doc_id: intent.clone(),
            client_did: client_did.clone(),
            agent_did: agent.did().into(),
            node_did: did.clone(),
            session_id: "session".into(),
            session_doc_id: "session-doc".into(),
            request_doc_id: "request-doc".into(),
            tool_call_id: id.into(),
            tool_call_doc_id: format!("doc-{id}"),
            action: "process_stop".into(),
            outcome: ToolControlReceiptOutcome::Process(outcome),
            signature: Vec::new(),
        };
        receipt.signature = agent
            .sign(&receipt.signing_payload().unwrap())
            .await
            .unwrap();
        if id == "forged" {
            receipt.outcome = ToolControlReceiptOutcome::Process(ProcessControlOutcome::Lost);
        }
        let invocation = json!({
            "_docID": format!("doc-{id}"), "node_did": did,
            "agent_did": agent.did(), "session_id": "session",
            "session_doc_id": "session-doc", "request_doc_id": "request-doc",
            "tool_call_id": id
        });
        let mut intent_row = invocation.clone();
        intent_row["_docID"] = intent.clone().into();
        intent_row["client_did"] = client_did.clone().into();
        intent_row["tool_call_doc_id"] = format!("doc-{id}").into();
        intent_row["action"] = "process_stop".into();
        intent_row["outcome_json"] = serde_json::to_string(&receipt).unwrap().into();
        Mock::given(matchers::method("POST"))
            .and(matchers::path("/prefix/api/v0/graphql"))
            .and(matchers::header("accept", "text/event-stream"))
            .and(matchers::body_string_contains(&intent))
            .respond_with(ResponseTemplate::new(200).set_body_raw("", "text/event-stream"))
            .with_priority(1)
            .expect(2)
            .mount(&server)
            .await;
        Mock::given(matchers::method("POST"))
            .and(matchers::path("/prefix/api/v0/graphql"))
            .and(matchers::header_regex("authorization", "^Bearer .+"))
            .and(matchers::body_string_contains(&intent))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "data": {"AgentToolControl": [intent_row], "AgentToolCall": [invocation], "AgentToolControlClaim": []}
            })))
            .with_priority(1)
            .expect(1)
            .mount(&server)
            .await;
    }
    for id in [
        "success",
        "forged",
        "unverified",
        "lost",
        "not-found",
        "unsigned",
        "conflict",
        "redirect",
    ] {
        Mock::given(matchers::method("POST"))
            .and(matchers::path("/prefix/api/v0/graphql"))
            .and(matchers::body_string_contains(format!("doc-{id}")))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({"data": {"AgentToolCall": [{
                "_docID": format!("doc-{id}"), "node_did": did, "agent_did": agent.did(),
                "session_id": "session", "session_doc_id": "session-doc", "request_doc_id": "request-doc",
                "requester_did": client_did, "tool_call_id": id
            }]}})))
            .expect(1).mount(&server).await;
    }
    for (id, response) in [
        (
            "success",
            ResponseTemplate::new(202).set_body_json(ProcessControlOutcome::Queued {
                intent_doc_id: "intent-success".into(),
            }),
        ),
        (
            "forged",
            ResponseTemplate::new(202).set_body_json(ProcessControlOutcome::Queued {
                intent_doc_id: "intent-forged".into(),
            }),
        ),
        (
            "unverified",
            ResponseTemplate::new(202).set_body_json(ProcessControlOutcome::Queued {
                intent_doc_id: "intent-unverified".into(),
            }),
        ),
        (
            "lost",
            ResponseTemplate::new(202).set_body_json(ProcessControlOutcome::Queued {
                intent_doc_id: "intent-lost".into(),
            }),
        ),
        (
            "not-found",
            ResponseTemplate::new(202).set_body_json(ProcessControlOutcome::Queued {
                intent_doc_id: "intent-not-found".into(),
            }),
        ),
        (
            "unsigned",
            ResponseTemplate::new(200).set_body_json(ProcessControlOutcome::Cancelled {
                live_execution_cancelled: true,
            }),
        ),
        (
            "conflict",
            ResponseTemplate::new(409).set_body_json(json!({"error": "owner unavailable"})),
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
            .and(matchers::body_partial_json(
                json!({"tool_call_doc_id": format!("doc-{id}")}),
            ))
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
    let forged_error = crate::process_control::process_control(&core, &command("forged"))
        .await
        .unwrap_err()
        .message;
    assert!(
        forged_error.contains("signature verification failed"),
        "{forged_error}"
    );
    assert!(
        forged_error.contains("the action may still take effect"),
        "{forged_error}"
    );
    let unsigned_error = crate::process_control::process_control(&core, &command("unsigned"))
        .await
        .unwrap_err()
        .message;
    assert!(unsigned_error.contains("without a queued intent"), "{unsigned_error}");
    assert!(unsigned_error.contains("the action may still take effect"));
    for (id, detail) in [("conflict", "owner unavailable"), ("redirect", "redirect refused")] {
        let error = crate::process_control::process_control(&core, &command(id))
            .await
            .unwrap_err();
        assert!(error.message.contains("refused"), "{}", error.message);
        assert!(error.message.ends_with(detail), "{}", error.message);
    }
    let followed = redirect_target.received_requests().await.unwrap();
    assert!(
        followed.is_empty(),
        "process control followed a redirect: {:?}",
        followed
            .iter()
            .map(|request| format!("{} {}", request.method, request.url))
            .collect::<Vec<_>>()
    );
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
        use identity::Identity as _;
        let authorization = request
            .headers
            .get("authorization")
            .unwrap()
            .to_str()
            .unwrap();
        let token = authorization.strip_prefix("Bearer ").unwrap();
        let signer = identity::from_token(token.as_bytes()).unwrap();
        assert_eq!(signer.did().unwrap().as_str(), client_did);
        let intent: ToolControlIntent = serde_json::from_slice(&request.body).unwrap();
        intent.validate_shape().unwrap();
        assert_eq!(intent.client_did, client_did);
        assert_eq!(intent.node_did, did);
        assert_eq!(intent.agent_did, agent.did());
        assert_eq!(intent.session_id, "session");
        assert_eq!(intent.session_doc_id, "session-doc");
        assert_eq!(intent.request_doc_id, "request-doc");
        assert_eq!(intent.requester_did.as_deref(), Some(client_did.as_str()));
        assert_eq!(
            intent.tool_call_doc_id,
            format!("doc-{}", intent.tool_call_id)
        );
        assert_eq!(intent.action, "process_stop");
        if intent.tool_call_id == "unsigned" {
            assert!(unsigned_error.contains(&intent.intent_id));
        }
        assert!(core
            .node_identity()
            .verify(
                &client_did,
                &intent.signing_payload().unwrap(),
                &intent.signature,
            )
            .await
            .unwrap());
    }
    core.shutdown().await.unwrap();
}
