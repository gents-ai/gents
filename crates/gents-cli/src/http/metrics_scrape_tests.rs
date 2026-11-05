use super::*;
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct BindCase {
    bind: String,
    token_configured: bool,
    allowed: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct RequestCase {
    authority_matches: bool,
    token_configured: bool,
    token_presented: bool,
    decision: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Cases {
    binds: Vec<BindCase>,
    requests: Vec<RequestCase>,
}

fn model_cases() -> Cases {
    let stdout = gents_lean_contract::run_prepared_generator(
        &gents_lean_contract::proofs_dir().unwrap(),
        "Proofs.Conformance.MetricsScrapeAccess:olean",
        "Proofs/Conformance/MetricsScrapeAccess.lean",
    )
    .unwrap();
    serde_json::from_str(&stdout).unwrap()
}

fn bind_addr(bind: &str) -> SocketAddr {
    match bind {
        "loopback" => "127.0.0.1:9197",
        "interface" => "100.64.0.7:9197",
        "unspecified" => "0.0.0.0:9197",
        other => panic!("unknown bind {other}"),
    }
    .parse()
    .unwrap()
}

fn stub_router(addr: SocketAddr, token: Option<&str>) -> Router {
    protect(
        Router::new().route("/metrics", axum::routing::get(|| async { "gents_up 1\n" })),
        addr,
        token.map(ToOwned::to_owned),
    )
}

struct Served {
    endpoint: String,
    _server: tokio_util::task::AbortOnDropHandle<()>,
}

async fn serve(router: impl FnOnce(SocketAddr) -> Router) -> Served {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let router = router(addr);
    let server = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    }));
    Served {
        endpoint: format!("http://{addr}"),
        _server: server,
    }
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap()
}

#[test]
fn generated_bind_cases_match_listener_validation() {
    let cases = model_cases();
    assert_eq!(cases.binds.len(), 6);
    for case in cases.binds {
        let addr = bind_addr(&case.bind);
        assert_eq!(
            ScrapeBind::classify(addr.ip()).allowed(case.token_configured),
            case.allowed,
            "{} token={}",
            case.bind,
            case.token_configured
        );
        let token = case.token_configured.then_some("secret");
        assert_eq!(
            validate_listen_addr(addr, token).is_ok(),
            case.allowed,
            "{} token={}",
            case.bind,
            case.token_configured
        );
    }
    for ip in ["::1", "127.0.0.2"] {
        assert_eq!(
            ScrapeBind::classify(ip.parse().unwrap()),
            ScrapeBind::Loopback
        );
    }
    assert_eq!(
        ScrapeBind::classify("::".parse().unwrap()),
        ScrapeBind::Unspecified
    );
}

#[tokio::test]
async fn generated_request_cases_bind_real_requests() {
    let cases = model_cases();
    assert_eq!(cases.requests.len(), 8);
    for case in cases.requests {
        let token = case.token_configured.then_some("scrape-secret");
        let served = serve(|addr| stub_router(addr, token)).await;
        let mut request = client().get(format!("{}/metrics", served.endpoint));
        if case.token_presented {
            request = request.bearer_auth("scrape-secret");
        }
        if !case.authority_matches {
            request = request.header(header::HOST, "attacker.example:9197");
        }
        let response = request.send().await.unwrap();
        let expected = match case.decision.as_str() {
            "misdirected" => StatusCode::MISDIRECTED_REQUEST,
            "unauthorized" => StatusCode::UNAUTHORIZED,
            "metrics" => StatusCode::OK,
            other => panic!("unknown decision {other}"),
        };
        assert_eq!(
            response.status(),
            expected,
            "authority={} configured={} presented={}",
            case.authority_matches,
            case.token_configured,
            case.token_presented
        );
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        let body = response.text().await.unwrap();
        assert_eq!(
            body == "gents_up 1\n",
            case.decision == "metrics",
            "refused requests must not reach the exposition"
        );
    }
}

#[tokio::test]
async fn wrong_or_duplicated_bearer_is_refused() {
    let served = serve(|addr| stub_router(addr, Some("scrape-secret"))).await;
    let url = format!("{}/metrics", served.endpoint);
    for authorization in [
        "Bearer wrong",
        "Bearer scrape-secre",
        "scrape-secret",
        "Basic x",
    ] {
        let response = client()
            .get(&url)
            .header(header::AUTHORIZATION, authorization)
            .send()
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            StatusCode::UNAUTHORIZED,
            "{authorization}"
        );
    }
    let mut headers = HeaderMap::new();
    headers.append(
        header::AUTHORIZATION,
        "Bearer scrape-secret".parse().unwrap(),
    );
    headers.append(header::AUTHORIZATION, "Bearer wrong".parse().unwrap());
    assert!(!token_presented(&headers, "scrape-secret"));
}

fn runtime_state(graphql: &str, node_did: &str) -> crate::http::router::RuntimeHttpState {
    let (runtime, observation) = crate::http::router::empty_activation_state();
    crate::http::runtime_contract_router(
        format!("{graphql}/api/v0/graphql"),
        gents::store::config_client::GraphqlEndpoint::anonymous(format!(
            "{graphql}/api/v0/graphql"
        )),
        "scrape-test".into(),
        node_did.into(),
        "readwrite".into(),
        Ok(None),
        Vec::new(),
        None,
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
    .scrape_state
}

#[tokio::test]
async fn bound_listener_serves_only_the_exposition_without_operator_identity() {
    let home = tempfile::tempdir().unwrap();
    let paths = gents_desktop_core::client::DesktopPaths::from_root(home.path().join("node"));
    paths.ensure_root_dirs().await.unwrap();
    let node = gents_desktop_core::client::PrincipalIdentity::load_or_create(&paths)
        .await
        .unwrap();
    let graphql = serve(|_| {
        Router::new().route(
            "/api/v0/graphql",
            axum::routing::post(|| async { axum::Json(serde_json::json!({ "data": {} })) }),
        )
    })
    .await;
    let listener = MetricsScrapeListener::bind("127.0.0.1:0".parse().unwrap(), None)
        .await
        .unwrap();
    assert!(!listener.auth_required());
    let endpoint = format!("http://{}", listener.addr());
    let (shutdown_tx, shutdown_rx) = watch::channel(false);
    let handle = listener.spawn(runtime_state(&graphql.endpoint, node.did()), shutdown_rx);

    let response = client()
        .get(format!("{endpoint}/metrics"))
        .send()
        .await
        .unwrap();
    let status = response.status();
    let body = response.text().await.unwrap();
    assert_eq!(status, StatusCode::OK, "{body}");
    for family in [
        "gents_up 1",
        "# TYPE gents_p2p_request_dispatch_saturated_total counter",
        "# TYPE process_resident_memory_bytes gauge",
        "# TYPE gents_store_size_bytes gauge",
    ] {
        assert!(body.lines().any(|line| line == family), "{family}");
    }
    for path in [
        "/self",
        "/sessions",
        "/healthz",
        "/status",
        "/api/v0/graphql",
    ] {
        let response = client()
            .get(format!("{endpoint}{path}"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
    }

    shutdown_tx.send_replace(true);
    tokio::time::timeout(std::time::Duration::from_secs(5), handle)
        .await
        .expect("metrics listener stops on shutdown")
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn bind_refuses_unsafe_addresses_before_listening() {
    for (addr, token) in [
        ("0.0.0.0:0", Some("t")),
        ("[::]:0", None),
        ("100.64.0.7:0", None),
    ] {
        let error = MetricsScrapeListener::bind(addr.parse().unwrap(), token.map(Into::into))
            .await
            .err()
            .unwrap_or_else(|| panic!("{addr} must be refused"));
        assert!(error.to_string().starts_with("refusing"), "{addr}: {error}");
    }
}
