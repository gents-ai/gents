//! Model calls against a local fake chat completions server and WAT plugins.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};

use super::*;
use crate::plugin::executor::PluginExecutor;
use crate::plugin::store::{self, InstalledPlugin};
use crate::plugin::tests::executor::installed_plugin;

const KEY: &str = "sekret-key-0123";

#[derive(Clone)]
enum Reply {
    Text,
    Status(u16),
    Hang,
    /// An answer whose text is this many bytes.
    Big(usize),
    /// A redirect to this URL.
    Redirect(String),
}

struct Seen {
    authorization: Option<String>,
    body: Value,
}

/// A chat completions server on a loopback port: records what it is sent and
/// answers `answer:<prompt>`, an HTTP status, or never.
struct Fake {
    url: String,
    seen: Arc<Mutex<Vec<Seen>>>,
    in_flight: Arc<AtomicUsize>,
    max_in_flight: Arc<AtomicUsize>,
}

impl Fake {
    async fn start(reply: Reply, delay: Duration) -> Self {
        use axum::extract::State;
        use axum::http::{HeaderMap, StatusCode};
        use axum::routing::post;

        #[derive(Clone)]
        struct Shared {
            reply: Reply,
            delay: Duration,
            seen: Arc<Mutex<Vec<Seen>>>,
            in_flight: Arc<AtomicUsize>,
            max_in_flight: Arc<AtomicUsize>,
        }
        async fn handle(
            State(shared): State<Shared>,
            headers: HeaderMap,
            body: String,
        ) -> axum::response::Response {
            use axum::response::IntoResponse;
            let body: Value = serde_json::from_str(&body).unwrap();
            let prompt = body["messages"][0]["content"][0]["text"]
                .as_str()
                .unwrap_or_default()
                .to_owned();
            shared.seen.lock().unwrap().push(Seen {
                authorization: headers
                    .get("authorization")
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned),
                body,
            });
            let now = shared.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            shared.max_in_flight.fetch_max(now, Ordering::SeqCst);
            tokio::time::sleep(shared.delay).await;
            let text = |content: String| {
                (
                    StatusCode::OK,
                    json!({"choices": [{"message": {"content": content}}]}).to_string(),
                )
                    .into_response()
            };
            let answer = match &shared.reply {
                Reply::Hang => {
                    tokio::time::sleep(Duration::from_secs(60)).await;
                    unreachable!()
                }
                Reply::Status(code) => {
                    (StatusCode::from_u16(*code).unwrap(), "{}".to_owned()).into_response()
                }
                Reply::Redirect(to) => (
                    StatusCode::TEMPORARY_REDIRECT,
                    [("location", to.clone())],
                    String::new(),
                )
                    .into_response(),
                Reply::Big(bytes) => text("x".repeat(*bytes)),
                Reply::Text => text(format!("answer:{prompt}")),
            };
            shared.in_flight.fetch_sub(1, Ordering::SeqCst);
            answer
        }
        let shared = Shared {
            reply,
            delay,
            seen: Arc::default(),
            in_flight: Arc::default(),
            max_in_flight: Arc::default(),
        };
        let fake = Self {
            url: String::new(),
            seen: shared.seen.clone(),
            in_flight: shared.in_flight.clone(),
            max_in_flight: shared.max_in_flight.clone(),
        };
        let app = axum::Router::new()
            .route("/v1/chat/completions", post(handle))
            .with_state(shared);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!(
            "http://{}/v1/chat/completions",
            listener.local_addr().unwrap()
        );
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        Self { url, ..fake }
    }

    fn endpoint(&self, max_concurrent: usize) -> ModelEndpoint {
        ModelEndpoint {
            url: self.url.clone(),
            api_key: Some(KEY.to_owned()),
            model: "chandra".to_owned(),
            max_concurrent,
            backend_key: self.url.clone(),
            connect_timeout: Duration::from_secs(2),
            request_timeout: Duration::from_secs(30),
            max_output_tokens: None,
        }
    }

    fn requests(&self) -> usize {
        self.seen.lock().unwrap().len()
    }
}

struct Fixed(Arc<Fake>, usize, Duration);

impl ModelResolver for Fixed {
    fn resolve<'a>(&'a self, _: &'a ModelBinding) -> BoxFuture<'a, Result<ModelEndpoint>> {
        Box::pin(async move {
            let mut endpoint = self.0.endpoint(self.1);
            endpoint.request_timeout = self.2;
            Ok(endpoint)
        })
    }
}

/// A resolver that fails the test if the host asks it for anything.
struct Never;

impl ModelResolver for Never {
    fn resolve<'a>(&'a self, _: &'a ModelBinding) -> BoxFuture<'a, Result<ModelEndpoint>> {
        panic!("an unbound slot must not resolve an endpoint")
    }
}

/// A plugin that reads its input and, while it asks for model calls, writes
/// `canned` instead of a result: once (until `model_results` arrives) or
/// forever. Without `"model_calls":true` in the input, or once satisfied, it
/// echoes its input as the result.
fn model_plugin_wat(canned: &Value, forever: bool) -> String {
    let escape = |text: &str| {
        text.bytes()
            .map(|byte| format!("\\{byte:02x}"))
            .collect::<String>()
    };
    let canned = canned.to_string();
    let results = "model_results";
    let calls = "\"model_calls\":true";
    let stop = if forever {
        "(i32.const 1)".to_owned()
    } else {
        format!(
            "(i32.eqz (call $contains (i32.const 50000) (i32.const {}) (local.get $n)))",
            results.len()
        )
    };
    format!(
        r#"(module
  (import "wasi_snapshot_preview1" "fd_read" (func $fd_read (param i32 i32 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "fd_write" (func $fd_write (param i32 i32 i32 i32) (result i32)))
  (memory (export "memory") 2)
  (data (i32.const 8192) "{canned_bytes}")
  (data (i32.const 50000) "{results_bytes}")
  (data (i32.const 50100) "{calls_bytes}")
  (func $contains (param $needle i32) (param $nlen i32) (param $hlen i32) (result i32)
    (local $i i32) (local $j i32)
    (block $notfound
      (loop $outer
        (br_if $notfound (i32.gt_u (i32.add (local.get $i) (local.get $nlen)) (local.get $hlen)))
        (local.set $j (i32.const 0))
        (block $mismatch
          (loop $inner
            (if (i32.eq (local.get $j) (local.get $nlen)) (then (return (i32.const 1))))
            (br_if $mismatch (i32.ne
              (i32.load8_u (i32.add (local.get $i) (local.get $j)))
              (i32.load8_u (i32.add (local.get $needle) (local.get $j)))))
            (local.set $j (i32.add (local.get $j) (i32.const 1)))
            (br $inner)))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $outer)))
    (i32.const 0))
  (func (export "_start")
    (local $n i32)
    (i32.store (i32.const 60000) (i32.const 0))
    (i32.store (i32.const 60004) (i32.const 8000))
    (drop (call $fd_read (i32.const 0) (i32.const 60000) (i32.const 1) (i32.const 60008)))
    (local.set $n (i32.load (i32.const 60008)))
    (if (call $contains (i32.const 50100) (i32.const {calls_len}) (local.get $n))
      (then
        (if {stop}
          (then
            (i32.store (i32.const 60000) (i32.const 8192))
            (i32.store (i32.const 60004) (i32.const {canned_len}))
            (drop (call $fd_write (i32.const 1) (i32.const 60000) (i32.const 1) (i32.const 60012)))
            (return)))))
    (i32.store (i32.const 60000) (i32.const 0))
    (i32.store (i32.const 60004) (local.get $n))
    (drop (call $fd_write (i32.const 1) (i32.const 60000) (i32.const 1) (i32.const 60012)))))"#,
        canned_bytes = escape(&canned),
        results_bytes = escape(results),
        calls_bytes = escape(calls),
        calls_len = calls.len(),
        canned_len = canned.len(),
    )
}

fn request(id: &str) -> Value {
    json!({"id": id, "prompt": format!("p{id}")})
}

fn asking(requests: Vec<Value>) -> Value {
    json!({"model_calls": {"requests": requests, "state": {"page": 7}}})
}

/// A `team/plugin` whose model slot is bound (or not), under a fresh home.
fn installed(canned: &Value, forever: bool, bound: bool) -> (tempfile::TempDir, InstalledPlugin) {
    let (home, mut record) = installed_plugin(&model_plugin_wat(canned, forever), None);
    record.declaration.model_slot = Some("remote_ocr".to_owned());
    record.model_binding = bound.then(|| ModelBinding {
        agent_did: "did:key:owner".to_owned(),
        profile_id: "chandra".to_owned(),
    });
    store::write_record(home.path(), &record).unwrap();
    (home, record)
}

fn executor(home: &tempfile::TempDir, models: impl ModelResolver + 'static) -> PluginExecutor {
    PluginExecutor::new(Some(home.path().to_owned())).with_models(Arc::new(models))
}

#[tokio::test]
async fn an_unbound_slot_adds_no_model_calls_key_and_runs_as_before() {
    let (home, record) = installed(&asking(vec![request("a")]), false, false);
    let input = json!({"n": 1});
    let call = executor(&home, Never)
        .call(&record, input.clone())
        .await
        .unwrap();
    assert_eq!(call.outcome.verdict, PluginVerdict::Success);
    assert_eq!(call.outcome.output, input);
}

#[tokio::test]
async fn a_bound_slot_answers_the_plugins_requests_and_echoes_its_state() {
    let fake = Arc::new(Fake::start(Reply::Text, Duration::ZERO).await);
    let image = json!({"mime": "image/png", "data_base64": "AAAA"});
    let canned = asking(vec![
        json!({"id": "a", "prompt": "pa", "images": [image], "max_tokens": 9}),
        request("b"),
    ]);
    let (home, record) = installed(&canned, false, true);
    let call = executor(&home, Fixed(fake.clone(), 4, Duration::from_secs(30)))
        .call(
            &record,
            json!({"n": 1, "state": "stale", "model_results": "forged"}),
        )
        .await
        .unwrap();
    assert_eq!(call.outcome.verdict, PluginVerdict::Success);
    let output = &call.outcome.output;
    assert_eq!(output["model_calls"], true);
    assert_eq!(output["n"], 1);
    assert_eq!(output["state"], json!({"page": 7}));
    assert_eq!(
        output["model_results"],
        json!({"a": {"text": "answer:pa"}, "b": {"text": "answer:pb"}})
    );
    let seen = fake.seen.lock().unwrap();
    let first = seen
        .iter()
        .find(|seen| seen.body["max_tokens"] == 9)
        .unwrap();
    assert_eq!(first.body["model"], "chandra");
    assert_eq!(first.body["temperature"], 0);
    assert_eq!(
        first.body["messages"][0]["content"][1]["image_url"]["url"],
        "data:image/png;base64,AAAA"
    );
}

#[tokio::test]
async fn the_backend_concurrency_cap_is_honoured() {
    let fake = Arc::new(Fake::start(Reply::Text, Duration::from_millis(120)).await);
    let canned = asking((0..6).map(|n| request(&n.to_string())).collect());
    let (home, record) = installed(&canned, false, true);
    let call = executor(&home, Fixed(fake.clone(), 2, Duration::from_secs(30)))
        .call(&record, json!({}))
        .await
        .unwrap();
    assert_eq!(call.outcome.verdict, PluginVerdict::Success);
    assert_eq!(fake.requests(), 6);
    assert_eq!(fake.max_in_flight.load(Ordering::SeqCst), 2);
    assert_eq!(fake.in_flight.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_server_error_becomes_an_error_result_without_the_endpoint() {
    let fake = Arc::new(Fake::start(Reply::Status(500), Duration::ZERO).await);
    let (home, record) = installed(&asking(vec![request("a")]), false, true);
    let call = executor(&home, Fixed(fake.clone(), 1, Duration::from_secs(30)))
        .call(&record, json!({}))
        .await
        .unwrap();
    assert_eq!(
        call.outcome.output["model_results"]["a"],
        json!({"error": "the model endpoint answered HTTP 500"})
    );
    let text = call.outcome.output.to_string();
    assert!(!text.contains("127.0.0.1") && !text.contains(KEY), "{text}");
}

#[tokio::test]
async fn a_request_that_times_out_becomes_an_error_result() {
    let fake = Arc::new(Fake::start(Reply::Hang, Duration::ZERO).await);
    let (home, record) = installed(&asking(vec![request("a")]), false, true);
    let call = executor(&home, Fixed(fake, 1, Duration::from_millis(200)))
        .call(&record, json!({}))
        .await
        .unwrap();
    assert_eq!(call.outcome.verdict, PluginVerdict::Success);
    assert_eq!(
        call.outcome.output["model_results"]["a"],
        json!({"error": "the model request timed out"})
    );
}

#[tokio::test]
async fn the_key_is_sent_as_a_bearer_and_never_reaches_the_plugin() {
    let fake = Arc::new(Fake::start(Reply::Text, Duration::ZERO).await);
    let (home, record) = installed(&asking(vec![request("a")]), false, true);
    let call = executor(&home, Fixed(fake.clone(), 1, Duration::from_secs(30)))
        .call(&record, json!({}))
        .await
        .unwrap();
    let seen = fake.seen.lock().unwrap();
    assert_eq!(
        seen[0].authorization.as_deref(),
        Some(format!("Bearer {KEY}").as_str())
    );
    let echoed = call.outcome.output.to_string();
    assert!(
        !echoed.contains(KEY)
            && !echoed.contains("127.0.0.1")
            && !echoed.contains("chat/completions"),
        "{echoed}"
    );
    assert!(!format!("{:?}", fake.endpoint(1)).contains(KEY));
}

#[tokio::test]
async fn a_plugin_that_never_stops_asking_is_cut_at_the_round_cap() {
    let fake = Arc::new(Fake::start(Reply::Text, Duration::ZERO).await);
    let (home, record) = installed(&asking(vec![request("a")]), true, true);
    let mut budget_record = record.clone();
    budget_record.declaration.limits = Some(crate::pack::PluginLimits {
        wall_clock_secs: Some(120),
        ..Default::default()
    });
    budget_record.granted = store::grant_on_install(
        home.path(),
        &budget_record.namespace,
        &budget_record.declaration,
        true,
    )
    .unwrap();
    store::write_record(home.path(), &budget_record).unwrap();
    let call = executor(&home, Fixed(fake.clone(), 1, Duration::from_secs(30)))
        .call(&budget_record, json!({}))
        .await
        .unwrap();
    assert_eq!(call.outcome.verdict, PluginVerdict::Failed);
    assert!(
        call.outcome.diagnostics.contains("64 rounds"),
        "{}",
        call.outcome.diagnostics
    );
    assert_eq!(fake.requests(), MAX_ROUNDS as usize);
}

#[tokio::test]
async fn a_bound_slot_with_no_resolver_fails_loudly() {
    let (home, record) = installed(&asking(vec![request("a")]), false, true);
    let error = PluginExecutor::new(Some(home.path().to_owned()))
        .call(&record, json!({}))
        .await
        .unwrap_err();
    assert!(format!("{error:#}").contains("remote_ocr"), "{error:#}");
}

fn session(fake: &Fake) -> Session {
    Session::new(fake.endpoint(1)).unwrap()
}

fn requests(ids: &[&str]) -> Vec<Request> {
    ids.iter()
        .map(|id| Request {
            id: (*id).to_owned(),
            prompt: "p".to_owned(),
            images: Vec::new(),
            max_tokens: None,
        })
        .collect()
}

#[tokio::test]
async fn two_failed_rounds_in_a_row_short_circuit_every_later_request() {
    let fake = Fake::start(Reply::Status(500), Duration::ZERO).await;
    let mut session = session(&fake);
    let deadline = Instant::now() + Duration::from_secs(30);
    for round in 1..=2 {
        let results = session.serve(requests(&["a", "b"]), deadline).await;
        assert_eq!(results.len(), 2);
        assert_eq!(fake.requests(), 2 * round);
    }
    let results = session.serve(requests(&["a", "b", "c"]), deadline).await;
    assert_eq!(
        fake.requests(),
        4,
        "a dead endpoint must not be called again"
    );
    assert!(results.values().all(|result| result["error"]
        .as_str()
        .is_some_and(|error| error.contains("not answering"))));
}

#[tokio::test]
async fn one_answered_request_resets_the_failed_round_count() {
    let fake = Fake::start(Reply::Text, Duration::ZERO).await;
    let mut session = session(&fake);
    session.failed_rounds = DEAD_ENDPOINT_ROUNDS - 1;
    let deadline = Instant::now() + Duration::from_secs(30);
    let results = session.serve(requests(&["a"]), deadline).await;
    assert_eq!(results["a"]["text"], "answer:p");
    assert_eq!(session.failed_rounds, 0);
}

#[test]
fn malformed_model_calls_are_named() {
    let bad = |calls: Value| parse_batch(&json!({"model_calls": calls})).err();
    assert!(parse_batch(&json!({"text": 1})).unwrap().is_none());
    assert!(parse_batch(&json!([1])).unwrap().is_none());
    assert!(bad(json!("x")).is_none() && bad(json!(true)).is_none());
    assert!(bad(json!({})).unwrap().contains("requests"));
    assert!(bad(json!({"requests": [{"prompt": "p"}]}))
        .unwrap()
        .contains("id"));
    assert!(bad(json!({"requests": [{"id": "a"}]}))
        .unwrap()
        .contains("prompt"));
    assert!(bad(json!({"requests": [request("a"), request("a")]}))
        .unwrap()
        .contains("twice"));
    assert!(bad(json!({"requests": [{"id": "a", "prompt": "p",
        "images": [{"mime": "image/gif", "data_base64": "AA"}]}]}))
    .unwrap()
    .contains("image/png"));
    assert!(
        bad(json!({"requests": [{"id": "a", "prompt": "p", "max_tokens": 0}]}))
            .unwrap()
            .contains("max_tokens")
    );
    let many = (0..=MAX_REQUESTS_PER_ROUND)
        .map(|n| request(&n.to_string()))
        .collect::<Vec<_>>();
    assert!(bad(json!({"requests": many})).unwrap().contains("at most"));
    let ok = parse_batch(&asking(vec![request("a")])).unwrap().unwrap();
    assert_eq!(ok.requests.len(), 1);
    assert_eq!(ok.state, Some(json!({"page": 7})));
}

#[tokio::test]
async fn a_profile_resolves_to_its_chat_completions_endpoint() {
    let owner = "did:key:model-owner";
    let node = Arc::new(defra_node::EmbeddedNode::builder().build().await.unwrap());
    crate::ensure_runtime_schemas(&node).await.unwrap();
    crate::test_support::install_test_behavior(&node, owner, "chandra").await;
    let models = AccessModels(ConfigAccess::Local(node.clone()));
    let binding = |profile: &str| ModelBinding {
        agent_did: owner.to_owned(),
        profile_id: profile.to_owned(),
    };
    let endpoint = models.resolve(&binding("chandra:inference")).await.unwrap();
    assert_eq!(endpoint.url, "http://127.0.0.1:1/v1/chat/completions");
    assert_eq!(endpoint.model, "test-model");
    assert_eq!(endpoint.max_concurrent, 1);
    assert!(endpoint.api_key.is_none());
    let error = models.resolve(&binding("gone")).await.unwrap_err();
    assert!(
        format!("{error:#}").contains("no longer exists"),
        "{error:#}"
    );
}

fn backend(patch: Value) -> InferenceBackend {
    let mut backend = json!({"agent_did": "did:key:o", "backend_id": "b", "name": "B",
        "provider_kind": "OpenAiCompatible", "endpoint": "http://h:8000/v1/",
        "auth": {"kind": "api_key", "key": "k"}, "max_concurrent": 3});
    backend
        .as_object_mut()
        .unwrap()
        .extend(patch.as_object().unwrap().clone());
    serde_json::from_value(backend).unwrap()
}

fn profile() -> InferenceProfile {
    serde_json::from_value(json!({"agent_did": "did:key:o", "profile_id": "p",
        "backend_id": "b", "model_name": "m", "max_output_tokens": 4096}))
    .unwrap()
}

#[test]
fn only_an_enabled_chat_completions_backend_can_serve_a_slot() {
    let endpoint = endpoint_for(&profile(), &backend(json!({}))).unwrap();
    assert_eq!(endpoint.url, "http://h:8000/v1/chat/completions");
    assert_eq!(endpoint.api_key.as_deref(), Some("k"));
    assert_eq!(endpoint.max_concurrent, 3);
    assert_eq!(endpoint.max_output_tokens, Some(4096));
    for patch in [
        json!({"enabled": false}),
        json!({"openai_wire_api": "responses"}),
        json!({"provider_kind": "ChatGptCodex"}),
        json!({"auth": {"kind": "principal_oauth"}}),
        json!({"max_concurrent": 0}),
    ] {
        assert!(
            endpoint_for(&profile(), &backend(patch.clone())).is_err(),
            "{patch}"
        );
    }
}

#[test]
fn binding_slots_survives_a_reinstall_and_unbinding_clears_it() {
    let (home, record) = installed(&asking(vec![request("a")]), false, false);
    let manifest: crate::pack::PackManifest = serde_json::from_value(json!({
        "manifest_version": 1, "name": "ocr", "namespace": "team", "version": "1.0.0", "description": "d",
        "authors": ["t"], "kind": "plugins", "assets": ["README.md", "plugins/plugin.afb"],
        "inference_slots": [{"name": "remote_ocr", "description": "d", "optional": true}],
        "plugins": [record.declaration],
    }))
    .unwrap();
    let bindings =
        std::collections::BTreeMap::from([("remote_ocr".to_owned(), "chandra".to_owned())]);
    let bound =
        crate::plugin::install::bind_plugin_slots(home.path(), &manifest, "did:key:o", &bindings)
            .unwrap();
    assert_eq!(bound, ["team/plugin"]);
    let read = || store::read_record(home.path(), "team", "plugin").unwrap();
    assert_eq!(read().model_binding.unwrap().profile_id, "chandra");

    let artifact =
        store::read_bytes(home.path(), record.digest.strip_prefix("sha256:").unwrap()).unwrap();
    crate::plugin::install::install_from_pack(
        home.path(),
        "team",
        "team/pack",
        "2.0.0",
        "sha256:p2",
        &read().declaration,
        &artifact,
        None,
        false,
    )
    .unwrap();
    assert_eq!(read().model_binding.unwrap().profile_id, "chandra");

    crate::plugin::install::set_model_binding(home.path(), "team/plugin", None).unwrap();
    assert!(read().model_binding.is_none());
    let mut plain = read();
    plain.declaration.model_slot = None;
    store::write_record(home.path(), &plain).unwrap();
    assert!(crate::plugin::install::set_model_binding(home.path(), "team/plugin", None).is_err());
}

#[tokio::test]
async fn a_model_tool_gets_its_model_answers_through_the_same_path() {
    use crate::document_config::PluginToolRef;
    use crate::llm::tool::ToolDyn;
    use crate::plugin::tool::PluginTool;

    let fake = Arc::new(Fake::start(Reply::Text, Duration::ZERO).await);
    let (home, record) = installed(&asking(vec![request("a")]), false, true);
    let executor = Arc::new(executor(
        &home,
        Fixed(fake.clone(), 1, Duration::from_secs(30)),
    ));
    let tool = PluginTool::resolve(
        executor,
        &PluginToolRef {
            plugin: "team/plugin".into(),
            digest: Some(record.digest.clone()),
        },
        None,
    )
    .unwrap();
    let output: Value = serde_json::from_str(&tool.call("{}".to_owned()).await.unwrap()).unwrap();
    assert_eq!(output["model_results"]["a"]["text"], "answer:pa");
    assert_eq!(fake.requests(), 1);
}

/// A resolver whose profile is gone.
struct Stale;

impl ModelResolver for Stale {
    fn resolve<'a>(&'a self, _: &'a ModelBinding) -> BoxFuture<'a, Result<ModelEndpoint>> {
        Box::pin(async { anyhow::bail!("the inference profile \"chandra\" no longer exists") })
    }
}

/// `installed`, but with the plugin's wall clock set to `secs`.
fn installed_within(
    canned: &Value,
    forever: bool,
    secs: u32,
) -> (tempfile::TempDir, InstalledPlugin) {
    let (home, mut record) = installed(canned, forever, true);
    record.declaration.limits = Some(crate::pack::PluginLimits {
        wall_clock_secs: Some(secs),
        ..Default::default()
    });
    record.granted =
        store::grant_on_install(home.path(), &record.namespace, &record.declaration, true).unwrap();
    store::write_record(home.path(), &record).unwrap();
    (home, record)
}

#[tokio::test]
async fn model_time_running_out_leaves_the_plugin_a_final_round_to_finish() {
    let fake = Arc::new(Fake::start(Reply::Hang, Duration::ZERO).await);
    let (home, record) = installed_within(&asking(vec![request("a")]), false, 6);
    let call = executor(&home, Fixed(fake, 1, Duration::from_secs(30)))
        .call(&record, json!({"n": 1}))
        .await
        .unwrap();
    assert_eq!(call.outcome.verdict, PluginVerdict::Success);
    assert_eq!(call.outcome.output["n"], 1);
    assert_eq!(
        call.outcome.output["model_results"]["a"],
        json!({"error": "the model request timed out"})
    );
    assert!(call.outcome.wall_ms < 6000, "{}", call.outcome.wall_ms);
}

#[tokio::test]
async fn a_plugin_that_asks_again_in_its_final_round_is_a_timeout() {
    let fake = Arc::new(Fake::start(Reply::Hang, Duration::ZERO).await);
    let (home, record) = installed_within(&asking(vec![request("a")]), true, 6);
    let call = executor(&home, Fixed(fake.clone(), 1, Duration::from_secs(30)))
        .call(&record, json!({}))
        .await
        .unwrap();
    assert_eq!(call.outcome.verdict, PluginVerdict::Timeout);
    assert_eq!(fake.requests(), 1);
}

#[tokio::test]
async fn a_stale_binding_runs_the_plugin_without_a_model_and_says_so() {
    let (home, record) = installed(&asking(vec![request("a")]), false, true);
    let input = json!({"n": 1});
    let call = executor(&home, Stale)
        .call(&record, input.clone())
        .await
        .unwrap();
    assert_eq!(call.outcome.verdict, PluginVerdict::Success);
    assert_eq!(call.outcome.output, input, "no model_calls key was sent");
    let note = call.binding_note.unwrap();
    assert!(
        note.contains("chandra")
            && note.contains("gents plugin bind")
            && note.contains("gents plugin unbind"),
        "{note}"
    );
}

#[tokio::test]
async fn a_healthy_binding_carries_no_note() {
    let fake = Arc::new(Fake::start(Reply::Text, Duration::ZERO).await);
    let (home, record) = installed(&asking(vec![request("a")]), false, true);
    let call = executor(&home, Fixed(fake, 1, Duration::from_secs(30)))
        .call(&record, json!({}))
        .await
        .unwrap();
    assert!(call.binding_note.is_none());
}

#[tokio::test]
async fn sessions_on_one_backend_share_its_concurrency_cap() {
    let fake = Fake::start(Reply::Text, Duration::from_millis(120)).await;
    let mut first = Session::new(fake.endpoint(2)).unwrap();
    let mut second = Session::new(fake.endpoint(2)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let ids = ["a", "b", "c", "d"];
    let (one, two) = tokio::join!(
        first.serve(requests(&ids), deadline),
        second.serve(requests(&ids), deadline)
    );
    assert_eq!(one.len() + two.len(), 8);
    assert_eq!(fake.requests(), 8);
    assert_eq!(fake.max_in_flight.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn requests_past_the_call_request_limit_get_error_results() {
    let fake = Fake::start(Reply::Text, Duration::ZERO).await;
    let mut session = session(&fake);
    session.max_requests = 3;
    let deadline = Instant::now() + Duration::from_secs(30);
    let results = session
        .serve(requests(&["a", "b", "c", "d", "e"]), deadline)
        .await;
    assert_eq!(
        results.values().filter(|r| r.get("text").is_some()).count(),
        3
    );
    assert_eq!(fake.requests(), 3);
    let later = session.serve(requests(&["f"]), deadline).await;
    assert!(later["f"]["error"]
        .as_str()
        .unwrap()
        .contains("request limit"));
    assert_eq!(fake.requests(), 3);
    assert_eq!(session.failed_rounds, 0, "a limit is not a dead endpoint");
}

#[tokio::test]
async fn answers_past_the_call_byte_budget_become_errors_and_stop_sending() {
    let fake = Fake::start(Reply::Text, Duration::ZERO).await;
    let mut session = session(&fake);
    // "answer:p" is 8 bytes: two fit, the third crosses 16.
    session.max_answer_bytes = 16;
    let deadline = Instant::now() + Duration::from_secs(30);
    let results = session.serve(requests(&["a", "b", "c"]), deadline).await;
    assert_eq!(
        results.values().filter(|r| r.get("text").is_some()).count(),
        2
    );
    assert_eq!(fake.requests(), 3);
    let later = session.serve(requests(&["d"]), deadline).await;
    assert!(later["d"]["error"]
        .as_str()
        .unwrap()
        .contains("answer budget"));
    assert_eq!(fake.requests(), 3);
}

#[tokio::test]
async fn an_answer_is_cut_at_max_result_bytes() {
    let deadline = Instant::now() + Duration::from_secs(30);
    let exact = Fake::start(Reply::Big(MAX_RESULT_BYTES), Duration::ZERO).await;
    let results = session(&exact).serve(requests(&["a"]), deadline).await;
    assert_eq!(
        results["a"]["text"].as_str().unwrap().len(),
        MAX_RESULT_BYTES
    );
    let over = Fake::start(Reply::Big(MAX_RESULT_BYTES + 1), Duration::ZERO).await;
    let results = session(&over).serve(requests(&["a"]), deadline).await;
    assert!(results["a"]["error"]
        .as_str()
        .unwrap()
        .contains("larger than"));
}

#[tokio::test]
async fn a_redirect_is_refused_and_never_followed() {
    let target = Fake::start(Reply::Text, Duration::ZERO).await;
    let fake = Fake::start(Reply::Redirect(target.url.clone()), Duration::ZERO).await;
    let deadline = Instant::now() + Duration::from_secs(30);
    let results = session(&fake).serve(requests(&["a"]), deadline).await;
    assert_eq!(
        results["a"],
        json!({"error": "the model endpoint answered HTTP 307"})
    );
    assert_eq!(target.requests(), 0);
}

#[tokio::test]
async fn max_tokens_is_capped_by_the_profile_and_defaulted_when_unset() {
    assert_eq!(token_limit(Some(9999), Some(100)), 100);
    assert_eq!(token_limit(Some(50), Some(100)), 50);
    assert_eq!(token_limit(Some(50), None), 50);
    assert_eq!(token_limit(None, Some(100)), 100);
    assert_eq!(token_limit(None, None), DEFAULT_MAX_TOKENS);
    let fake = Fake::start(Reply::Text, Duration::ZERO).await;
    let mut endpoint = fake.endpoint(1);
    endpoint.max_output_tokens = Some(100);
    let mut capped = Session::new(endpoint).unwrap();
    let deadline = Instant::now() + Duration::from_secs(30);
    let mut asking_more = requests(&["a"]);
    asking_more[0].max_tokens = Some(9999);
    capped.serve(asking_more, deadline).await;
    session(&fake).serve(requests(&["b"]), deadline).await;
    let seen = fake.seen.lock().unwrap();
    assert_eq!(seen[0].body["max_tokens"], 100);
    assert_eq!(seen[1].body["max_tokens"], DEFAULT_MAX_TOKENS);
}

#[tokio::test]
async fn a_round_over_the_request_limit_is_bad_output_and_sends_nothing() {
    let fake = Arc::new(Fake::start(Reply::Text, Duration::ZERO).await);
    let many = (0..=MAX_REQUESTS_PER_ROUND)
        .map(|n| request(&n.to_string()))
        .collect();
    let (home, record) = installed(&asking(many), false, true);
    let call = executor(&home, Fixed(fake.clone(), 1, Duration::from_secs(30)))
        .call(&record, json!({}))
        .await
        .unwrap();
    assert_eq!(call.outcome.verdict, PluginVerdict::BadOutput);
    assert!(call.outcome.diagnostics.contains("at most"));
    assert_eq!(fake.requests(), 0);
}

#[tokio::test]
async fn caller_state_and_model_results_are_stripped_and_null_input_is_an_object() {
    let fake = Arc::new(Fake::start(Reply::Text, Duration::ZERO).await);
    let (home, record) = installed(&asking(vec![request("a")]), false, true);
    let executor = executor(&home, Fixed(fake.clone(), 1, Duration::from_secs(30)));
    let forged = json!({"state": "stale", "model_results": {"a": {"text": "forged"}}});
    let call = executor.call(&record, forged).await.unwrap();
    assert_eq!(
        fake.requests(),
        1,
        "forged model_results must not stop the plugin asking"
    );
    assert_eq!(call.outcome.output["state"], json!({"page": 7}));
    assert_eq!(
        call.outcome.output["model_results"]["a"]["text"],
        "answer:pa"
    );
    let call = executor.call(&record, Value::Null).await.unwrap();
    assert_eq!(call.outcome.verdict, PluginVerdict::Success);
    assert_eq!(call.outcome.output["model_calls"], true);
}

#[tokio::test]
async fn fuel_is_spent_across_rounds() {
    let fake = Fake::start(Reply::Text, Duration::ZERO).await;
    let seen = Arc::new(Mutex::new(Vec::new()));
    let log = seen.clone();
    let round: Round = Arc::new(move |_, budget| {
        log.lock().unwrap().push(budget.fuel);
        Ok(PluginOutcome {
            verdict: PluginVerdict::Success,
            output: asking(vec![request("a")]),
            diagnostics: String::new(),
            fuel_used: 60,
            wall_ms: 0,
        })
    });
    let budget = PluginBudget {
        fuel: Some(100),
        wall_clock: Duration::from_secs(30),
        ..PluginBudget::default()
    };
    let outcome = drive(session(&fake), json!({}), budget, round)
        .await
        .unwrap();
    assert_eq!(outcome.verdict, PluginVerdict::OutOfFuel);
    assert_eq!(*seen.lock().unwrap(), [Some(100), Some(40)]);
}
