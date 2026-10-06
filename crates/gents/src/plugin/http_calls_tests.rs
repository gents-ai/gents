//! Host-mediated plugin HTTP against a loopback server, fake resolvers and
//! WAT plugins.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;

use serde_json::{json, Value};

use super::*;
use crate::plugin::executor::PluginExecutor;
use crate::plugin::store;
use crate::plugin::tests::executor::{asking_plugin_wat, installed_plugin};
use crate::plugin::PluginVerdict;

const PLUGIN: &str = "team/plugin";

/// A loopback HTTP server: `/hello`, redirects, an oversized body, a slow
/// answer, a cookie and an echo of the request's credentials headers.
struct Server {
    port: u16,
    hits: Arc<Mutex<Vec<String>>>,
}

impl Server {
    async fn start() -> Self {
        use axum::extract::{Path, State};
        use axum::http::{HeaderMap, StatusCode};
        use axum::response::IntoResponse;
        use axum::routing::get;

        type Hits = Arc<Mutex<Vec<String>>>;
        async fn handle(
            State(hits): State<Hits>,
            Path(path): Path<String>,
            headers: HeaderMap,
        ) -> axum::response::Response {
            hits.lock().unwrap().push(path.clone());
            let header = |name: &str| {
                headers
                    .get(name)
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned)
            };
            if let Some(to) = path.strip_prefix("redirect/") {
                let to = to.replacen('/', "://", 1);
                return (StatusCode::FOUND, [("location", to)], String::new()).into_response();
            }
            match path.as_str() {
                "hello" => (StatusCode::OK, [("x-served", "yes")], "hello").into_response(),
                "loop" => {
                    (StatusCode::FOUND, [("location", "/loop")], String::new()).into_response()
                }
                "big" => "x".repeat(MAX_RESPONSE_BYTES + 1).into_response(),
                "slow" => {
                    tokio::time::sleep(Duration::from_secs(5)).await;
                    "late".into_response()
                }
                "cookie" => (StatusCode::OK, [("set-cookie", "session=1")], "set").into_response(),
                "headers" => json!({
                    "cookie": header("cookie"),
                    "authorization": header("authorization"),
                    "x-plugin": header("x-plugin"),
                })
                .to_string()
                .into_response(),
                _ => StatusCode::NOT_FOUND.into_response(),
            }
        }
        let hits: Hits = Arc::default();
        let app = axum::Router::new()
            .route("/{*path}", get(handle).post(handle))
            .with_state(hits.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self { port, hits }
    }

    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}/{path}", self.port)
    }

    /// The explicit grant of this server's loopback literal.
    fn literal(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    fn hits(&self) -> Vec<String> {
        self.hits.lock().unwrap().clone()
    }
}

/// Answers every name with `addrs`, counting lookups.
struct Static(Vec<IpAddr>, Arc<AtomicUsize>);

impl Lookup for Static {
    fn lookup(&self, _: String) -> BoxFuture<'static, std::io::Result<Vec<IpAddr>>> {
        self.1.fetch_add(1, Ordering::SeqCst);
        let addrs = self.0.clone();
        Box::pin(async move { Ok(addrs) })
    }
}

fn http_manifold(hosts: Option<Vec<String>>) -> Manifold {
    Manifold {
        net: NetAccess::OutboundHttp(hosts),
        ..Manifold::sealed()
    }
}

fn session(hosts: &[String]) -> Session {
    Session::for_grant(PLUGIN, &http_manifold(Some(hosts.to_vec())))
        .unwrap()
        .unwrap()
}

fn session_resolving(hosts: &[String], addrs: Vec<IpAddr>) -> (Session, Arc<AtomicUsize>) {
    let lookups = Arc::new(AtomicUsize::new(0));
    let session = Session::with_lookup(
        PLUGIN,
        &http_manifold(Some(hosts.to_vec())),
        Arc::new(Static(addrs, lookups.clone())),
    )
    .unwrap()
    .unwrap();
    (session, lookups)
}

async fn ask(session: &mut Session, requests: Value) -> Map<String, Value> {
    let batch = parse_batch(&json!({"http_calls": {"requests": requests}}))
        .unwrap()
        .unwrap();
    session
        .serve(batch.requests, Instant::now() + Duration::from_secs(20))
        .await
}

fn error(results: &Map<String, Value>, id: &str) -> String {
    results[id]["error"]
        .as_str()
        .unwrap_or_else(|| panic!("{id} was answered: {}", results[id]))
        .to_owned()
}

#[test]
fn generated_plugin_network_cases_drive_admission() {
    let cases = &crate::lean_vocab_test::lean_contract_snapshot().plugin_resource_cases;
    let cases = cases["network"].as_array().unwrap();
    assert!(!cases.is_empty());
    for case in cases {
        let net = match &case["grant"] {
            Value::Null => NetAccess::None,
            Value::String(any) if any == "any" => NetAccess::OutboundHttp(None),
            hosts => NetAccess::OutboundHttp(Some(serde_json::from_value(hosts.clone()).unwrap())),
        };
        let grant = net_grant(&net).unwrap();
        let url = Url::parse(case["url"].as_str().unwrap()).unwrap();
        let addrs: Vec<IpAddr> = serde_json::from_value(case["addresses"].clone()).unwrap();
        let public: Vec<bool> = serde_json::from_value(case["public"].clone()).unwrap();
        assert_eq!(
            addrs.iter().map(ip_public).collect::<Vec<_>>(),
            public,
            "{case}"
        );
        let admitted = admit(&grant, &url, PLUGIN);
        if let Ok(admission) = &admitted {
            if let Some(literal) = admission.literal {
                assert_eq!(addrs, [literal], "{case}");
            }
        }
        let allowed = admitted.is_ok_and(|admission| addresses_allowed(admission.explicit, &addrs));
        assert_eq!(allowed, case["expected"].as_bool().unwrap(), "{case}");
    }
}

#[test]
fn ipv6_internal_ranges_and_embedded_ipv4_are_judged() {
    for internal in [
        "::1",
        "::",
        "::ffff:127.0.0.1",
        "::ffff:10.0.0.1",
        "fe80::1",
        "fc00::1",
        "fd12::1",
        "ff02::1",
        "2001:db8::1",
        "2002:7f00:1::",
        "64:ff9b::7f00:1",
    ] {
        assert!(!ip_public(&internal.parse().unwrap()), "{internal}");
    }
    for public in [
        "2606:4700::1111",
        "::ffff:93.184.216.34",
        "2002:5db8:d822::",
    ] {
        assert!(ip_public(&public.parse().unwrap()), "{public}");
    }
}

#[test]
fn allow_list_entries_parse_and_unservable_grants_are_refused() {
    let grant = net_grant(&NetAccess::OutboundHttp(Some(vec![
        "API.Example.com".into(),
        "*.example.org".into(),
        "http://[::1]:8080".into(),
        "https://10.0.0.1:8443".into(),
    ])))
    .unwrap();
    let NetGrant::Hosts(entries) = grant else {
        panic!("{grant:?}")
    };
    assert_eq!(
        entries[0].pattern,
        HostPattern::Exact(TargetHost::Name("api.example.com".into()))
    );
    assert_eq!(
        entries[1].pattern,
        HostPattern::Suffix("example.org".into())
    );
    assert_eq!((entries[2].plaintext, entries[2].port), (true, Some(8080)));
    assert_eq!((entries[3].plaintext, entries[3].port), (false, Some(8443)));
    for bad in [
        "*.1.2.3.4",
        "example.com/path",
        "user@example.com",
        "*",
        "",
        "a.com:0",
    ] {
        assert!(
            net_grant(&NetAccess::OutboundHttp(Some(vec![bad.into()]))).is_err(),
            "{bad}"
        );
    }
    let full = net_grant(&NetAccess::OutboundFull(None)).unwrap_err();
    assert!(full.contains("OutboundHttp"), "{full}");
    assert_eq!(
        net_grant(&NetAccess::OutboundHttp(Some(Vec::new()))).unwrap(),
        NetGrant::Sealed
    );
}

#[test]
fn a_pack_cannot_declare_a_network_grant_the_host_cannot_serve() {
    let mut plugin: crate::pack::PackPlugin = serde_json::from_value(json!({
        "name": "fetcher",
        "description": "Fetches",
        "artifact": "plugins/fetcher.afb",
        "language": "rust",
        "input_schema": {"type": "object"},
    }))
    .unwrap();
    for (net, timeout) in [
        (json!({"OutboundFull": null}), Value::Null),
        (json!({"OutboundHttp": ["*.10.0.0.1"]}), Value::Null),
        (json!({"OutboundHttp": ["api.example.com"]}), json!(0)),
    ] {
        plugin.manifold = Some(
            json!({"fs": "None", "net": net, "env": "None", "crypto": false, "child_process": false, "http_timeout_ms": timeout}),
        );
        let error = plugin.validate().unwrap_err();
        assert!(
            format!("{error:#}").contains("network grant the host cannot serve"),
            "{error:#}"
        );
    }
    plugin.manifold = Some(
        json!({"fs": "None", "net": {"OutboundHttp": ["api.example.com"]}, "env": "None", "crypto": false, "child_process": false}),
    );
    plugin.validate().unwrap();
}

#[test]
fn a_sealed_or_empty_grant_offers_no_network() {
    assert!(Session::for_grant(PLUGIN, &Manifold::sealed())
        .unwrap()
        .is_none());
    assert!(Session::for_grant(PLUGIN, &http_manifold(Some(Vec::new())))
        .unwrap()
        .is_none());
}

/// `team/plugin` asking once for `requests`, declaring `net`, under a fresh
/// home; granted with consent when `grant`.
fn installed_asking(
    requests: Value,
    net: Value,
    grant: bool,
) -> (tempfile::TempDir, store::InstalledPlugin) {
    let canned = json!({"http_calls": {"requests": requests, "state": {"step": 1}}});
    let (home, mut record) = installed_plugin(
        &asking_plugin_wat("http_calls", "http_results", &canned, false),
        None,
    );
    record.declaration.manifold = Some(
        json!({"fs": "None", "net": net, "env": "None", "crypto": false, "child_process": false}),
    );
    record.granted = if grant {
        store::grant_on_install(home.path(), &record.namespace, &record.declaration, true).unwrap()
    } else {
        None
    };
    store::write_record(home.path(), &record).unwrap();
    (home, record)
}

#[tokio::test]
async fn an_allow_listed_request_reaches_the_server_only_when_granted() {
    let server = Server::start().await;
    let requests = json!([{"id": "a", "url": server.url("hello")}]);
    let net = json!({"OutboundHttp": [server.literal()]});

    let (home, record) = installed_asking(requests.clone(), net.clone(), true);
    let executor = PluginExecutor::new(Some(home.path().to_owned()));
    let call = executor.call(&record, json!({"n": 1})).await.unwrap();
    assert_eq!(
        call.outcome.verdict,
        PluginVerdict::Success,
        "{}",
        call.outcome.diagnostics
    );
    let answer = &call.outcome.output["http_results"]["a"];
    assert_eq!(answer["status"], 200, "{answer}");
    assert_eq!(answer["body"], "hello");
    assert_eq!(answer["headers"]["x-served"], "yes");
    assert_eq!(call.outcome.output["state"], json!({"step": 1}));
    assert_eq!(call.outcome.output["n"], 1);
    assert_eq!(server.hits(), ["hello"]);

    // Declared but not granted: the plugin runs sealed and is never offered
    // the network, so its input reaches it untouched.
    let (home, record) = installed_asking(requests, net, false);
    let call = PluginExecutor::new(Some(home.path().to_owned()))
        .call(&record, json!({"n": 1}))
        .await
        .unwrap();
    assert_eq!(call.outcome.verdict, PluginVerdict::Success);
    assert_eq!(call.outcome.output, json!({"n": 1}));
    assert_eq!(server.hits(), ["hello"]);
}

#[test]
fn granting_a_network_allow_list_needs_grant_authority() {
    let (home, record) = installed_asking(
        json!([]),
        json!({"OutboundHttp": ["api.example.com"]}),
        false,
    );
    let error = store::grant_on_install(home.path(), &record.namespace, &record.declaration, false)
        .unwrap_err();
    let error = format!("{error:#}");
    assert!(
        error.contains("HTTP to api.example.com") && error.contains("--grant-authority"),
        "{error}"
    );
}

#[tokio::test]
async fn a_host_outside_the_allow_list_is_refused_with_the_next_step() {
    let server = Server::start().await;
    let mut session = session(&["api.example.com".into()]);
    let results = ask(
        &mut session,
        json!([{"id": "a", "url": server.url("hello")}]),
    )
    .await;
    let error = error(&results, "a");
    assert!(
        error.contains("is not in plugin team/plugin's granted network allow-list")
            && error.contains("--grant-authority"),
        "{error}"
    );
    // Plaintext and other ports need their own entry.
    let results = ask(
        &mut session,
        json!([
            {"id": "plain", "url": "http://api.example.com/"},
            {"id": "port", "url": "https://api.example.com:8443/"},
        ]),
    )
    .await;
    assert!(error_of(&results, "plain").contains("not in plugin"));
    assert!(error_of(&results, "port").contains("not in plugin"));
    assert!(server.hits().is_empty());
}

fn error_of(results: &Map<String, Value>, id: &str) -> String {
    error(results, id)
}

#[tokio::test]
async fn loopback_and_private_targets_are_refused_unless_named_literally() {
    let server = Server::start().await;
    // A name that resolves to loopback, through the system resolver.
    let mut named = session(&[format!("http://localhost:{}", server.port)]);
    let results = ask(
        &mut named,
        json!([{"id": "a", "url": format!("http://localhost:{}/hello", server.port)}]),
    )
    .await;
    let refusal = error(&results, "a");
    assert!(
        refusal.contains("resolves to the internal address"),
        "{refusal}"
    );

    // Any host means any public host.
    let mut any = Session::for_grant(PLUGIN, &http_manifold(None))
        .unwrap()
        .unwrap();
    let results = ask(
        &mut any,
        json!([
            {"id": "loopback", "url": "https://127.0.0.1/"},
            {"id": "private", "url": "https://10.0.0.1/"},
            {"id": "metadata", "url": "https://169.254.169.254/latest/meta-data"},
            {"id": "mapped", "url": "https://[::ffff:127.0.0.1]/"},
        ]),
    )
    .await;
    for id in ["loopback", "private", "metadata", "mapped"] {
        let refusal = error(&results, id);
        assert!(refusal.contains("internal address"), "{id}: {refusal}");
    }
    assert!(server.hits().is_empty());
}

#[tokio::test]
async fn a_rebinding_name_is_checked_on_the_addresses_the_connection_uses() {
    let server = Server::start().await;
    let grant = [format!("http://rebind.test:{}", server.port)];
    let url = format!("http://rebind.test:{}/hello", server.port);
    let (mut session, lookups) = session_resolving(&grant, vec!["127.0.0.1".parse().unwrap()]);
    let results = ask(&mut session, json!([{"id": "a", "url": url}])).await;
    let refusal = error(&results, "a");
    assert!(
        refusal.contains("rebind.test resolves to the internal address 127.0.0.1"),
        "{refusal}"
    );
    // One resolution, and it is the one that was checked.
    assert_eq!(lookups.load(Ordering::SeqCst), 1);

    let (mut mixed, _) = session_resolving(
        &grant,
        vec![
            "93.184.216.34".parse().unwrap(),
            "10.0.0.5".parse().unwrap(),
        ],
    );
    let results = ask(&mut mixed, json!([{"id": "a", "url": url}])).await;
    assert!(error(&results, "a").contains("internal address 10.0.0.5"));
    assert!(server.hits().is_empty());
}

#[tokio::test]
async fn a_redirect_is_admitted_again_at_every_hop() {
    let server = Server::start().await;
    let port = server.port;
    let (mut session, _) = session_resolving(
        &[server.literal(), format!("http://internal.test:{port}")],
        vec!["127.0.0.1".parse().unwrap()],
    );
    let results = ask(
        &mut session,
        json!([
            {"id": "followed", "url": server.url(&format!("redirect/http/127.0.0.1:{port}/hello"))},
            {"id": "to_private_name", "url": server.url(&format!("redirect/http/internal.test:{port}/hello"))},
            {"id": "to_private_literal", "url": server.url("redirect/http/10.0.0.1/hello")},
            {"id": "loop", "url": server.url("loop")},
        ]),
    )
    .await;
    assert_eq!(
        results["followed"]["status"], 200,
        "{}",
        results["followed"]
    );
    assert_eq!(results["followed"]["url"], server.url("hello"));
    assert!(error(&results, "to_private_name")
        .contains("internal.test resolves to the internal address"));
    assert!(error(&results, "to_private_literal").contains("not in plugin"));
    assert!(error(&results, "loop").contains("redirected more than"));
    let hits = server.hits();
    assert_eq!(
        hits.iter().filter(|hit| *hit == "hello").count(),
        1,
        "{hits:?}"
    );
    assert_eq!(
        hits.iter().filter(|hit| *hit == "loop").count(),
        MAX_REDIRECTS + 1
    );
}

#[tokio::test]
async fn caps_bound_bodies_requests_and_time() {
    let server = Server::start().await;
    let mut session = Session::for_grant(
        PLUGIN,
        &Manifold {
            http_timeout_ms: Some(300),
            ..http_manifold(Some(vec![server.literal()]))
        },
    )
    .unwrap()
    .unwrap();
    let results = ask(
        &mut session,
        json!([
            {"id": "big", "url": server.url("big")},
            {"id": "slow", "url": server.url("slow")},
        ]),
    )
    .await;
    assert!(error(&results, "big").contains("larger than"));
    assert!(error(&results, "slow").contains("timed out"));

    session.max_requests = session.requests_sent + 1;
    let results = ask(
        &mut session,
        json!([
            {"id": "a", "url": server.url("hello")},
            {"id": "b", "url": server.url("hello")},
        ]),
    )
    .await;
    let answered = ["a", "b"]
        .iter()
        .filter(|id| results[**id].get("status").is_some())
        .count();
    assert_eq!(answered, 1, "{results:?}");

    let mut session = session_bytes(&server, 8);
    let results = ask(
        &mut session,
        json!([{"id": "a", "url": server.url("hello")}]),
    )
    .await;
    assert_eq!(results["a"]["status"], 200);
    let results = ask(
        &mut session,
        json!([{"id": "b", "url": server.url("hello")}]),
    )
    .await;
    assert!(error(&results, "b").contains("response budget"));

    let too_many: Vec<Value> = (0..=MAX_REQUESTS_PER_ROUND)
        .map(|n| json!({"id": n.to_string(), "url": server.url("hello")}))
        .collect();
    let error = parse_batch(&json!({"http_calls": {"requests": too_many}}))
        .err()
        .unwrap();
    assert!(error.contains("at most"), "{error}");
    let body = "x".repeat(MAX_REQUEST_BODY_BYTES + 1);
    let error = parse_batch(&json!({"http_calls": {"requests": [{"id": "a", "method": "POST", "url": server.url("hello"), "body": body}]}}))
        .err()
        .unwrap();
    assert!(error.contains("larger than"), "{error}");
}

/// Concurrent requests share one budget, and a response refused for its
/// size is charged for what was read of it.
#[tokio::test]
async fn the_response_budget_is_charged_as_bytes_are_read() {
    let server = Server::start().await;
    let mut session = session_bytes(&server, 8);
    let results = ask(
        &mut session,
        json!([
            {"id": "a", "url": server.url("hello")},
            {"id": "b", "url": server.url("hello")},
        ]),
    )
    .await;
    let answered = ["a", "b"]
        .iter()
        .filter(|id| results[**id].get("status").is_some())
        .count();
    assert_eq!(answered, 1, "{results:?}");

    let mut session = session_bytes(&server, MAX_RESPONSE_BYTES);
    let results = ask(
        &mut session,
        json!([{"id": "big", "url": server.url("big")}]),
    )
    .await;
    error(&results, "big");
    let results = ask(
        &mut session,
        json!([{"id": "a", "url": server.url("hello")}]),
    )
    .await;
    assert!(error(&results, "a").contains("was not sent"));
    assert_eq!(server.hits(), ["hello", "hello", "big"]);
}

fn session_bytes(server: &Server, max_response_bytes: usize) -> Session {
    let mut session = session(&[server.literal()]);
    session.max_response_bytes = max_response_bytes;
    session
}

#[tokio::test]
async fn requests_carry_only_what_the_plugin_sets() {
    let server = Server::start().await;
    let mut session = session(&[server.literal()]);
    let results = ask(
        &mut session,
        json!([{"id": "a", "url": server.url("cookie")}]),
    )
    .await;
    assert_eq!(results["a"]["headers"]["set-cookie"], "session=1");
    let results = ask(
        &mut session,
        json!([{"id": "b", "url": server.url("headers"), "headers": {"x-plugin": "1"}}]),
    )
    .await;
    let seen: Value = serde_json::from_str(results["b"]["body"].as_str().unwrap()).unwrap();
    assert_eq!(
        seen,
        json!({"cookie": null, "authorization": null, "x-plugin": "1"})
    );
}

#[test]
fn malformed_http_calls_are_named() {
    for (request, why) in [
        (json!({"url": "https://a.test/"}), "non-empty string id"),
        (json!({"id": "a"}), "needs a url"),
        (
            json!({"id": "a", "url": "https://a.test/", "method": "CONNECT"}),
            "use GET",
        ),
        (
            json!({"id": "a", "url": "https://a.test/", "headers": {"Host": "b"}}),
            "set by the host",
        ),
        (
            json!({"id": "a", "url": "https://a.test/", "headers": {"proxy-authorization": "b"}}),
            "set by the host",
        ),
        (
            json!({"id": "a", "url": "https://a.test/", "body": "x", "body_base64": "eA=="}),
            "one string body",
        ),
    ] {
        let error = parse_batch(&json!({"http_calls": {"requests": [request]}}))
            .err()
            .unwrap_or_else(|| panic!("{why}"));
        assert!(error.contains(why), "{error}");
    }
    assert!(parse_batch(&json!({"http_calls": true})).unwrap().is_none());
}

#[tokio::test]
async fn a_round_asking_both_services_is_bad_output() {
    let canned = json!({
        "http_calls": {"requests": []},
        "model_calls": {"requests": []},
    });
    let (home, mut record) = installed_plugin(
        &asking_plugin_wat("http_calls", "http_results", &canned, false),
        None,
    );
    record.declaration.manifold = Some(
        json!({"fs": "None", "net": {"OutboundHttp": ["api.example.com"]}, "env": "None", "crypto": false, "child_process": false}),
    );
    record.declaration.model_slot = Some("slot".into());
    record.model_binding = Some(crate::plugin::model_calls::ModelBinding {
        agent_did: "did:key:owner".into(),
        profile_id: "p".into(),
    });
    record.granted =
        store::grant_on_install(home.path(), &record.namespace, &record.declaration, true).unwrap();
    store::write_record(home.path(), &record).unwrap();
    let call = PluginExecutor::new(Some(home.path().to_owned()))
        .with_models(Arc::new(Endpoint))
        .call(&record, json!({}))
        .await
        .unwrap();
    assert_eq!(call.outcome.verdict, PluginVerdict::BadOutput);
    assert!(call.outcome.diagnostics.contains("not both"));
}

struct Endpoint;

impl crate::plugin::model_calls::ModelResolver for Endpoint {
    fn resolve<'a>(
        &'a self,
        _: &'a crate::plugin::model_calls::ModelBinding,
    ) -> BoxFuture<'a, Result<crate::plugin::model_calls::ModelEndpoint>> {
        Box::pin(async {
            Ok(crate::plugin::model_calls::ModelEndpoint {
                url: "http://127.0.0.1:9/v1/chat/completions".into(),
                api_key: None,
                model: "m".into(),
                max_concurrent: 1,
                backend_key: "test".into(),
                connect_timeout: Duration::from_secs(1),
                request_timeout: Duration::from_secs(1),
                max_output_tokens: None,
            })
        })
    }
}
