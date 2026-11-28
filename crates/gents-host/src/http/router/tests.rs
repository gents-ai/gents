#[cfg(unix)]
mod process_control_acceptance;

use std::time::Instant;

use axum::http::header;
use gents_protocol::row::{
    AgentReadinessEntry, AgentReadinessState, NodeReadinessProcessState, NodeReadinessRow,
    NodeReadinessSnapshot, NODE_READINESS_FORMAT_VERSION,
};

use serde_json::{json, Value};

use super::metrics::{
    load_p2p_metrics_for_scrape, p2p_metrics_from_status, P2P_METRICS_FETCH_BUDGET,
};
use super::status::{status_handler, STATUS_PROBE_BUDGET};
use super::*;
use crate::http::prometheus::store::MetricsRuntimeRow;

fn desktop_endpoint(
    url: impl Into<String>,
    identity: &gents_store::client_journal::principal_identity::PrincipalIdentity,
) -> GraphqlEndpoint {
    GraphqlEndpoint::as_principal(url, identity.did())
        .with_client_signing_capability(
            gents_store::config_client::ClientSigningCapability::from_ed25519_key(
                identity.private_key_bytes(),
            )
            .unwrap(),
        )
        .unwrap()
}

pub(crate) fn state() -> RuntimeHttpState {
    let (activation_runtime, activation_observation) = empty_activation_state();
    RuntimeHttpState {
        client_access_decisions: Default::default(),
        graphql: gents_store::config_client::GraphqlEndpoint::anonymous(
            "http://127.0.0.1:9181/api/v0/graphql",
        ),
        p2p_graphql: gents_store::config_client::GraphqlEndpoint::anonymous(
            "http://127.0.0.1:9181/api/v0/graphql",
        ),
        node_name: "amy".to_string(),
        node_did: "did:key:zAgent".to_string(),
        tool_ceiling: "readwrite".to_string(),
        tool_root: Some("/Users/test".to_string()),
        tool_root_error: None,
        cli_tools: Vec::new(),
        home: None,
        started_at: "2026-06-04T00:00:00Z".to_string(),
        started_instant: Instant::now(),
        backend_health: None,
        p2p_admission: None,
        p2p_metrics_cache: std::sync::Arc::new(std::sync::Mutex::new(None)),
        store_size: crate::http::prometheus::StoreSizeCache::new(None),
        enrollment_offer_issuer: crate::http::enrollment::empty_issuer_handle(),
        enrollment_decisions: crate::http::enrollment::empty_decision_service_handle(),
        activation_runtime,
        activation_observation,
        replicated_schema: Default::default(),
        serve_lifecycle: Default::default(),
        document_acp: Default::default(),
        node_identity: None,
        pack_watch_slots: std::sync::Arc::new(tokio::sync::Semaphore::new(16)),
    }
}

#[tokio::test]
async fn inference_activity_and_backend_discovery_require_authenticated_identity() {
    let app = Router::new()
        .route(
            gents_inference::inference_observation::INFERENCE_ACTIVITY_PATH,
            post(crate::http::inference_activity::read),
        )
        .route(
            crate::http::inference_activity::BACKEND_DISCOVERY_PATH,
            post(crate::http::inference_activity::discover_backend),
        )
        .with_state(state());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let _server = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    }));
    let client = reqwest::Client::new();
    for (path, payload) in [
        (
            gents_inference::inference_observation::INFERENCE_ACTIVITY_PATH,
            json!({"node":"node", "session":"session", "request":"request", "generation":"generation"}),
        ),
        (
            crate::http::inference_activity::BACKEND_DISCOVERY_PATH,
            json!({"node_did":"node", "backend_id":"backend"}),
        ),
    ] {
        let response = client
            .post(format!("http://{address}{path}"))
            .json(&payload)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED, "{path}");
        assert_eq!(
            response.headers().get(header::CACHE_CONTROL).unwrap(),
            "no-store",
            "{path}"
        );
    }
}

#[tokio::test]
async fn runtime_control_binds_signed_observation_and_retires_only_the_operator_observed_build() {
    use gents_protocol::serve_lifecycle::control::{
        RuntimeControlCommand, RuntimeControlDisposition, RuntimeControlReceipt, RuntimeOperation,
        RUNTIME_CONTROL_PATH, RUNTIME_CONTROL_VERSION,
    };
    use gents_store::client_journal::paths::DesktopPaths;
    use gents_store::client_journal::principal_identity::PrincipalIdentity;
    let temp = tempfile::tempdir().unwrap();
    // Runtime control admits only the Node's own signer; the enrolled User
    // configures the Agent the runtime starts over its fresh backend admission.
    let home = crate::test_support::ClientHome::new().await;
    let identity = home.home.node_identity.clone();
    let foreign_paths = DesktopPaths::from_root(temp.path().join("foreign"));
    foreign_paths.ensure_root_dirs().await.unwrap();
    let foreign = PrincipalIdentity::load_or_create(&foreign_paths)
        .await
        .unwrap();
    let owner = identity.did().to_owned();
    let node = home.node().clone();
    let binding = home.agent("fixture").await;
    home.mark_backend_healthy(&binding).await;
    let runtime = gents::Gents::builder()
        .node(node.clone())
        .backend_admission(home.home.backend_admission())
        .identity(identity.clone())
        .tool_ceiling(gents_tools::surface::ToolCeiling::meta_only())
        .agent("fixture")
        .identity_binding(binding.clone())
        .backend_id(format!("{}:backend", binding.agent_did))
        .model_name("test-model")
        .done()
        .build()
        .await
        .unwrap();
    let admission = runtime.service_admission();
    let mut state = state();
    state.node_did = owner.clone();
    let lifecycle = state.serve_lifecycle.clone();
    assert!(state.activation_runtime.set(runtime).is_ok());
    let mut starting_state = state.clone();
    starting_state.serve_lifecycle = Default::default();
    let app = Router::new()
        .route(
            RUNTIME_CONTROL_PATH,
            post(crate::http::runtime_control::runtime_control_handler),
        )
        .with_state(state)
        .merge(
            Router::new()
                .route(
                    "/runtime/operator/starting",
                    post(crate::http::runtime_control::runtime_control_handler),
                )
                .with_state(starting_state),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    }));
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let url = format!("http://{addr}{RUNTIME_CONTROL_PATH}");
    let graphql = format!("http://{addr}/api/v0/graphql");
    let endpoint = GraphqlEndpoint::as_principal(&graphql, identity.did())
        .with_client_signing_capability(identity.client_signing_capability().unwrap())
        .unwrap();
    let command = |operation| RuntimeControlCommand {
        version: RUNTIME_CONTROL_VERSION,
        node_did: owner.clone(),
        nonce: uuid::Uuid::new_v4().to_string(),
        issued_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        operation,
    };
    let observe = command(RuntimeOperation::Observe);
    let response = client.post(&url).json(&observe).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let starting: RuntimeControlReceipt = response.json().await.unwrap();
    gents_store::service::verify_control_receipt(&observe, &starting).unwrap();
    assert_eq!(starting.disposition, RuntimeControlDisposition::Starting);
    lifecycle.mark_ready();
    let observe = command(RuntimeOperation::Observe);
    let receipt: RuntimeControlReceipt = client
        .post(&url)
        .json(&observe)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    gents_store::service::verify_control_receipt(&observe, &receipt).unwrap();
    assert_eq!(receipt.disposition, RuntimeControlDisposition::Observed);
    let mut modified = receipt.clone();
    modified.capabilities.insert("request_input".into(), 999);
    assert!(gents_store::service::verify_control_receipt(&observe, &modified).is_err());
    let other_challenge = command(RuntimeOperation::Observe);
    assert!(gents_store::service::verify_control_receipt(&other_challenge, &receipt).is_err());
    let retire = command(RuntimeOperation::Retire {
        expected_build: receipt.build.clone(),
    });
    assert!(gents_store::service::verify_control_receipt(&retire, &receipt).is_err());
    let response = client.post(&url).json(&retire).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let response = client
        .post(&url)
        .bearer_auth("invalid")
        .json(&retire)
        .send()
        .await
        .unwrap();
    assert!(response.status().is_client_error());
    let foreign_endpoint = desktop_endpoint(&graphql, &foreign);
    let response = foreign_endpoint
        .authorize_runtime(client.post(&url))
        .await
        .unwrap()
        .json(&retire)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let mut other_node = retire.clone();
    other_node.node_did = foreign.did().to_owned();
    let response = endpoint
        .authorize_runtime(client.post(&url))
        .await
        .unwrap()
        .json(&other_node)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let changed = command(RuntimeOperation::Retire {
        expected_build: "0".repeat(64),
    });
    let refused: RuntimeControlReceipt = endpoint
        .authorize_runtime(client.post(&url))
        .await
        .unwrap()
        .json(&changed)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    gents_store::service::verify_control_receipt(&changed, &refused).unwrap();
    assert_eq!(refused.disposition, RuntimeControlDisposition::BuildChanged);
    assert!(!admission.is_retiring().unwrap());

    node.shutdown().await.expect("node shutdown");
    let response = endpoint
        .authorize_runtime(client.post(format!("http://{addr}/runtime/operator/starting")))
        .await
        .unwrap()
        .json(&retire)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let accepted: RuntimeControlReceipt = response.json().await.unwrap();
    gents_store::service::verify_control_receipt(&retire, &accepted).unwrap();
    assert_eq!(accepted.disposition, RuntimeControlDisposition::Retiring);
    assert_eq!(
        accepted.lifecycle,
        gents_protocol::serve_lifecycle::ServeLifecycle::Starting
    );
    assert!(admission.is_retiring().unwrap());
    let repeated: RuntimeControlReceipt = endpoint
        .authorize_runtime(client.post(&url))
        .await
        .unwrap()
        .json(&retire)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    gents_store::service::verify_control_receipt(&retire, &repeated).unwrap();
    assert_eq!(repeated.disposition, RuntimeControlDisposition::Stopping);
    drop(server);
}

/// The home's GraphQL beside the contract routes, as one runtime serves
/// both: the request keeps its `Host`, which its credential names.
fn forward_graphql(
    upstream: String,
) -> impl Fn(
    axum::http::HeaderMap,
    axum::body::Bytes,
) -> futures::future::BoxFuture<'static, axum::response::Response>
       + Clone
       + Send
       + Sync
       + 'static {
    move |headers, body| {
        let upstream = upstream.clone();
        Box::pin(async move {
            let mut request = reqwest::Client::new().post(&upstream).body(body);
            for name in [header::AUTHORIZATION, header::HOST, header::CONTENT_TYPE] {
                if let Some(value) = headers.get(&name) {
                    request = request.header(name, value);
                }
            }
            let response = request.send().await.unwrap();
            let status = response.status();
            let bytes = response.bytes().await.unwrap();
            (status, bytes).into_response()
        })
    }
}

/// The home's native node explanation beside the contract routes, as one
/// runtime serves both, so a private reader's admin observation reaches it.
fn forward_explain(
    upstream: String,
) -> impl Fn(
    axum::http::HeaderMap,
    axum::extract::RawQuery,
) -> futures::future::BoxFuture<'static, axum::response::Response>
       + Clone
       + Send
       + Sync
       + 'static {
    move |headers, query| {
        let upstream = upstream.clone();
        Box::pin(async move {
            let url = match query.0 {
                Some(query) => format!("{upstream}?{query}"),
                None => upstream,
            };
            let mut request = reqwest::Client::new().get(url);
            for name in [header::AUTHORIZATION, header::HOST] {
                if let Some(value) = headers.get(&name) {
                    request = request.header(name, value);
                }
            }
            let response = request.send().await.unwrap();
            let status = response.status();
            let bytes = response.bytes().await.unwrap();
            (status, bytes).into_response()
        })
    }
}

fn demo_subject(digest: &str, name: &str) -> gents_pack::pack::wire_subject::WireSubject {
    gents_pack::pack::wire_subject::WireSubject {
        owner_pack_digest: digest.into(),
        owner_pack_coordinate: "acme/demo".into(),
        namespace: "acme".into(),
        name: name.into(),
        version: "1.0.0".into(),
        digest: format!("sha256:{}", "a".repeat(64)),
        declaration: gents_pack::pack::PackPlugin {
            name: name.into(),
            description: "Runs".into(),
            artifact: format!("plugins/{name}.wasm"),
            language: "rust".into(),
            input_schema: json!({"type": "object"}),
            ..gents_pack::pack::PackPlugin::default()
        },
    }
}

fn demo_approval(
    digest: &str,
    version: &str,
    plugins: &[&str],
    tools: &[&str],
    agent_targets: &[&str],
) -> gents_pack::pack::approvals::PackApproval {
    use gents_pack::pack::wire_subject::IdPart;
    let ids = |ids: &[&str]| {
        ids.iter()
            .map(|id| vec![IdPart::Text((*id).to_owned())])
            .collect()
    };
    gents_pack::pack::approvals::PackApproval {
        digest: digest.into(),
        coordinate: "acme/demo".into(),
        version: version.into(),
        kind: gents_pack::pack::PackKind::Plugins,
        approved_at: "2026-11-19T00:00:00Z".into(),
        subjects: plugins
            .iter()
            .map(|name| demo_subject(digest, name))
            .collect(),
        yields: Some(gents_pack::pack::approvals::PackYield {
            tools: ids(tools),
            agent_targets: ids(agent_targets),
            ..Default::default()
        }),
    }
}

#[tokio::test]
async fn a_runtime_answering_with_what_the_client_did_not_approve_gets_nothing_signed() {
    use gents_pack::pack::plugin_wire::WireRequest;
    use std::sync::atomic::{AtomicUsize, Ordering};
    let user =
        tokio::task::spawn_blocking(|| gents_store::test_support::test_identity("unvouched"))
            .await
            .unwrap();
    let (older, newer, unapproved) = (
        format!("sha256:{}", "e".repeat(64)),
        format!("sha256:{}", "f".repeat(64)),
        format!("sha256:{}", "9".repeat(64)),
    );
    let served = std::sync::Arc::new(std::sync::Mutex::new(json!({})));
    let installation = std::sync::Arc::new(std::sync::Mutex::new(json!({})));
    let sessions = std::sync::Arc::new(AtomicUsize::new(0));
    let (record, installed, opened) = (served.clone(), installation.clone(), sessions.clone());
    let app = Router::new()
        .route(
            gents_pack::pack::plugin_wire::PLUGIN_RECORD_PATH,
            get(move || {
                let record = record.clone();
                async move { axum::Json(record.lock().unwrap().clone()) }
            }),
        )
        .route(
            "/api/v0/graphql",
            post(move || {
                let installed = installed.clone();
                async move {
                    axum::Json(
                        json!({"data": {"PackInstallation": [installed.lock().unwrap().clone()]}}),
                    )
                }
            }),
        )
        .route(
            "/packs/install/session",
            get(move || {
                opened.fetch_add(1, Ordering::SeqCst);
                async { StatusCode::BAD_REQUEST }
            }),
        );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let _server = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    }));
    let endpoint =
        crate::test_support::user_endpoint(&format!("http://{address}/api/v0/graphql"), &user);
    let owner = "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK";
    let approvals_root = tempfile::tempdir().unwrap();
    let approvals = gents_pack::pack::approvals::Approvals::under(approvals_root.path());
    for approval in [
        demo_approval(
            &older,
            "0.9.0",
            &["tool"],
            &["old-tools"],
            &["a", "old-target"],
        ),
        demo_approval(&newer, "1.0.0", &["tool", "other"], &[], &["a"]),
    ] {
        approvals.record(owner, approval).await.unwrap();
    }
    let install = |digest: &str, documents: &[(&str, &str)]| {
        *installation.lock().unwrap() = json!({
            "_docID": "bae-demo",
            "digest": digest,
            "documents": documents
                .iter()
                .map(|(collection, id)| json!({
                    "collection": collection, "id": id, "digest": "", "created": true
                }))
                .collect::<Vec<_>>(),
        });
    };
    let request = WireRequest {
        plugin: "acme/tool".into(),
        agent: "did:key:agent".into(),
        targets_from: vec!["acme/demo".into()],
        remove: false,
        preview: false,
        tool_calls_consent: false,
    };
    let wire = |request: WireRequest| {
        let (endpoint, approvals, root) = (endpoint.clone(), &approvals, approvals_root.path());
        async move {
            let plan =
                crate::http::pack_wire_client::served_plan(&endpoint, owner, &request, approvals)
                    .await?;
            crate::http::pack_wire_client::wire_as_client(
                endpoint,
                owner.to_owned(),
                root.to_owned(),
                vec![gents_pack::pack::plugin_wire::ClientWire {
                    request,
                    plan: Some(plan),
                }],
            )
            .await
        }
    };

    *served.lock().unwrap() =
        serde_json::to_value(demo_subject(&newer, "other").installed()).unwrap();
    let another = wire(request.clone())
        .await
        .expect_err("another approved plugin's record plans nothing for the one asked for");
    assert!(
        format!("{another:#}").contains("gents config pack approve"),
        "{another:#}"
    );

    *served.lock().unwrap() =
        serde_json::to_value(demo_subject(&newer, "tool").installed()).unwrap();
    for (digest, documents, refusal, why) in [
        (
            &newer,
            vec![("AgentTarget", "a"), ("AgentTarget", "rogue")],
            "does not yield",
            "an AgentTarget no approved version yields",
        ),
        (
            &newer,
            vec![("AgentTarget", "a"), ("AgentTarget", "old-target")],
            "does not yield",
            "an AgentTarget only the older approved version yields",
        ),
        (
            &newer,
            vec![("Tools", "old-tools"), ("AgentTarget", "a")],
            "does not yield",
            "a Tools document only the older approved version yields",
        ),
        (
            &unapproved,
            vec![("AgentTarget", "a")],
            "did not approve",
            "a version neither approval names",
        ),
    ] {
        install(digest, &documents);
        let refused = wire(request.clone()).await.expect_err(why);
        assert!(
            format!("{refused:#}").contains(refusal),
            "{why}: {refused:#}"
        );
        assert_eq!(
            sessions.load(Ordering::SeqCst),
            0,
            "{why}: nothing reached a pack session"
        );
    }

    install(&newer, &[("AgentTarget", "a")]);
    let _ = wire(request.clone()).await;
    assert_eq!(
        sessions.load(Ordering::SeqCst),
        1,
        "the newer version's own documents wire as far as the pack session"
    );
}

/// A User wires an installed plugin through the runtime of a home whatever
/// decides its access: the runtime holds the plugin store and runs the
/// transaction, the client signs each write with its own key, and the
/// runtime never loads a User key of its own.
#[tokio::test]
async fn a_client_wires_a_plugin_through_the_runtime_signing_each_write_itself() {
    use gents_pack::pack::plugin_wire::WireRequest;
    use gents_store::local::document_acp::DocumentAcp;
    let home = crate::test_support::ClientHome::owned_by_user().await;
    let identity = home.home.node_identity.clone();
    let owner = identity.did().to_owned();
    let binding = home.agent("fixture").await;
    home.mark_backend_healthy(&binding).await;
    let configuration: gents_documents::PackConfig = serde_json::from_value(json!({
        "node": {"node_did": owner},
        "tools": [{"node_did": owner, "tools_id": "client-wire-tools"}],
        "contexts": [{"node_did": owner, "context_id": "client-wire-context", "tools_id": "client-wire-tools"}],
        "agents": [{"node_did": owner, "agent_did": binding.agent_did,
            "creation_nonce": binding.creation_nonce, "context_id": "client-wire-context",
            "inference_profile_id": format!("{}:inference", binding.agent_did), "enabled": true}]
    }))
    .unwrap();
    let plan = gents_documents::DesiredStateApplyPlan::from_pack_config(&configuration).unwrap();
    home.access
        .transact_write("test.client_wire.context", |txn| {
            let plan = &plan;
            Box::pin(async move {
                gents_documents::apply_desired_state_plan(
                    txn,
                    plan,
                    &gents_inference::resolved::ProviderProfilePolicy,
                )
                .await
            })
        })
        .await
        .unwrap();
    let (mut plugin, artifact) = gents_pack::plugin::test_fixtures::build_plugin_pack(
        "client_wire",
        &gents_pack::plugin::test_fixtures::constant_output_wat(br#"{}"#),
        None,
    );
    plugin.name = "tool".into();
    for document_acp in [DocumentAcp::Local, DocumentAcp::Vera] {
        let plugin_home = tempfile::tempdir().unwrap();
        let runtime = gents::Gents::builder()
            .node(home.node().clone())
            .backend_admission(home.home.backend_admission())
            .identity(identity.clone())
            .tool_ceiling(gents_tools::surface::ToolCeiling::meta_only())
            .agent("fixture")
            .identity_binding(binding.clone())
            .backend_id(format!("{}:backend", binding.agent_did))
            .model_name("test-model")
            .done()
            .plugin_home(plugin_home.path())
            .build()
            .await
            .unwrap();
        let installed = gents_pack::plugin::install::install_from_pack(
            plugin_home.path(),
            &gents_pack::plugin::install::OwningPack {
                namespace: "acme",
                coordinate: "acme/client_wire",
                version: "1.0.0",
                digest: &format!("sha256:{}", "b".repeat(64)),
                gents_version: None,
            },
            gents_pack::plugin::install::ShippedPlugin {
                declaration: &plugin,
                artifact: &artifact,
                instructions: None,
                data_unique_keys: Default::default(),
                data_collection_fields: Default::default(),
                data_collection_schemas: Default::default(),
            },
            false,
        )
        .unwrap();
        let (activation_runtime, activation_observation) = empty_activation_state();
        assert!(activation_runtime.set(runtime.clone()).is_ok());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let graphql = format!("http://{addr}/api/v0/graphql");
        let app = runtime_contract_router(
            RouterIdentity {
                graphql: graphql.clone(),
                p2p_graphql: GraphqlEndpoint::anonymous(graphql.clone()),
                node_name: "client-wire".into(),
                node_did: owner.clone(),
                node_identity: None,
                document_acp,
            },
            ToolSurface {
                tool_ceiling: "readwrite".into(),
                tool_root: Ok(None),
                cli_tools: Vec::new(),
                home: Some(plugin_home.path().display().to_string()),
                store_dir: None,
                mcp: None,
            },
            EnrollmentHandles {
                offer_issuer: crate::http::enrollment::empty_issuer_handle(),
                decisions: crate::http::enrollment::empty_decision_service_handle(),
                p2p_admission: None,
            },
            RuntimeActivation {
                runtime: activation_runtime,
                observation: activation_observation,
                replicated_schema: Default::default(),
                serve_lifecycle: Default::default(),
            },
            None,
            16,
        )
        .contract
        .route(
            "/api/v0/graphql",
            post(forward_graphql(home.home.graphql_endpoint().to_owned())),
        )
        .route(
            "/api/v0/acp/node/explain",
            get(forward_explain(
                home.home
                    .graphql_endpoint()
                    .replace("/api/v0/graphql", "/api/v0/acp/node/explain"),
            )),
        );
        let _server = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        }));
        let request = WireRequest {
            plugin: "acme/tool".into(),
            agent: binding.agent_did.clone(),
            targets_from: Vec::new(),
            remove: false,
            preview: false,
            tool_calls_consent: false,
        };
        let client_home = tempfile::tempdir().unwrap();
        let unplanned = crate::http::pack_wire_client::wire_as_client(
            crate::test_support::user_endpoint(&graphql, &home.user),
            owner.clone(),
            client_home.path().to_owned(),
            vec![gents_pack::pack::plugin_wire::ClientWire {
                request: request.clone(),
                plan: None,
            }],
        )
        .await
        .expect_err("a client whose own home holds no record has nothing to plan from");
        assert!(
            format!("{unplanned:#}").contains("is not installed"),
            "{unplanned:#}"
        );
        let sent = gents_pack::pack::wire_subject::WireSubject::of_record(&installed)
            .expect("an installed pack plugin names its pack");
        let endpoint = crate::test_support::user_endpoint(&graphql, &home.user);
        let approvals_root = tempfile::tempdir().unwrap();
        let approvals = gents_pack::pack::approvals::Approvals::under(approvals_root.path());
        let approve = |subject: gents_pack::pack::wire_subject::WireSubject| {
            gents_pack::pack::approvals::PackApproval {
                digest: subject.owner_pack_digest.clone(),
                coordinate: subject.owner_pack_coordinate.clone(),
                version: subject.version.clone(),
                kind: gents_pack::pack::PackKind::Plugins,
                approved_at: "2026-11-19T00:00:00Z".into(),
                subjects: vec![subject],
                yields: Some(Default::default()),
            }
        };
        let refused = |error: anyhow::Error| {
            let error = format!("{error:#}");
            assert!(
                error.contains("gents config pack approve"),
                "{document_acp:?}: {error}"
            );
        };
        refused(
            crate::http::pack_wire_client::served_plan(&endpoint, &owner, &request, &approvals)
                .await
                .expect_err("a client that approved nothing plans nothing"),
        );
        for unrelated in [
            gents_pack::pack::wire_subject::WireSubject {
                owner_pack_digest: format!("sha256:{}", "c".repeat(64)),
                ..sent.clone()
            },
            gents_pack::pack::wire_subject::WireSubject {
                owner_pack_coordinate: "acme/other".into(),
                ..sent.clone()
            },
        ] {
            let other_root = tempfile::tempdir().unwrap();
            let other = gents_pack::pack::approvals::Approvals::under(other_root.path());
            other.record(&owner, approve(unrelated)).await.unwrap();
            refused(
                crate::http::pack_wire_client::served_plan(&endpoint, &owner, &request, &other)
                    .await
                    .expect_err("a record of a pack this client never approved plans nothing"),
            );
        }
        let mut altered = sent.clone();
        altered.declaration.description = "altered after approval".into();
        let altered_root = tempfile::tempdir().unwrap();
        let altered_approvals = gents_pack::pack::approvals::Approvals::under(altered_root.path());
        altered_approvals
            .record(&owner, approve(altered.clone()))
            .await
            .unwrap();
        refused(
            crate::http::pack_wire_client::served_plan(
                &endpoint,
                &owner,
                &request,
                &altered_approvals,
            )
            .await
            .expect_err("a record other than the one the approved digest yields plans nothing"),
        );
        let plan = if document_acp == DocumentAcp::Vera {
            approvals
                .record(&owner, approve(sent.clone()))
                .await
                .unwrap();
            crate::http::pack_wire_client::served_plan(&endpoint, &owner, &request, &approvals)
                .await
                .unwrap()
        } else {
            gents_pack::pack::wire_subject::wire_plan_record(
                &request.plugin,
                &[],
                gents_pack::pack::wire_subject::WireRecord::SentPack(sent.clone()),
            )
            .map(|record| gents_pack::pack::plugin_wire::ClientPlan {
                record,
                approvals: None,
            })
            .unwrap()
        };
        let report = crate::http::pack_wire_client::wire_as_client(
            endpoint.clone(),
            owner.clone(),
            client_home.path().to_owned(),
            vec![gents_pack::pack::plugin_wire::ClientWire {
                request: request.clone(),
                plan: Some(plan),
            }],
        )
        .await
        .unwrap();
        let [report] = report.as_slice() else {
            panic!("{document_acp:?}: one wire reports once: {report:?}");
        };
        assert_eq!(report["preview"], false, "{document_acp:?}: {report}");
        let rows = home
            .access
            .execute("{ Tools { tools_id integrations } }")
            .await
            .unwrap();
        assert!(
            rows["data"]["Tools"].as_array().unwrap().iter().any(|row| {
                row["tools_id"] == "client-wire-tools"
                    && row["integrations"]["plugins"]
                        .as_array()
                        .is_some_and(|plugins| {
                            plugins.iter().any(|entry| {
                                entry["plugin"] == "acme/tool"
                                    && entry["digest"] == installed.digest
                            })
                        })
            }),
            "{document_acp:?}: {rows}"
        );
        let outside = crate::http::pack_wire_client::wire_as_client(
            endpoint.clone(),
            owner.clone(),
            client_home.path().to_owned(),
            vec![gents_pack::pack::plugin_wire::ClientWire {
                request: request.clone(),
                plan: Some(gents_pack::pack::plugin_wire::ClientPlan {
                    record: gents_pack::pack::wire_subject::WireSubject {
                        digest: format!("sha256:{}", "d".repeat(64)),
                        ..sent.clone()
                    }
                    .installed(),
                    approvals: None,
                }),
            }],
        )
        .await;
        assert!(
            outside.is_err(),
            "{document_acp:?}: a write the runtime proposes outside the client's plan is not signed"
        );
        let after = home
            .access
            .execute("{ Tools { tools_id integrations } }")
            .await
            .unwrap();
        assert_eq!(
            after, rows,
            "{document_acp:?}: the refused wire wrote nothing"
        );
    }
}

#[tokio::test]
async fn operator_inventory_authenticates_and_reads_native_metadata() {
    use gents_protocol::file_inventory::{
        DirectFileInventoryRequest, FileInventoryObservation, FileInventoryReadAction,
        FILE_INVENTORY_OPERATOR_PATH,
    };
    use gents_store::client_journal::paths::DesktopPaths;
    use gents_store::client_journal::principal_identity::PrincipalIdentity;

    let temp = tempfile::tempdir().unwrap();
    // The enrolled User holds the operator grants and configures the Node
    // through its own client-signed endpoint.
    let home = crate::test_support::ClientHome::new().await;
    let identity = home.home.node_identity.clone();
    let user = &home.user;
    let foreign_paths = DesktopPaths::from_root(temp.path().join("foreign"));
    foreign_paths.ensure_root_dirs().await.unwrap();
    let foreign = PrincipalIdentity::load_or_create(&foreign_paths)
        .await
        .unwrap();
    let node = home.node().clone();
    let owner = identity.did();
    // The foreign client may read documents on this Node, but no session of
    // the User's is visible to it.
    let admin = gents_store::config_client::ConfigAccess::local_as(
        node.clone(),
        identity::Did::new(owner).unwrap(),
    );
    for permission in [
        gents_store::config_client::NodePermission::DocumentRead,
        gents_store::config_client::NodePermission::DocumentUpdate,
    ] {
        admin
            .grant_node_permission(&identity::Did::new(foreign.did()).unwrap(), permission)
            .await
            .unwrap();
    }
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();
    let access = home.access.clone();
    let session_id = "operator-http-session";
    let binding = home.agent("fixture").await;
    home.mark_backend_healthy(&binding).await;
    home.agent_session(&binding, session_id).await;
    let runtime = gents::Gents::builder()
        .node(node.clone())
        .backend_admission(home.home.backend_admission())
        .identity(identity.clone())
        .tool_ceiling(gents_tools::surface::ToolCeiling::meta_only())
        .agent("fixture")
        .identity_binding(binding.clone())
        .backend_id(format!("{}:backend", binding.agent_did))
        .model_name("test-model")
        .done()
        .build()
        .await
        .unwrap();
    let mut state = state();
    state.node_did = owner.into();
    assert!(state.activation_runtime.set(runtime).is_ok());
    let app = Router::new()
        .route(
            FILE_INVENTORY_OPERATOR_PATH,
            post(crate::http::file_inventory::operator_inventory_handler),
        )
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    }));
    let graphql = format!("http://{addr}/api/v0/graphql");
    let endpoint = crate::test_support::user_endpoint(&graphql, user);
    let foreign_endpoint = desktop_endpoint(&graphql, &foreign);
    let url = format!("http://{addr}{FILE_INVENTORY_OPERATOR_PATH}");
    let demand = DirectFileInventoryRequest {
        node_did: owner.into(),
        session_id: session_id.into(),
        action: FileInventoryReadAction::default(),
    };
    let response = client.post(&url).json(&demand).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let response = client
        .post(&url)
        .bearer_auth("invalid-token")
        .json(&demand)
        .send()
        .await
        .unwrap();
    assert!(response.status().is_client_error());
    let wrong_audience =
        crate::test_support::user_endpoint("http://other.invalid/api/v0/graphql", user)
            .runtime_authorization()
            .await
            .unwrap()
            .unwrap();
    let response = client
        .post(&url)
        .header(header::AUTHORIZATION, wrong_audience)
        .json(&demand)
        .send()
        .await
        .unwrap();
    assert!(response.status().is_client_error());
    let response = foreign_endpoint
        .authorize_runtime(client.post(&url))
        .await
        .unwrap()
        .json(&demand)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response
        .json::<Option<FileInventoryObservation>>()
        .await
        .unwrap()
        .is_none());
    let mut retained = None;
    for _ in 0..2 {
        let response = endpoint
            .authorize_runtime(client.post(&url))
            .await
            .unwrap()
            .json(&demand)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
        let reply = response
            .json::<Option<FileInventoryObservation>>()
            .await
            .unwrap()
            .unwrap();
        let page = reply.current.as_ref().unwrap();
        assert_eq!(page.binding.coverage.entries, 0);
        assert_eq!(page.binding.coverage.pending, 0);
        assert_eq!(page.binding.coverage.unknown, 0);
        if let Some(prior) = &retained {
            assert_eq!(&reply, prior);
        }
        retained = Some(reply);
    }
    let mut wrong_runtime = demand.clone();
    wrong_runtime.node_did = foreign.did().into();
    let response = endpoint
        .authorize_runtime(client.post(&url))
        .await
        .unwrap()
        .json(&wrong_runtime)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let mut missing = demand.clone();
    missing.session_id.push_str("-missing");
    let response = endpoint
        .authorize_runtime(client.post(&url))
        .await
        .unwrap()
        .json(&missing)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response
        .json::<Option<FileInventoryObservation>>()
        .await
        .unwrap()
        .is_none());
    drop(server);
    drop(access);
    node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn inference_settings_authenticates_the_client_before_reading_configuration() {
    use gents_observe::client_inference::INFERENCE_SETTINGS_PATH;
    use gents_store::client_journal::paths::DesktopPaths;
    use gents_store::client_journal::principal_identity::PrincipalIdentity;
    let temp = tempfile::tempdir().unwrap();
    let paths = DesktopPaths::from_root(temp.path());
    paths.ensure_root_dirs().await.unwrap();
    let identity = PrincipalIdentity::load_or_create(&paths).await.unwrap();
    let app = Router::new()
        .route(
            INFERENCE_SETTINGS_PATH,
            get(crate::http::inference::inference_settings_handler),
        )
        .with_state(state());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let url = format!("http://{addr}{INFERENCE_SETTINGS_PATH}");
    let client = reqwest::Client::new();
    let response = client.get(&url).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    let response = client
        .get(&url)
        .bearer_auth("invalid-token")
        .send()
        .await
        .unwrap();
    assert!(response.status().is_client_error());
    let foreign_audience = desktop_endpoint("http://other.invalid/api/v0/graphql", &identity)
        .runtime_authorization()
        .await
        .unwrap()
        .unwrap();
    let response = client
        .get(&url)
        .header(header::AUTHORIZATION, foreign_audience)
        .send()
        .await
        .unwrap();
    assert!(response.status().is_client_error());
    let endpoint = desktop_endpoint(format!("http://{addr}/api/v0/graphql"), &identity);
    let response = endpoint
        .authorize_runtime(client.get(&url))
        .await
        .unwrap()
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-store");
    assert_eq!(
        response.json::<Value>().await.unwrap()["error"],
        "The runtime is still starting. Try again shortly."
    );
    server.abort();
}

#[test]
fn activation_observation_requires_one_successful_exact_tuple() {
    let observed = RuntimeActivationObservation {
        event: Some((4, "desired".into(), Ok(()))),
        router: Some((4, "desired".into())),
    };
    assert!(observed.successful_for(4, "desired"));
    assert!(!observed.successful_for(5, "desired"));
    assert!(!observed.successful_for(4, "other"));

    let failed = RuntimeActivationObservation {
        event: Some((4, "desired".into(), Err("seed failed".into()))),
        router: Some((4, "desired".into())),
    };
    assert!(!failed.successful_for(4, "desired"));

    let torn = RuntimeActivationObservation {
        event: Some((4, "desired".into(), Ok(()))),
        router: Some((5, "desired".into())),
    };
    assert!(!torn.successful_for(4, "desired"));
}

fn runtime() -> MetricsRuntimeRow {
    MetricsRuntimeRow {
        node_did: "did:key:zAgent".to_string(),
        reconcile_phase: "idle".to_string(),
        last_reconcile_result: "applied".to_string(),
        last_reconcile_completed_at: "2026-06-04T00:00:00Z".to_string(),
    }
}

fn readiness(default_agent_did: &str) -> NodeReadinessRow {
    NodeReadinessRow {
        node_did: "did:key:zAgent".to_string(),
        snapshot_json: serde_json::to_string(&NodeReadinessSnapshot {
            format_version: NODE_READINESS_FORMAT_VERSION,
            process_state: NodeReadinessProcessState::Ready,
            active_generation: 1,
            router_generation: 1,
            default_agent_did: default_agent_did.to_string(),
            agents: vec![AgentReadinessEntry {
                agent_did: default_agent_did.to_string(),
                state: AgentReadinessState::Ready,
                reason: None,
                effective_inference: None,
            }],
        })
        .unwrap(),
        updated_at: "2026-06-04T00:00:00Z".to_string(),
    }
}

#[test]
fn p2p_metrics_snapshot_decodes_pinned_live_sync_status() {
    let mut sync_status = gents_p2p::p2p::observability::P2pSyncStatusSnapshot::default();
    sync_status.push_backlog.queued_items = 7;
    sync_status.push_backlog.stale_head_retirements_total = 17;
    sync_status.push_backlog.peer_capacity_parks_total = 13;
    sync_status.push_backlog.per_peer =
        vec![gents_p2p::p2p::observability::P2pPeerBacklogSnapshot {
            peer_id: "peer-a".to_string(),
            consecutive_failures: 3,
            ..Default::default()
        }];
    sync_status.push_updates_coalesced_total = 43;
    sync_status.persisted_pending_dags = 17;
    sync_status.missing_link_retries = 23;
    sync_status.gossip_direction_filtered_total = 47;
    sync_status.pending_dag_capacity_shed = 59;
    sync_status.next_pending_retry_in_ms = Some(71);
    sync_status.pending_dag_terminal_quarantined = 73;
    sync_status.quarantined_pending_dags = 79;
    let snapshot = p2p_metrics_from_status(
        &json!({
            "enabled": true,
            "p2p_connected_peers": ["peer-a", "peer-b"],
            "p2p_replicator_count": 2,
            "p2p_sync_status": serde_json::to_value(sync_status)
                .expect("serialize pinned sync status")
        }),
        None,
    );

    assert_eq!(snapshot.connected_peers, 2);
    assert_eq!(snapshot.replicators, 2);
    let sync = snapshot.sync_status.expect("valid pinned sync status");
    assert_eq!(sync.push_backlog.queued_items, 7);
    assert_eq!(sync.push_backlog.stale_head_retirements_total, 17);
    assert_eq!(sync.push_backlog.peer_capacity_parks_total, 13);
    assert_eq!(sync.push_backlog.per_peer[0].consecutive_failures, 3);
    assert_eq!(sync.push_updates_coalesced_total, 43);
    assert_eq!(sync.persisted_pending_dags, 17);
    assert_eq!(sync.missing_link_retries, 23);
    assert_eq!(sync.gossip_direction_filtered_total, 47);
    assert_eq!(sync.pending_dag_capacity_shed, 59);
    assert_eq!(sync.next_pending_retry_in_ms, Some(71));
    assert_eq!(sync.pending_dag_terminal_quarantined, 73);
    assert_eq!(sync.quarantined_pending_dags, 79);
}

#[tokio::test]
async fn status_lifecycle_is_starting_until_serve_marks_it_ready() {
    async fn lifecycle(state: &RuntimeHttpState) -> Value {
        let response = status_handler(State(state.clone()), &state.p2p_graphql, None).await;
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("status body");
        let body = serde_json::from_slice::<Value>(&body).expect("status json");
        assert_eq!(
            body[gents_tools::plugin::approval::workspace::STATUS_SUPPORT_FIELD],
            json!(gents_tools::plugin::approval::workspace::support()),
        );
        body[gents_protocol::serve_lifecycle::STATUS_LIFECYCLE_FIELD].clone()
    }
    let mut state = state();
    state.graphql = GraphqlEndpoint::anonymous("http://127.0.0.1:9/api/v0/graphql");
    state.p2p_graphql = state.graphql.clone();

    assert_eq!(lifecycle(&state).await, json!("starting"));
    state.serve_lifecycle.mark_ready();
    assert_eq!(lifecycle(&state).await, json!("ready"));
}

async fn stalled_activity_state(
    core_gate: Option<(
        std::sync::Arc<tokio::sync::Notify>,
        std::sync::Arc<tokio::sync::Notify>,
    )>,
) -> (
    RuntimeHttpState,
    gents_store::test_support::wire_database::WireDatabase,
) {
    let claimed_at = chrono::Utc::now() - chrono::Duration::seconds(120);
    let database = gents_store::test_support::wire_database::WireDatabase::spawn(
        json!({
            "NodeRuntime": [serde_json::to_value(runtime()).unwrap()],
            "NodeReadiness": [serde_json::to_value(readiness("default")).unwrap()],
            "AgentRequest": [{
                "_docID": "doc-req-1",
                "request_id": "req-1",
                "node_did": "did:key:zAgent",
                "lifecycle_state": "processing",
                "claimed_at": claimed_at.to_rfc3339(),
                "deadline": (chrono::Utc::now() + chrono::Duration::hours(1)).to_rfc3339(),
            }],
        }),
        std::sync::Arc::new(move |collection| {
            let core_gate = core_gate.clone();
            Box::pin(async move {
                match collection.as_str() {
                    "NodeRuntime" => {
                        if let Some((entered, release)) = core_gate {
                            entered.notify_one();
                            release.notified().await;
                        }
                    }
                    "InferenceCall" => std::future::pending::<()>().await,
                    _ => {}
                }
                Ok(())
            })
        }),
    )
    .await;
    let mut state = state();
    state.graphql = database.reader();
    state.p2p_graphql = state.graphql.clone();
    (state, database)
}

async fn health_body(response: Response) -> (StatusCode, Value) {
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("health body");
    (status, serde_json::from_slice(&bytes).expect("health json"))
}

fn progress_age_ms(body: &Value) -> i64 {
    body.pointer("/liveness/requests/0/last_progress_age_ms")
        .and_then(Value::as_i64)
        .expect("liveness request row")
}

#[tokio::test]
async fn stalled_liveness_activity_read_does_not_hold_health() {
    let (state, _database) = stalled_activity_state(None).await;

    let started = std::time::Instant::now();
    let (status, healthz) = health_body(healthz_handler(State(state.clone())).await).await;
    assert!(
        started.elapsed() < Duration::from_millis(1_500),
        "/healthz waited {:?} on a stalled activity read",
        started.elapsed()
    );
    assert_eq!(status, StatusCode::OK, "{healthz}");
    assert_eq!(healthz["ok"], json!(true), "{healthz}");
    assert!((120_000..180_000).contains(&progress_age_ms(&healthz)));

    let started = std::time::Instant::now();
    let (_, status_body) =
        health_body(status_handler(State(state.clone()), &state.p2p_graphql, None).await).await;
    assert!(
        started.elapsed() < STATUS_PROBE_BUDGET + P2P_METRICS_FETCH_BUDGET,
        "/status waited {:?}",
        started.elapsed()
    );
    assert_eq!(status_body["ok"], json!(true), "{status_body}");
    assert!((120_000..180_000).contains(&progress_age_ms(&status_body)));
}

#[tokio::test]
async fn late_core_read_leaves_status_ok_when_activity_stalls() {
    let entered = std::sync::Arc::new(tokio::sync::Notify::new());
    let release = std::sync::Arc::new(tokio::sync::Notify::new());
    let (state, _database) = stalled_activity_state(Some((entered.clone(), release.clone()))).await;

    let started = std::time::Instant::now();
    let status_fut = async {
        health_body(status_handler(State(state.clone()), &state.p2p_graphql, None).await).await
    };
    tokio::pin!(status_fut);
    let (_, status_body) = tokio::select! {
        biased;
        result = &mut status_fut => result,
        () = entered.notified() => {
            tokio::time::pause();
            tokio::time::advance(Duration::from_millis(1_700)).await;
            release.notify_one();
            tokio::time::resume();
            status_fut.await
        }
    };
    assert!(
        started.elapsed() < STATUS_PROBE_BUDGET + P2P_METRICS_FETCH_BUDGET,
        "/status waited {:?}",
        started.elapsed()
    );
    assert_eq!(status_body["ok"], json!(true), "{status_body}");
    assert!((120_000..180_000).contains(&progress_age_ms(&status_body)));
}

#[tokio::test]
async fn disabled_p2p_metrics_skip_live_status_fetch() {
    let mut state = state();
    state.graphql = GraphqlEndpoint::anonymous("http://127.0.0.1:9/api/v0/graphql");
    state.p2p_graphql = state.graphql.clone();

    let snapshot = load_p2p_metrics_for_scrape(&state).await;

    assert!(!snapshot.enabled);
    assert!(!snapshot.stale);
    assert_eq!(snapshot.connected_peers, 0);
    assert_eq!(snapshot.replicators, 0);
    assert!(snapshot.admission.is_none());
    assert!(snapshot.sync_status.is_none());
}

struct UsageServer {
    address: std::net::SocketAddr,
    client: reqwest::Client,
    _server: tokio_util::task::AbortOnDropHandle<()>,
}

impl UsageServer {
    async fn start(state: &RuntimeHttpState) -> Self {
        let app = Router::new()
            .route("/accounts/usage/read", post(account_usage_read_handler))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        }));
        Self {
            address,
            client: reqwest::Client::new(),
            _server: server,
        }
    }

    async fn post(
        &self,
        command: crate::usage_read::UsageReadCommand,
        bearer: Option<&dyn gents_store::identity::NodeIdentity>,
    ) -> (StatusCode, Value) {
        let request = self
            .client
            .post(format!("http://{}/accounts/usage/read", self.address))
            .json(&command);
        let request = if let Some(identity) = bearer {
            GraphqlEndpoint::as_principal(
                format!("http://{}/api/v0/graphql", self.address),
                identity.did(),
            )
            .with_client_signing_capability(identity.client_signing_capability().unwrap())
            .unwrap()
            .authorize_runtime(request)
            .await
            .unwrap()
        } else {
            request
        };
        let response = request.send().await.unwrap();
        let status = response.status();
        (status, response.json().await.expect("usage json"))
    }
}

fn usage_command(signer_did: &str) -> crate::usage_read::UsageReadCommand {
    crate::usage_read::UsageReadCommand {
        trigger: gents_inference::usage_observation::UsageTrigger::Open,
        provider: None,
        signer_did: signer_did.to_string(),
        issued_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        nonce: "nonce-usage-1".to_string(),
        sig: vec![0; 64],
    }
}

#[tokio::test]
async fn account_usage_route_is_unavailable_before_activation() {
    let server = UsageServer::start(&state()).await;
    let (status, body) = server.post(usage_command("did:key:zAgent"), None).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(body["error"].is_string(), "{body}");
}

#[tokio::test]
async fn account_usage_route_refuses_unsigned_and_foreign_signers() {
    let temp = tempfile::tempdir().unwrap();
    let key = |name: &str| -> Arc<dyn gents_store::identity::NodeIdentity> {
        Arc::new(
            gents_store::identity::KeyIdentity::load_or_create(temp.path().join(name), None)
                .unwrap(),
        )
    };
    // The owner is the enrolled User that published the Node; the runtime
    // authenticates its signed reads against the Node it can see.
    let home = crate::test_support::ClientHome::new().await;
    let identity = home.home.node_identity.clone();
    let owner = home.user_signer();
    let other = key("other.key");
    let node = home.node().clone();
    let mut state = state();
    state.node_did = identity.did().into();
    *state.enrollment_decisions.write().await = Some(
        crate::http::enrollment::EnrollmentDecisionService::new(identity.clone(), node),
    );
    let server = UsageServer::start(&state).await;
    let signed = |identity: &Arc<dyn gents_store::identity::NodeIdentity>| {
        let identity = identity.clone();
        async move {
            crate::usage_read::UsageReadCommand::signed(
                identity.as_ref(),
                gents_inference::usage_observation::UsageTrigger::Open,
                None,
            )
            .await
            .unwrap()
        }
    };

    let (status, body) = server
        .post(usage_command(owner.did()), Some(owner.as_ref()))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "unsigned: {body}");
    let (status, body) = server
        .post(signed(&other).await, Some(other.as_ref()))
        .await;
    assert_eq!(status, StatusCode::FORBIDDEN, "foreign: {body}");

    let command = signed(&owner).await;
    let (status, body) = server.post(command.clone(), Some(other.as_ref())).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "bearer mismatch: {body}");
    let (status, body) = server.post(command.clone(), Some(owner.as_ref())).await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE, "signed: {body}");
    let (status, body) = server.post(command, Some(owner.as_ref())).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "replay: {body}");
}

#[tokio::test]
async fn a_status_read_with_a_nonce_says_whether_the_node_key_signed_it() {
    let mut state = state();
    state.graphql = GraphqlEndpoint::anonymous("http://127.0.0.1:9/api/v0/graphql");
    state.p2p_graphql = state.graphql.clone();
    let app = Router::new()
        .route("/status", get(super::status::status_route))
        .with_state(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let _server = tokio_util::task::AbortOnDropHandle::new(tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    }));
    let read = |query: Option<&str>| {
        let mut request = reqwest::Client::new().get(format!("http://{address}/status"));
        if let Some(nonce) = query {
            request = request.query(&[(gents_protocol::home_attestation::HOME_NONCE_QUERY, nonce)]);
        }
        request.send()
    };
    let field = gents_protocol::home_attestation::STATUS_HOME_ATTESTATION_FIELD;

    let displayed: Value = read(None).await.unwrap().json().await.unwrap();
    assert!(displayed.get(field).is_none(), "{displayed}");

    let nonce = gents_protocol::home_attestation::HomeNonce::from_bytes([3; 32]);
    let answered = read(Some(nonce.as_str())).await.unwrap();
    assert_eq!(
        answered.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-store"
    );
    let answered: Value = answered.json().await.unwrap();
    assert_eq!(
        serde_json::from_value::<gents_protocol::home_attestation::HomeAttestation>(
            answered[field].clone()
        )
        .unwrap(),
        gents_protocol::home_attestation::HomeAttestation::Unsigned {
            reason: gents_protocol::home_attestation::UnsignedHomeStatus::RuntimeNotReady,
        },
        "a runtime that has not started holds no node key to sign with"
    );

    assert_eq!(
        read(Some("not-a-nonce")).await.unwrap().status(),
        StatusCode::BAD_REQUEST
    );
}
