use super::wire::wire_method;
use super::*;
use crate::commands::grok_shim::projection::tools::PAGER_WAIT_FOR_EXIT_MESSAGE;
use crate::commands::grok_shim::test_fixtures::seed_canonical_assistant_message;
use crate::commands::grok_shim::test_fixtures::{
    configure_runtime_agent, seed_job_row,
    streaming_backend::{MockStreamingBackend, StreamChunk, StreamPlan, StreamResponse},
};
use crate::commands::grok_shim::turn::wire::PromptBlock;
use gents::config_client::ConfigAccess;
use gents::graphql::{ensure_no_errors, escape_graphql_string};
use tokio::sync::Mutex;

fn bound_model() -> BoundModel {
    BoundModel {
        model_id: "GLM-5.3-NVFP4".to_string(),
        name: "GLM 5.3 NVFP4".to_string(),
        total_context_tokens: 262_144,
    }
}

async fn config() -> (tempfile::TempDir, AcpServiceConfig) {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let identity = gents::KeyIdentity::load_or_create(tempdir.path().join("actor.key"), None)
        .expect("test signing identity");
    let node = Arc::new(
        EmbeddedNode::builder()
            .with_node_identity_did(gents::NodeIdentity::did(&identity))
            .data_path(tempdir.path().join("node"))
            .with_storage_backend(gents::defra_node::StorageBackend::Regolith)
            .build()
            .await
            .expect("embedded node"),
    );
    let plugin_home = tempdir.path().join("home");
    (
        tempdir,
        AcpServiceConfig {
            node,
            node_did: Arc::from("did:test:grok-shim"),
            agent_id: Arc::from("did:test:grok-shim:default"),
            current_model: bound_model(),
            grok_home: None,
            plugin_home,
        },
    )
}

async fn test_service() -> (tempfile::TempDir, AcpService) {
    service_over(false).await
}

/// `signing_node`: the node DID is the registered test signing identity,
/// for owners that author documents as the node (job control's kill).
async fn service_over(signing_node: bool) -> (tempfile::TempDir, AcpService) {
    let (staging, mut config) = config().await;
    let identity = gents::KeyIdentity::load_or_create(staging.path().join("actor.key"), None)
        .expect("test signing identity");
    let actor = ::identity::Did::new(gents::NodeIdentity::did(&identity).to_owned())
        .expect("fixture creator DID");
    if signing_node {
        config.node_did = Arc::from(gents::NodeIdentity::did(&identity));
    }
    gents::schema::ensure_runtime_schemas(config.node.as_ref())
        .await
        .expect("runtime schemas");
    let turns = Arc::new(TurnManager::new(
        config.node.clone(),
        super::super::turn::TurnManagerConfig {
            actor,
            node_did: config.node_did.to_string(),
            agent_id: config.agent_id.to_string(),
            graphql: "http://127.0.0.1:8000/api/v0/graphql".to_string(),
        },
    ));
    let projections = Arc::new(ProjectionEngine::new(
        config.node.clone(),
        super::super::projection::BoundModelContext::new(
            config.current_model.model_id.clone(),
            config.current_model.name.clone(),
            config.current_model.total_context_tokens,
        ),
    ));
    (staging, AcpService::new(config, turns, projections))
}

fn request_payload(method: &str, params: Value) -> String {
    serde_json::to_string(&json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": wire_method(method),
        "params": params,
    }))
    .expect("request payload")
}

fn parse_response(line: &str) -> Value {
    serde_json::from_str(line).expect("response line is JSON")
}

async fn spawn_mock_graphql(node: Arc<EmbeddedNode>) -> String {
    async fn handler(
        axum::extract::State(node): axum::extract::State<Arc<EmbeddedNode>>,
        axum::Json(body): axum::Json<Value>,
    ) -> axum::Json<Value> {
        let query = body
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let response = node.execute(&query).await;
        axum::Json(serde_json::to_value(&response).unwrap_or_default())
    }
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind mock graphql listener");
    let addr = listener
        .local_addr()
        .expect("mock graphql listener address");
    let router = axum::Router::new()
        .route("/", axum::routing::post(handler))
        .with_state(node);
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    format!("http://{addr}/")
}

mod host_commands;
mod live_delegate;
mod models_and_modes;
mod request_validation;
mod runtime_controls;
mod session_load;
mod session_new;
mod wire_errors;
