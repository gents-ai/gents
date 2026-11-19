use super::*;
use gents_protocol::serve_lifecycle::ObservedServeLifecycle;
use gents_store::client_journal::paths::DesktopPaths;
use serde_json::json;

fn sample_summary() -> DesktopInitSummary {
    DesktopInitSummary {
        status: "initialized",
        node_home: "/tmp/node".to_string(),
        desktop_home: "/tmp/desktop".to_string(),
        runtime_directory: "/tmp/desktop/connections.json".to_string(),
        label: "Local Node".to_string(),
        node_name: "default".to_string(),
        node_did: "did:test:default".to_string(),
        graphql: "http://127.0.0.1:9191/graphql".to_string(),

        next_steps: vec![
            "Run `gents-desktop` and leave the desktop app open.".to_string(),
            "Wait for the app to show `Connected`.".to_string(),
            "Then submit prompts from Chat, or run `gents` in another terminal.".to_string(),
        ],
    }
}

#[test]
fn init_summary_tells_demo_to_wait_for_the_desktop_connection() {
    let summary = sample_summary();
    assert!(summary
        .next_steps
        .iter()
        .any(|step| step.contains("show `Connected`")));

    let rendered = render_human_summary(&summary);
    assert!(rendered.contains("Runtime connection saved"));
    assert!(rendered.contains("Open the application to request access."));
    assert!(rendered.contains(&summary.graphql));
}

#[test]
fn runtime_status_url_accepts_case_insensitive_http_schemes() {
    for (input, expected) in [
        ("HTTP://Example.COM:9191", "http://example.com:9191/status"),
        (
            "https://example.com/proxy/api/v0/graphql",
            "https://example.com/proxy/status",
        ),
        (
            "hTtPs://Example.COM:9191/api/v0/graphql?query=1#fragment",
            "https://example.com:9191/status",
        ),
        (
            "example.com:9191/prefix?query=1#fragment",
            "http://example.com:9191/prefix/status",
        ),
    ] {
        assert_eq!(runtime_status_url(input).unwrap(), expected, "{input}");
    }
}

#[test]
fn runtime_graphql_url_accepts_case_insensitive_http_schemes() {
    for (input, expected) in [
        (
            "hTtP://Example.COM:9191/api/v0/graphql?query=1#fragment",
            "http://example.com:9191/api/v0/graphql",
        ),
        (
            "HTTPS://Example.COM:9191/api/v0/graphql/",
            "https://example.com:9191/api/v0/graphql",
        ),
        (
            "example.com:9191/prefix/graphql?query=1#fragment",
            "http://example.com:9191/prefix/graphql",
        ),
    ] {
        assert_eq!(runtime_graphql_url(input).unwrap(), expected, "{input}");
    }
    assert!(runtime_graphql_url("HTTPS://example.com:9191/status").is_err());
}

#[tokio::test]
async fn peer_status_fetches_a_mixed_case_http_endpoint() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/status"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({"node_did": "did:key:node"})))
        .expect(1)
        .mount(&server)
        .await;
    let endpoint = server.uri().replacen("http://", "hTtP://", 1);
    let record = gents_protocol::runtime_connection::RuntimeConnection {
        label: ("Node").to_string(),
        node_did: ("did:key:node").to_string(),
        graphql: format!("{endpoint}/api/v0/graphql"),
        local_node_home: None,
        access_request: None,
        access_state: None,
    };
    let status = super::http::peer_status(&record).await.unwrap().unwrap();
    assert_eq!(status["node_did"], "did:key:node");
}

#[test]
fn desktop_graphql_rewrites_loopback_endpoint_for_remote_status_host() {
    assert_eq!(
        graphql_endpoint_for_desktop_access("http://100.73.235.38:9181/status").as_deref(),
        Some("http://100.73.235.38:9181/api/v0/graphql")
    );
}

#[test]
fn canonical_graphql_is_rewritten_in_status_payload() {
    let payload = normalize_runtime_connection_payload(
        serde_json::json!({
            "node_did": "did:key:z6MkNode",
            "graphql": "http://127.0.0.1:9181/api/v0/graphql"
        }),
        "http://100.73.235.38:9181/status",
    );

    assert_eq!(
        payload.get("graphql").and_then(serde_json::Value::as_str),
        Some("http://100.73.235.38:9181/api/v0/graphql")
    );
}

#[test]
fn reset_desktop_runtime_state_removes_node_dir_only() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let paths = DesktopPaths::from_root(tempdir.path());
    std::fs::create_dir_all(paths.node_data_dir()).expect("node dir");
    std::fs::write(paths.node_data_dir().join("store.bin"), "x").expect("node data");
    std::fs::write(paths.runtime_directory_path(), "{}").expect("peer directory");

    let cleared = reset_desktop_runtime_state(&paths).expect("reset desktop runtime state");

    assert!(cleared);
    assert!(!paths.node_data_dir().exists());
    assert!(paths.runtime_directory_path().exists());
}

#[test]
fn dangerously_overwrite_desktop_home_removes_root_dir() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let desktop_root = tempdir.path().join("desktop");
    std::fs::create_dir_all(&desktop_root).expect("desktop root");
    std::fs::write(desktop_root.join("peers.json"), "{}").expect("desktop file");

    dangerously_overwrite_desktop_home(&desktop_root).expect("overwrite desktop home");

    assert!(!desktop_root.exists());
}

#[test]
fn discovery_binds_to_the_live_ready_did() {
    let did = "did:key:z6MkLocal";
    let observe = |status: serde_json::Value| serving_runtime(&status, did);
    assert_eq!(
        observe(json!({ "node_did": did, "lifecycle": "starting" })).unwrap(),
        ObservedServeLifecycle::Starting
    );
    assert_eq!(
        observe(json!({ "node_did": did, "lifecycle": "ready" })).unwrap(),
        ObservedServeLifecycle::Ready
    );
    assert_eq!(
        observe(json!({ "node_did": did, "version": "0.18.2" })).unwrap(),
        ObservedServeLifecycle::Outdated {
            version: Some("0.18.2".to_string())
        }
    );
    for live in [
        json!({ "lifecycle": "ready" }),
        json!({ "node_did": "", "lifecycle": "ready" }),
        json!({ "node_did": "   ", "lifecycle": "ready" }),
        json!({ "node_did": 7, "lifecycle": "ready" }),
        json!({ "node_did": "z6MkLocal", "lifecycle": "ready" }),
        json!({ "node_did": "did:", "lifecycle": "ready" }),
        json!({ "node_did": "did:key:z6MkOther", "lifecycle": "ready" }),
    ] {
        assert!(observe(live.clone()).is_err(), "accepted {live}");
    }
    for blank in ["", "  "] {
        assert!(
            serving_runtime(&json!({ "node_did": blank, "lifecycle": "ready" }), blank).is_err()
        );
    }
}

async fn serve_status_after(
    listener: std::net::TcpListener,
    delay: std::time::Duration,
    body: serde_json::Value,
) -> tokio::task::JoinHandle<()> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let body = body.to_string();
    tokio::spawn(async move {
        tokio::time::sleep(delay).await;
        listener.set_nonblocking(true).unwrap();
        let listener = tokio::net::TcpListener::from_std(listener).unwrap();
        loop {
            let Ok((mut stream, _)) = listener.accept().await else {
                return;
            };
            let mut buffer = [0_u8; 4096];
            let _ = stream.read(&mut buffer).await;
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes()).await;
        }
    })
}

fn seed_home(home: &std::path::Path, did: &str, graphql: &str) {
    std::fs::create_dir_all(home).unwrap();
    std::fs::write(
        home.join("init.json"),
        json!({ "node_name": "Local", "node_did": did }).to_string(),
    )
    .unwrap();
    std::fs::write(
        home.join("runtime.json"),
        json!({
            "graphql": graphql,
            "node_name": "Local",
            "node_did": did,
            "p2p_transport": "iroh",
        })
        .to_string(),
    )
    .unwrap();
}

#[tokio::test]
async fn discovery_rejects_a_bad_live_did_without_touching_the_peer_store() {
    let did = "did:key:z6MkLocal";
    for live in [
        json!(null),
        json!(""),
        json!("not-a-did"),
        json!("did:key:z6MkOther"),
    ] {
        let temp = tempfile::tempdir().unwrap();
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = serve_status_after(
            listener,
            std::time::Duration::ZERO,
            json!({ "node_did": live, "lifecycle": "ready" }),
        )
        .await;
        let home = temp.path().join("node");
        seed_home(
            &home,
            did,
            &format!("http://127.0.0.1:{port}/api/v0/graphql"),
        );
        let paths = DesktopPaths::from_root(temp.path().join("desktop"));

        let error = init_standard_local_runtime(DesktopInitOptions {
            node_home: home,
            desktop_paths: paths.clone(),
            label: "Local".to_string(),
        })
        .await
        .expect_err("a live identity other than the initialized one is rejected");
        assert!(error.to_string().contains("serves"), "{error:#}");
        assert!(
            !paths.runtime_directory_path().exists(),
            "no route is saved for {live}"
        );
        server.abort();
    }
}

#[tokio::test]
async fn discovery_waits_for_a_runtime_that_is_not_listening_yet() {
    let did = "did:key:z6MkLocal";
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let runtime = StoredRuntimeState {
        graphql: format!("http://127.0.0.1:{port}/api/v0/graphql"),
        node_name: "Local".to_string(),
        node_did: did.to_string(),
    };
    let server = serve_status_after(
        listener,
        std::time::Duration::from_millis(600),
        json!({ "node_did": did, "lifecycle": "ready" }),
    )
    .await;
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_millis(300))
        .build()
        .unwrap();
    await_serving_runtime_within(&client, &runtime, std::time::Duration::from_secs(10))
        .await
        .expect("discovery waits for the runtime to answer");
    server.abort();
}

#[tokio::test]
async fn discovery_fails_at_once_for_a_runtime_that_predates_readiness() {
    let did = "did:key:z6MkLocal";
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let server = serve_status_after(
        listener,
        std::time::Duration::ZERO,
        json!({ "node_did": did, "version": "0.18.2" }),
    )
    .await;
    let runtime = StoredRuntimeState {
        graphql: format!("http://127.0.0.1:{port}/api/v0/graphql"),
        node_name: "Local".to_string(),
        node_did: did.to_string(),
    };
    let error = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        await_serving_runtime_within(
            &reqwest::Client::new(),
            &runtime,
            std::time::Duration::from_secs(60),
        ),
    )
    .await
    .expect("an outdated runtime is not waited on")
    .unwrap_err();
    assert!(
        error.to_string().contains("v0.18.2) predates this app"),
        "{error:#}"
    );
    server.abort();
}

const HOSTED_GRAPHQL: &str = "http://127.0.0.1:9191/api/v0/graphql";

fn hosted_home(dir: &std::path::Path) -> (std::path::PathBuf, String) {
    use gents_store::identity::NodeIdentity as _;

    let home = dir.join("node");
    std::fs::create_dir_all(&home).unwrap();
    let key_path = home.join("node.key");
    let identity = gents_store::identity::KeyIdentity::load_or_create(&key_path, None).unwrap();
    let did = identity.did().to_string();
    seed_home(&home, &did, HOSTED_GRAPHQL);
    std::fs::write(
        home.join("init.json"),
        json!({ "node_name": "Local", "node_did": did, "key_path": key_path }).to_string(),
    )
    .unwrap();
    (home, did)
}

fn hosted_record(
    home: &std::path::Path,
    did: &str,
    graphql: &str,
) -> gents_protocol::runtime_connection::RuntimeConnection {
    let mut record = gents_protocol::runtime_connection::RuntimeConnection {
        label: ("Local").to_string(),
        node_did: (did).to_string(),
        graphql: (graphql).to_string(),
        local_node_home: None,
        access_request: None,
        access_state: None,
    };
    record.local_node_home = Some(home.to_string_lossy().into_owned());
    record
}

#[tokio::test]
async fn runtime_status_without_a_user_signer_is_explicitly_unavailable() {
    use wiremock::MockServer;
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().to_owned();
    let server = MockServer::start().await;
    let graphql = format!("{}/api/v0/graphql", server.uri());
    let endpoint = graphql.clone();
    let (home, did) = tokio::task::spawn_blocking(move || {
        let (home, did) = hosted_home(&root);
        seed_home(&home, &did, &endpoint);
        (home, did)
    })
    .await
    .unwrap();
    let record = hosted_record(&home, &did, &graphql);
    let error = super::fetch_hosted_runtime_status(&record, None)
        .await
        .unwrap_err();
    assert!(
        matches!(error, super::HostedRuntimeStatusError::NoUserSigner),
        "{error:#}"
    );
    assert!(server.received_requests().await.unwrap().is_empty());
}

/// W7 regression: the managed-runtime status check signs as the User and never
/// opens the co-hosted runtime's node key, even when that key is on disk; a
/// refusal is a typed denial rather than an anonymous retry.
#[tokio::test]
async fn hosted_runtime_status_reads_as_the_user_and_never_loads_the_node_key() {
    use identity::Identity as _;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("node");
    std::fs::create_dir_all(&home).unwrap();
    let key_path = home.join("node.key");
    let node = gents_store::identity::load_or_create_file_identity(&key_path).unwrap();
    let did = node.did().unwrap().to_string();
    let server = MockServer::start().await;
    let graphql = format!("{}/api/v0/graphql", server.uri());
    seed_home(&home, &did, &graphql);
    std::fs::write(
        home.join("init.json"),
        json!({ "node_name": "Local", "node_did": did, "key_path": key_path }).to_string(),
    )
    .unwrap();
    let user =
        gents_store::identity::KeyIdentity::load_or_create(directory.path().join("user.key"), None)
            .unwrap();
    let capability = user.client_signing_capability().unwrap();
    let record = hosted_record(&home, &did, &graphql);
    let signed_did = did.clone();
    Mock::given(method("GET"))
        .and(path("/status"))
        .respond_with(move |request: &wiremock::Request| {
            use identity::FullIdentity as _;
            let mut status = gents_store::test_support::local_home_status_now(
                &signed_did,
                &|payload| node.sign(payload).unwrap(),
                request.url.query(),
            );
            status["tool_root"] = "/w".into();
            ResponseTemplate::new(200).set_body_json(status)
        })
        .mount(&server)
        .await;
    let payload = super::fetch_hosted_runtime_status(&record, Some(&capability))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(payload["tool_root"], "/w");
    assert!(!gents_store::identity::can_mint_defradb_bearer(&did));
    let requests = server.received_requests().await.unwrap();
    let bearer = requests
        .iter()
        .find_map(|request| request.headers.get("authorization"))
        .unwrap()
        .to_str()
        .unwrap()
        .strip_prefix("Bearer ")
        .unwrap();
    assert_eq!(
        identity::from_token(bearer.as_bytes())
            .unwrap()
            .did()
            .unwrap()
            .to_string(),
        gents_store::identity::NodeIdentity::did(&user)
    );

    server.reset().await;
    Mock::given(method("GET"))
        .and(path("/status"))
        .respond_with(ResponseTemplate::new(403))
        .mount(&server)
        .await;
    let error = super::fetch_hosted_runtime_status(&record, Some(&capability))
        .await
        .unwrap_err();
    assert!(
        matches!(
            &error,
            super::HostedRuntimeStatusError::Denied { node_did, status: 403 } if node_did == &did
        ),
        "{error:#}"
    );
    assert!(!gents_store::identity::can_mint_defradb_bearer(&did));
}

#[tokio::test]
async fn managed_readiness_status_rejects_redirects_without_forwarding_authority() {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let directory = tempfile::tempdir().unwrap();
    let (home, did) = hosted_home(directory.path());
    let server = MockServer::start().await;
    let graphql = format!("{}/api/v0/graphql", server.uri());
    // Re-point only the live runtime state: `init.json` keeps the runtime
    // key path the hosted record opens.
    std::fs::write(
        home.join("runtime.json"),
        json!({"graphql": graphql, "node_name": "Local", "node_did": did, "p2p_transport": "iroh"})
            .to_string(),
    )
    .unwrap();
    Mock::given(method("GET"))
        .and(path("/status"))
        .respond_with(
            ResponseTemplate::new(302)
                .insert_header("Location", format!("{}/forwarded", server.uri()))
                .set_body_json(json!({"node_did":did,"tool_root":"unreviewed"})),
        )
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/forwarded"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({})))
        .expect(0)
        .mount(&server)
        .await;
    let caller = gents_store::identity::KeyIdentity::load_or_create(
        directory.path().join("caller.key"),
        None,
    )
    .unwrap();
    let error = super::fetch_hosted_runtime_status(
        &hosted_record(&home, &did, &graphql),
        Some(&caller.client_signing_capability().unwrap()),
    )
    .await
    .unwrap_err();
    assert!(error.to_string().contains("302"), "{error:#}");
}

#[tokio::test]
async fn document_operator_endpoint_preserves_client_without_loading_runtime_key() {
    let directory = tempfile::tempdir().unwrap();
    let runtime_key =
        gents_store::identity::load_or_create_file_identity(&directory.path().join("runtime.key"))
            .unwrap();
    let runtime_did = {
        use identity::Identity as _;
        runtime_key.did().unwrap().to_string()
    };
    let runtime = runtime_did.as_str();
    let caller = gents_store::identity::KeyIdentity::load_or_create(
        directory.path().join("caller.key"),
        None,
    )
    .unwrap();
    let server = wiremock::MockServer::start().await;
    let signed_did = runtime_did.clone();
    wiremock::Mock::given(wiremock::matchers::path("/status"))
        .respond_with(move |request: &wiremock::Request| {
            use identity::FullIdentity as _;
            wiremock::ResponseTemplate::new(200).set_body_json(
                gents_store::test_support::local_home_status_now(
                    &signed_did,
                    &|payload| runtime_key.sign(payload).unwrap(),
                    request.url.query(),
                ),
            )
        })
        .mount(&server)
        .await;
    let record = hosted_record(
        directory.path(),
        runtime,
        &format!("{}/api/v0/graphql", server.uri()),
    );
    let capability = caller.client_signing_capability().unwrap();
    let endpoint = super::operator_endpoint(&record, &capability).unwrap();
    assert!(endpoint.runtime_authorization().await.unwrap().is_some());
    assert!(!gents_store::identity::can_mint_defradb_bearer(runtime));
}

#[tokio::test]
async fn runtime_status_uses_the_caller_and_rejects_another_node() {
    use gents_store::identity::NodeIdentity as _;
    use identity::Identity as _;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};
    let directory = tempfile::tempdir().unwrap();
    let caller = gents_store::identity::KeyIdentity::load_or_create(
        directory.path().join("caller.key"),
        None,
    )
    .unwrap();
    let server = MockServer::start().await;
    let endpoint = gents_store::config_client::GraphqlEndpoint::as_principal(
        format!("{}/api/v0/graphql", server.uri()),
        caller.did(),
    )
    .with_client_signing_capability(caller.client_signing_capability().unwrap())
    .unwrap();
    Mock::given(method("GET"))
        .and(path("/status"))
        .respond_with(
            ResponseTemplate::new(200)
                .set_body_json(json!({"node_did":"did:key:target", "service":"gents"})),
        )
        .expect(2)
        .mount(&server)
        .await;
    let value = super::fetch_runtime_status_for_node(&endpoint, "did:key:target")
        .await
        .unwrap();
    assert!(value.get("liveness").is_none());
    assert!(
        super::fetch_runtime_status_for_node(&endpoint, "did:key:other")
            .await
            .is_err()
    );
    for request in server.received_requests().await.unwrap() {
        let bearer = request
            .headers
            .get("authorization")
            .unwrap()
            .to_str()
            .unwrap()
            .strip_prefix("Bearer ")
            .unwrap();
        assert_eq!(
            identity::from_token(bearer.as_bytes())
                .unwrap()
                .did()
                .unwrap()
                .to_string(),
            caller.did()
        );
    }
}

/// A home whose edge requires a delegation this client cannot learn to
/// present is told as an update of Gents, never as a raw refusal.
#[tokio::test]
async fn a_status_read_refused_at_a_vera_edge_asks_for_an_update() {
    let home = std::sync::Arc::new(gents_store::test_support::test_identity(
        "update-required-home",
    ));
    let home_did = gents_store::identity::NodeIdentity::did(home.as_ref()).to_owned();
    let signer = home.clone();
    let edge = axum::Router::new().route(
        "/status",
        axum::routing::get(
            move |headers: axum::http::HeaderMap,
                  query: axum::extract::Query<rapidhash::RapidHashMap<String, String>>| {
                let signer = signer.clone();
                async move {
                    use axum::response::IntoResponse;
                    if headers.contains_key(axum::http::header::AUTHORIZATION) {
                        return (
                            axum::http::StatusCode::UNAUTHORIZED,
                            axum::Json(json!({
                                "code": gents_protocol::delegation_audience::DELEGATION_REQUIRED_CODE,
                                "error": "refused",
                            })),
                        )
                            .into_response();
                    }
                    let Some(nonce) = query
                        .get(gents_protocol::home_attestation::HOME_NONCE_QUERY)
                        .and_then(|nonce| {
                            gents_protocol::home_attestation::HomeNonce::parse(nonce).ok()
                        })
                    else {
                        return axum::http::StatusCode::BAD_REQUEST.into_response();
                    };
                    axum::Json(
                        gents_store::test_support::signed_home_status(
                            signer.as_ref(),
                            &gents_protocol::delegation_audience::ReportedDelegationAudience::Absent,
                            None,
                            &nonce,
                        )
                        .await,
                    )
                    .into_response()
                }
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let served = tokio::spawn(async move { axum::serve(listener, edge).await });
    let user = gents_store::test_support::test_identity("update-required-user");
    let capability = gents_store::identity::NodeIdentity::client_signing_capability(&user).unwrap();
    let record = hosted_record(
        std::path::Path::new("."),
        &home_did,
        &format!("http://{address}/api/v0/graphql"),
    );
    let error = super::fetch_hosted_runtime_status(&record, Some(&capability))
        .await
        .unwrap_err();
    assert!(
        matches!(&error, super::HostedRuntimeStatusError::Failed(failed)
            if failed.downcast_ref::<gents_store::client_error::BridgeError>().map(|bridge| bridge.code)
                == Some(gents_store::client_error::BridgeErrorCode::UpdateRequired)),
        "{error:#}"
    );
    assert!(format!("{error:#}").contains("update Gents"));
    served.abort();
}

/// A GraphQL request whose identity token the edge of a Vera home refuses is
/// told as the same update of Gents, not as a raw HTTP 401; any other 401
/// stays the refusal it was.
#[tokio::test]
async fn a_graphql_request_refused_at_a_vera_edge_asks_for_an_update() {
    use wiremock::matchers::method;
    use wiremock::{Mock, MockServer, ResponseTemplate};
    for (body, update) in [
        (
            json!({
                "code": gents_protocol::delegation_audience::DELEGATION_REQUIRED_CODE,
                "error": "refused",
            }),
            true,
        ),
        (json!({ "error": "refused" }), false),
    ] {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .respond_with(ResponseTemplate::new(401).set_body_json(body))
            .mount(&server)
            .await;
        let user = gents_store::test_support::test_identity("graphql-update-user");
        let capability =
            gents_store::identity::NodeIdentity::client_signing_capability(&user).unwrap();
        let endpoint = gents_store::config_client::GraphqlEndpoint::as_principal(
            format!("{}/api/v0/graphql", server.uri()),
            capability.did(),
        )
        .with_client_signing_capability(capability)
        .unwrap();
        let error =
            gents_store::config_client::post_graphql(&endpoint, "query { Node { _docID } }")
                .await
                .unwrap_err();
        let bridge = gents_store::client_error::BridgeError::classify_transport_error(error.into());
        assert_eq!(
            bridge.code == gents_store::client_error::BridgeErrorCode::UpdateRequired,
            update,
            "{bridge:?}"
        );
    }
}
