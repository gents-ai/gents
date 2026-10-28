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

struct Fixture {
    _temp: tempfile::TempDir,
    _server: tokio_util::task::AbortOnDropHandle<()>,
    owner: PrincipalIdentity,
    other: PrincipalIdentity,
    endpoint: String,
    client: reqwest::Client,
    queries: std::sync::Arc<std::sync::atomic::AtomicUsize>,
}

impl Fixture {
    async fn start() -> Self {
        Self::start_with_home(None).await
    }

    async fn start_with_home(home: Option<String>) -> Self {
        let temp = tempfile::tempdir().unwrap();
        let owner_paths = DesktopPaths::from_root(temp.path().join("owner"));
        let other_paths = DesktopPaths::from_root(temp.path().join("other"));
        owner_paths.ensure_root_dirs().await.unwrap();
        other_paths.ensure_root_dirs().await.unwrap();
        let owner = PrincipalIdentity::load_or_create(&owner_paths)
            .await
            .unwrap();
        let other = PrincipalIdentity::load_or_create(&other_paths)
            .await
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}", listener.local_addr().unwrap());
        let (runtime, observation) = crate::http::router::empty_activation_state();
        let router = crate::http::runtime_contract_router(
            format!("{endpoint}/api/v0/graphql"),
            "test-runtime".into(),
            owner.did().into(),
            "readwrite".into(),
            Ok(Some("/private/operator/workspace".into())),
            Vec::new(),
            Some(home.unwrap_or_else(|| "/private/operator/home".into())),
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
        );
        let queries = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let observed_queries = queries.clone();
        let router = router.merge(axum::Router::new().route(
            "/api/v0/graphql",
            axum::routing::post(move || {
                let queries = observed_queries.clone();
                async move {
                    queries.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
                    axum::Json(serde_json::json!({ "data": {} }))
                }
            }),
        ));
        let server = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        }));
        Self {
            _temp: temp,
            _server: server,
            owner,
            other,
            endpoint,
            client: reqwest::Client::builder().no_proxy().build().unwrap(),
            queries,
        }
    }

    fn get(&self, path: &str) -> reqwest::RequestBuilder {
        self.client.get(format!("{}{path}", self.endpoint))
    }
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Case {
    authority_matches: bool,
    surface: String,
    identity: String,
    decision: String,
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
    assert_eq!(cases.len(), 12);
    let f = Fixture::start().await;
    for case in cases {
        let path = if case.surface == "bootstrap" {
            "/status"
        } else {
            "/activation"
        };
        let mut request = f.get(path);
        let identity = match case.identity.as_str() {
            "operator" => Some(&f.owner),
            "other" => Some(&f.other),
            "anonymous" => None,
            _ => panic!("unknown identity"),
        };
        if let Some(identity) = identity {
            request = gents::config_client::GraphqlEndpoint::as_principal(
                format!("{}/api/v0/graphql", f.endpoint),
                identity.did(),
            )
            .authorize(request)
            .unwrap();
        }
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
        let body: Value = response.json().await.unwrap();
        if case.decision == "operator" && path == "/status" {
            assert_eq!(body["home"], "/private/operator/home");
            assert_eq!(body["tool_root"], "/private/operator/workspace");
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
    let authorization = gents::config_client::GraphqlEndpoint::as_principal(
        format!("{}/api/v0/graphql", f.endpoint),
        f.other.did(),
    )
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
    ] {
        for foreign in [false, true] {
            let mut request = if path == "/identity/decide" {
                f.client.post(format!("{}{path}", f.endpoint)).json(&serde_json::json!({ "actor": "untrusted", "permission": "read", "resource": "policy/row/document" }))
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
        "/enrollment/decisions",
        "/enrollment/pending",
        "/accounts/usage/read",
        gents_protocol::serve_lifecycle::control::RUNTIME_CONTROL_PATH,
        gents_protocol::job::JOB_CONTROL_PATH,
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
async fn plugin_preflight_requires_the_operator_and_reads_the_runtime_store() {
    use gents::pack::plugin_references::{CheckResult, CHECK_PATH};
    let home = tempfile::tempdir().unwrap();
    let record = serde_json::from_value(serde_json::json!({
        "namespace": "acme", "name": "search", "version": "1.0.0",
        "digest": format!("sha256:{}", "a".repeat(64)), "language": "rust",
        "declaration": {"name": "search", "description": "Search", "artifact": "plugins/search.afb", "language": "rust", "input_schema": {"type": "object"}}
    })).unwrap();
    gents::plugin::store::write_record(home.path(), &record).unwrap();
    let f = Fixture::start_with_home(Some(home.path().to_str().unwrap().into())).await;
    for (identity, expected) in [
        (None, StatusCode::UNAUTHORIZED),
        (Some(&f.other), StatusCode::FORBIDDEN),
        (Some(&f.owner), StatusCode::OK),
    ] {
        let mut request = f
            .client
            .post(format!("{}{CHECK_PATH}", f.endpoint))
            .json(&serde_json::json!({"plugins": ["acme/search", "acme/missing"]}));
        if let Some(identity) = identity {
            request = gents::config_client::GraphqlEndpoint::as_principal(
                format!("{}/api/v0/graphql", f.endpoint),
                identity.did(),
            )
            .authorize(request)
            .unwrap();
        }
        let response = request.send().await.unwrap();
        assert_eq!(response.status(), expected);
        if expected == StatusCode::OK {
            assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
            let result: CheckResult = response.json().await.unwrap();
            assert_eq!(
                result.missing.into_iter().collect::<Vec<_>>(),
                ["acme/missing"]
            );
        }
    }
    let request = f
        .client
        .post(format!("{}{CHECK_PATH}", f.endpoint))
        .json(&serde_json::json!({"plugins": ["../search"]}));
    let response = gents::config_client::GraphqlEndpoint::as_principal(
        format!("{}/api/v0/graphql", f.endpoint),
        f.owner.did(),
    )
    .authorize(request)
    .unwrap()
    .send()
    .await
    .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    assert_eq!(f.queries.load(std::sync::atomic::Ordering::SeqCst), 0);
}
