use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use futures::future::BoxFuture;
use gents_pack::plugin::http_test_server::Server;
use gents_pack::plugin::store;
use gents_pack::plugin::PluginVerdict;
use serde_json::{json, Value};

use crate::plugin::executor::PluginExecutor;
use gents_pack::plugin::test_fixtures::{asking_plugin_wat, installed_plugin};

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
    record.model_binding = Some(gents_pack::plugin::model_binding::ModelBinding {
        node_did: "did:key:owner".into(),
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
    assert_eq!(
        call.outcome.diagnostics,
        "one round asks for exactly one of model_calls, http_calls, tool_calls, data_calls or job_calls"
    );
}

struct Endpoint;

impl gents_pack::plugin::model_binding::ModelResolver for Endpoint {
    fn resolve<'a>(
        &'a self,
        _: &'a gents_pack::plugin::model_binding::ModelBinding,
        _: Option<&'a identity::Did>,
    ) -> BoxFuture<'a, Result<gents_pack::plugin::model_binding::ModelEndpoint>> {
        Box::pin(async {
            Ok(gents_pack::plugin::model_binding::ModelEndpoint {
                url: "http://127.0.0.1:9/v1/chat/completions".into(),
                api_key: None,
                model: "m".into(),
                max_concurrent: 1,
                backend_key: "test".into(),
                connect_timeout: Duration::from_secs(1),
                request_timeout: Duration::from_secs(1),
                max_output_tokens: None,
                profile_visibility: None,
            })
        })
    }
}
