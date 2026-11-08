use super::*;
use axum::{body::Body, http::Uri};
use gents_desktop_core::client::{DesktopPaths, PrincipalIdentity};
use serde::Deserialize;
use serde_json::Value;

fn request(host: &str) -> Request {
    Request::builder()
        .uri("/status")
        .header(header::HOST, host)
        .body(Body::empty())
        .unwrap()
}

#[test]
fn authority_matching_rejects_rebinding_ambiguous_hosts_and_wrong_ports() {
    let policy = HttpAuthority::for_endpoint("http://100.64.0.7:9191/api/v0/graphql");
    for host in [
        "localhost:9191",
        "LOCALHOST:9191",
        "127.0.0.1:9191",
        "[::1]:9191",
        "100.64.0.7:9191",
    ] {
        assert!(policy.accepts(&request(host)), "{host}");
    }
    for host in [
        "attacker.example:9191",
        "localhost",
        "localhost:80",
        "localhost:9192",
        "localhost.attacker.example:9191",
        "localhost.:9191",
        "user@localhost:9191",
        "0.0.0.0:9191",
        "[::]:9191",
        "localhost:invalid",
        "localhost:65536",
        "localhost:",
    ] {
        assert!(!policy.accepts(&request(host)), "{host}");
    }
    let mut ambiguous = request("localhost:9191");
    ambiguous.headers_mut().append(
        header::HOST,
        header::HeaderValue::from_static("127.0.0.1:9191"),
    );
    assert!(!policy.accepts(&ambiguous));
    let mut absolute = request("localhost:9191");
    *absolute.uri_mut() = Uri::from_static("http://attacker.example:9191/status");
    assert!(!policy.accepts(&absolute));
    *absolute.uri_mut() = Uri::from_static("http://localhost:9191/status");
    assert!(policy.accepts(&absolute));
    let mut missing = request("localhost:9191");
    missing.headers_mut().remove(header::HOST);
    assert!(!policy.accepts(&missing));
    assert!(!HttpAuthority::for_endpoint("invalid").accepts(&request("localhost:9191")));
    for invalid in ["localhost:invalid", "localhost:+80", "localhost:"] {
        assert!(!HttpAuthority::for_endpoint("http://localhost").accepts(&request(invalid)));
    }
}

fn principal_endpoint(
    url: impl Into<String>,
    identity: &PrincipalIdentity,
) -> gents::store::config_client::GraphqlEndpoint {
    gents::store::config_client::GraphqlEndpoint::as_principal(url, identity.did())
        .with_client_signing_capability(
            gents::store::config_client::ClientSigningCapability::from_ed25519_key(
                identity.private_key_bytes(),
            )
            .unwrap(),
        )
        .unwrap()
}

struct Fixture {
    _temp: tempfile::TempDir,
    _server: tokio_util::task::AbortOnDropHandle<()>,
    owner: PrincipalIdentity,
    other: PrincipalIdentity,
    admin: PrincipalIdentity,
    endpoint: String,
    client: reqwest::Client,
    queries: std::sync::Arc<std::sync::atomic::AtomicUsize>,
    query_bearers: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    status_requests: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

fn runtime_routes(endpoint: &str, owner: &PrincipalIdentity) -> axum::Router {
    let (runtime, observation) = crate::http::router::empty_activation_state();
    crate::http::runtime_contract_router(
        format!("{endpoint}/api/v0/graphql"),
        gents::store::config_client::GraphqlEndpoint::as_principal(
            format!("{endpoint}/api/v0/graphql"),
            owner.did(),
        )
        .with_client_signing_capability(
            gents::store::config_client::ClientSigningCapability::from_ed25519_key(
                owner.private_key_bytes(),
            )
            .unwrap(),
        )
        .unwrap(),
        "test-runtime".into(),
        owner.did().into(),
        "readwrite".into(),
        Ok(Some("/private/operator/workspace".into())),
        Vec::new(),
        Some("/private/operator/home".into()),
        None,
        None,
        None,
        None,
        None,
        crate::http::enrollment::empty_issuer_handle(),
        crate::http::enrollment::empty_decision_service_handle(),
        runtime,
        observation,
        Default::default(),
        Default::default(),
    )
    .contract
}

impl Fixture {
    async fn start() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let owner_paths = DesktopPaths::from_root(temp.path().join("owner"));
        let other_paths = DesktopPaths::from_root(temp.path().join("other"));
        let admin_paths = DesktopPaths::from_root(temp.path().join("admin"));
        owner_paths.ensure_root_dirs().await.unwrap();
        other_paths.ensure_root_dirs().await.unwrap();
        admin_paths.ensure_root_dirs().await.unwrap();
        let owner = PrincipalIdentity::load_or_create(&owner_paths)
            .await
            .unwrap();
        let other = PrincipalIdentity::load_or_create(&other_paths)
            .await
            .unwrap();
        let admin = PrincipalIdentity::load_or_create(&admin_paths)
            .await
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let router = runtime_routes(&endpoint, &owner);
        let queries = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed_queries = queries.clone();
        let query_bearers = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let observed_bearers = query_bearers.clone();
        let admin_did = admin.did().to_owned();
        let router = router
            .merge(axum::Router::new().route(
                "/api/v0/graphql",
                axum::routing::post(move |headers: axum::http::HeaderMap| {
                    let queries = observed_queries.clone();
                    let bearers = observed_bearers.clone();
                    async move {
                        queries.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                        if let Some(bearer) = headers
                            .get(header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                        {
                            bearers.lock().unwrap().push(bearer.to_owned());
                        }
                        axum::Json(serde_json::json!({ "data": {} }))
                    }
                }),
            ))
            // Native NAC's explanation to the subject itself; only the
            // fixture admin holds the `admin` relation.
            .merge(axum::Router::new().route(
                "/api/v0/acp/node/explain",
                axum::routing::get(
                    move |query: axum::extract::Query<rapidhash::RapidHashMap<String, String>>| {
                        let admin_did = admin_did.clone();
                        async move {
                            let grants = if query.get("subject") == Some(&admin_did) {
                                vec![acp::nac::PermissionProvenance {
                                    permission: acp::nac::NodePermission::NacStatus,
                                    via: acp::nac::PermissionVia::Admin,
                                }]
                            } else {
                                Vec::new()
                            };
                            axum::Json(grants)
                        }
                    },
                ),
            ));
        let status_requests = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed_status_requests = status_requests.clone();
        let router = router.layer(axum::middleware::from_fn(
            move |request: Request, next: axum::middleware::Next| {
                let status_requests = observed_status_requests.clone();
                async move {
                    if request.uri().path() == "/status" {
                        status_requests.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    }
                    next.run(request).await
                }
            },
        ));
        let router = HttpAuthority::for_endpoint(&endpoint).wrap(router);
        let server = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        }));
        Self {
            _temp: temp,
            _server: server,
            owner,
            other,
            admin,
            endpoint,
            client: reqwest::Client::builder().no_proxy().build().unwrap(),
            queries,
            query_bearers,
            status_requests,
        }
    }

    fn get(&self, path: &str) -> reqwest::RequestBuilder {
        self.client.get(format!("{}{path}", self.endpoint))
    }
}

#[tokio::test]
async fn startup_gate_blocks_concurrent_client_operations_until_owner_bootstrap_finishes() {
    use axum::{
        routing::{get, post},
        Router,
    };
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;

    let ready = Arc::new(AtomicBool::new(false));
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = calls.clone();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}", listener.local_addr().unwrap());
    let router = Router::new()
        .route("/private-bind-probe", get(|| async { "probe" }))
        .route(
            "/api/v0/graphql",
            post(move || {
                let counter = counter.clone();
                async move {
                    counter.fetch_add(1, Ordering::SeqCst);
                    "ok"
                }
            }),
        );
    let router = HttpAuthority::for_endpoint(&endpoint)
        .with_startup_gate(ready.clone(), "/private-bind-probe".into())
        .wrap(router);
    let _server = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    }));
    let client = reqwest::Client::new();
    assert_eq!(
        client
            .get(format!("{endpoint}/private-bind-probe"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let mut pending = tokio::task::JoinSet::new();
    for _ in 0..16 {
        let client = client.clone();
        let url = format!("{endpoint}/api/v0/graphql");
        pending.spawn(async move { client.post(url).send().await.unwrap().status() });
    }
    while let Some(response) = pending.join_next().await {
        assert_eq!(response.unwrap(), StatusCode::SERVICE_UNAVAILABLE);
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
    ready.store(true, Ordering::Release);
    assert_eq!(
        client
            .post(format!("{endpoint}/api/v0/graphql"))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn complete_http_router_refuses_foreign_hosts_and_preserves_operator_authorization() {
    let directory = tempfile::tempdir().unwrap();
    let paths = DesktopPaths::from_root(directory.path().join("operator"));
    paths.ensure_root_dirs().await.unwrap();
    let owner = PrincipalIdentity::load_or_create(&paths).await.unwrap();
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let endpoint = format!("http://{address}");
    let ready = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let config = HttpAuthority::for_endpoint(&endpoint)
        .with_startup_gate(ready.clone(), "/private-bind-probe".into())
        .protect_http(
            gents::defra_node::HttpConfig::with_listener(listener)
                .unwrap()
                .with_extra_routes(runtime_routes(&endpoint, &owner)),
        );
    let node = gents::defra_node::Node::builder()
        .with_node_identity_did(owner.did())
        .with_node_acp_enabled()
        .with_http(config)
        .build()
        .await
        .unwrap();
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap();

    for path in ["/api/v0/graphql", "/api/v0/collections", "/status"] {
        assert_eq!(
            client
                .get(format!("{endpoint}{path}"))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }
    ready.store(true, std::sync::atomic::Ordering::Release);

    for path in [
        "/health-check",
        "/api/v0/version",
        "/api/v1/version/",
        "/api/v0/graphql",
        "/api/v0/schema",
        "/api/v0/collections",
        "/api/v0/p2p/info",
        "/version",
        "/status",
        "/self",
        "/sessions",
        "/missing/",
    ] {
        for method in [reqwest::Method::GET, reqwest::Method::POST] {
            let response = client
                .request(method.clone(), format!("{endpoint}{path}"))
                .header(header::HOST, "attacker.example:9191")
                .send()
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::MISDIRECTED_REQUEST,
                "{method} {path}"
            );
            assert_eq!(
                response.headers()[header::CACHE_CONTROL],
                "no-store",
                "{method} {path}"
            );
        }
    }

    for (path, expected) in [
        ("/health-check", StatusCode::OK),
        ("/api/v0/version", StatusCode::OK),
        ("/api/v1/version/", StatusCode::OK),
        ("/version", StatusCode::OK),
        ("/missing/", StatusCode::NOT_FOUND),
        ("/sessions", StatusCode::UNAUTHORIZED),
    ] {
        let response = client
            .get(format!("{endpoint}{path}"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), expected, "{path}");
    }
    let response = principal_endpoint(format!("{endpoint}/api/v0/graphql"), &owner)
        .authorize(client.get(format!("{endpoint}/api/v0/collections")))
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    node.shutdown().await.expect("node shutdown");
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Case {
    authority_matches: bool,
    surface: String,
    identity: String,
    decision: String,
    reads_as: Option<String>,
}

#[tokio::test]
async fn generated_http_access_cases_bind_real_bearers_and_router_dispatch() {
    let stdout = gents_lean_contract::run_prepared_generator(
        &gents_lean_contract::proofs_dir().unwrap(),
        "Proofs.Conformance.HttpAccess:olean",
        "Proofs/Conformance/HttpAccess.lean",
    )
    .unwrap();
    let cases: Vec<Case> = serde_json::from_str(&stdout).unwrap();
    assert_eq!(cases.len(), 24);
    let f = Fixture::start().await;
    for case in cases {
        let path = match case.surface.as_str() {
            "bootstrap" => "/status",
            "operator" => "/activation",
            "snapshot" => "/fleet/slots",
            _ => panic!("unknown surface"),
        };
        let mut request = f.get(path);
        let identity = match case.identity.as_str() {
            "operator" => Some(&f.owner),
            "other" => Some(&f.other),
            "admin" => Some(&f.admin),
            "anonymous" => None,
            _ => panic!("unknown identity"),
        };
        let mut bearer = None;
        if let Some(identity) = identity {
            let authorization =
                principal_endpoint(format!("{}/api/v0/graphql", f.endpoint), identity)
                    .authorization()
                    .unwrap()
                    .unwrap();
            request = request.header(header::AUTHORIZATION, &authorization);
            bearer = Some(authorization);
        }
        f.query_bearers.lock().unwrap().clear();
        if !case.authority_matches {
            request = request.header(header::HOST, "attacker.example:9191");
        }
        let queries_before = f.queries.load(std::sync::atomic::Ordering::SeqCst);
        let response = request.send().await.unwrap();
        let expected = match case.decision.as_str() {
            "misdirected" => StatusCode::MISDIRECTED_REQUEST,
            "unauthorized" => StatusCode::UNAUTHORIZED,
            "forbidden" => StatusCode::FORBIDDEN,
            "bootstrap" => StatusCode::OK,
            "operator" if path == "/activation" => StatusCode::SERVICE_UNAVAILABLE,
            "operator" => StatusCode::OK,
            _ => panic!("unknown decision"),
        };
        assert_eq!(
            response.status(),
            expected,
            "{} {} {}",
            case.surface,
            case.identity,
            case.authority_matches
        );
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        if case.decision == "unauthorized" {
            assert_eq!(response.headers()[header::WWW_AUTHENTICATE], "Bearer");
        }
        if path == "/fleet/slots" {
            let bearers = f.query_bearers.lock().unwrap().clone();
            if case.decision == "operator" {
                assert_eq!(case.reads_as.as_deref(), Some(case.identity.as_str()));
                assert!(!bearers.is_empty(), "the slot snapshot is read");
                assert!(
                    bearers.iter().all(|seen| Some(seen) == bearer.as_ref()),
                    "the slot snapshot reads as its caller {}: {bearers:?}",
                    case.identity
                );
            } else {
                assert_eq!(case.reads_as, None);
                assert!(bearers.is_empty(), "refused snapshots read nothing");
            }
            continue;
        }
        let body: Value = response.json().await.unwrap();
        if case.decision == "operator" && path == "/status" {
            assert_eq!(body["home"], "/private/operator/home");
            assert_eq!(body["tool_root"], "/private/operator/workspace");
            assert_eq!(case.reads_as.as_deref(), Some(case.identity.as_str()));
            let bearers = f.query_bearers.lock().unwrap().clone();
            assert!(!bearers.is_empty(), "private status reads the node's data");
            assert!(
                bearers.iter().all(|seen| Some(seen) == bearer.as_ref()),
                "private status reads as its caller {}: {bearers:?}",
                case.identity
            );
        }
        if case.decision != "operator" {
            assert_eq!(case.reads_as, None);
        }
        if case.decision != "operator" {
            assert_eq!(
                f.queries.load(std::sync::atomic::Ordering::SeqCst),
                queries_before,
                "refused and bootstrap requests must not query as the node"
            );
        }
        if case.decision == "unauthorized" {
            assert_eq!(body["error"], "Runtime operator authorization is required.");
        }
        if case.decision == "bootstrap" {
            assert_eq!(body["node_did"], f.owner.did());
            assert!(body.get("enrollment").is_some());
            assert!(body
                .get(gents_protocol::serve_lifecycle::STATUS_LIFECYCLE_FIELD)
                .is_some());
            for key in [
                "ok",
                "status",
                "home",
                "tool_root",
                "tool_root_error",
                "cli_tools",
                "agents",
                "backends",
                "runtimes",
                "context",
                "context_budget",
                "p2p",
                "error",
            ] {
                assert!(body.get(key).is_none(), "private bootstrap field: {key}");
            }
        }
    }
}

#[tokio::test]
async fn node_principal_routes_refuse_anonymous_and_foreign_callers_before_work() {
    let f = Fixture::start().await;
    let authorization = principal_endpoint(format!("{}/api/v0/graphql", f.endpoint), &f.other)
        .authorization()
        .unwrap()
        .unwrap();
    for path in [
        "/self",
        "/sessions",
        "/fleet",
        "/fleet/slots",
        "/mcp/pool",
        "/healthz",
        "/metrics",
        "/activation",
        "/identity/decide",
        "/packs/ux/watch",
    ] {
        for foreign in [false, true] {
            let mut request = if path == "/identity/decide" {
                f.client.post(format!("{}{path}", f.endpoint)).json(&serde_json::json!({ "actor": "untrusted", "permission": "read", "resource": "policy/row/document" }))
            } else if path == "/packs/ux/watch" {
                f.client
                    .post(format!("{}{path}", f.endpoint))
                    .json(&serde_json::json!({"ux":"x", "nodeDid":"x", "surface":"x", "tool":"x"}))
            } else {
                f.get(path)
            };
            if foreign {
                request = request.header(header::AUTHORIZATION, &authorization);
            }
            let response = request.send().await.unwrap();
            assert_eq!(
                response.status(),
                if foreign {
                    StatusCode::FORBIDDEN
                } else {
                    StatusCode::UNAUTHORIZED
                },
                "{path}"
            );
        }
    }
    assert_eq!(f.queries.load(std::sync::atomic::Ordering::SeqCst), 0);
    let invalid = f
        .get("/sessions")
        .bearer_auth("invalid")
        .send()
        .await
        .unwrap();
    assert_eq!(invalid.status(), StatusCode::FORBIDDEN);
    assert_eq!(f.queries.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test]
async fn foreign_host_is_refused_on_public_private_and_independently_authenticated_routes() {
    let f = Fixture::start().await;
    for path in [
        "/version",
        "/status",
        "/healthz",
        "/metrics",
        "/self",
        "/sessions",
        "/fleet",
        "/fleet/slots",
        "/mcp/pool",
        "/activation",
        "/identity/decide",
        gents_protocol::agent_identity::AGENT_IDENTITY_PATH,
        "/enrollment/decisions",
        "/enrollment/pending",
        "/accounts/usage/read",
        gents_protocol::serve_lifecycle::control::RUNTIME_CONTROL_PATH,
        gents_desktop_core::inference::INFERENCE_SETTINGS_PATH,
        gents_protocol::file_inventory::FILE_INVENTORY_OPERATOR_PATH,
    ] {
        let response = f
            .get(path)
            .header(header::HOST, "attacker.example")
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::MISDIRECTED_REQUEST, "{path}");
    }
}

#[tokio::test]
async fn managed_readiness_status_requires_a_held_hosted_node_key() {
    use gents_desktop_core::local_runtime::{
        fetch_hosted_runtime_status, OperatorIdentityInteraction,
    };
    let f = Fixture::start().await;
    let home = f._temp.path().join("runtime-home");
    std::fs::create_dir_all(&home).unwrap();
    let graphql = format!("{}/api/v0/graphql", f.endpoint);
    std::fs::write(
        home.join("runtime.json"),
        serde_json::json!({
            "node_name":"test-runtime", "node_did": f.owner.did(), "graphql":graphql,
        })
        .to_string(),
    )
    .unwrap();
    let mut record = gents_desktop_core::client::PeerRecord::local_standard(
        "Local",
        "",
        f.owner.did(),
        &graphql,
    );
    record.local_node_home = Some(home.to_string_lossy().into_owned());
    let public = gents_desktop_core::local_runtime::fetch_runtime_connection_payload(&format!(
        "{}/status",
        f.endpoint
    ))
    .await
    .unwrap();
    assert!(public.get("tool_root").is_none());
    let calls = f.status_requests.load(std::sync::atomic::Ordering::SeqCst);
    assert_eq!(calls, 1);
    let error = fetch_hosted_runtime_status(&record, OperatorIdentityInteraction::Prevent)
        .await
        .unwrap_err();
    assert!(
        matches!(
            error.downcast_ref::<gents::store::config_client::ConfigClientError>(),
            Some(gents::store::config_client::ConfigClientError::ClientAgentSignerRequired)
        ),
        "{error:#}"
    );
    assert_eq!(
        f.status_requests.load(std::sync::atomic::Ordering::SeqCst),
        calls
    );
    record.graphql = Some("http://127.0.0.1:1/api/v0/graphql".into());
    let error = fetch_hosted_runtime_status(&record, OperatorIdentityInteraction::Prevent)
        .await
        .expect_err("authority never follows a repointed endpoint");
    assert!(
        matches!(
            error.downcast_ref::<gents::store::config_client::ConfigClientError>(),
            Some(gents::store::config_client::ConfigClientError::ClientAgentSignerRequired)
        ),
        "{error:#}"
    );
    assert_eq!(
        f.status_requests.load(std::sync::atomic::Ordering::SeqCst),
        calls
    );
}

#[tokio::test]
async fn remote_pack_install_requires_authenticated_client_before_work() {
    let fixture = Fixture::start().await;
    let body = serde_json::json!({
        "node_did": fixture.owner.did(), "root": "", "dependencies": [],
        "inference_slots": [], "overwrite": false, "keep": false,
        "preview": false, "force_rebind": false, "grant_authority": false,
        "explicit": true, "wire": []
    });
    // An authenticated caller reaches request decoding, which refuses this
    // archive-less body before any database work.
    for (identity, expected) in [
        (None, StatusCode::UNAUTHORIZED),
        (Some(&fixture.other), StatusCode::BAD_REQUEST),
        (Some(&fixture.owner), StatusCode::BAD_REQUEST),
    ] {
        let mut request = fixture
            .client
            .post(format!("{}/packs/install", fixture.endpoint))
            .json(&body);
        if let Some(identity) = identity {
            request = principal_endpoint(format!("{}/api/v0/graphql", fixture.endpoint), identity)
                .authorize(request)
                .unwrap();
        }
        let response = request.send().await.unwrap();
        assert_eq!(response.status(), expected);
    }
    assert_eq!(fixture.queries.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[tokio::test]
async fn agent_identity_provisioning_requires_client_authentication_and_writes_no_config() {
    let f = Fixture::start().await;
    let url = format!(
        "{}{}",
        f.endpoint,
        gents_protocol::agent_identity::AGENT_IDENTITY_PATH
    );
    let unsigned = f
        .client
        .post(&url)
        .json(&serde_json::json!({"node_did":f.owner.did()}))
        .send()
        .await
        .unwrap();
    assert_eq!(unsigned.status(), StatusCode::UNAUTHORIZED);
    let endpoint = principal_endpoint(format!("{}/api/v0/graphql", f.endpoint), &f.owner);
    let response = endpoint
        .authorize(
            f.client
                .post(&url)
                .json(&serde_json::json!({"node_did":"different-node"})),
        )
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(f.queries.load(std::sync::atomic::Ordering::SeqCst), 0);
}

#[test]
fn advertised_authority_is_explicit_and_does_not_trust_forwarded_headers() {
    let authority = HttpAuthority::for_endpoint("http://0.0.0.0:9191/api/v0/graphql")
        .with_advertised_endpoint(Some("https://runtime.example:8443/api/v0/graphql"));
    assert!(authority.accepts(&request("runtime.example:8443")));
    assert!(authority.accepts(&request("localhost:9191")));
    assert!(!authority.accepts(&request("runtime.example:9191")));
    let mut foreign = request("attacker.example:8443");
    foreign.headers_mut().insert(
        "x-forwarded-host",
        header::HeaderValue::from_static("runtime.example:8443"),
    );
    assert!(!authority.accepts(&foreign));
}
