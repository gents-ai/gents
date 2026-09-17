use anyhow::{Context, Result};
use gents::{defra_node::EmbeddedNode, ConfigAccess};
use serde_json::Value;

/// Runtime observation access. Container trials use the production HTTP adapter;
/// embedded trials retain their existing node and transaction handling.
#[derive(Clone, Copy)]
pub(super) enum RuntimeAccess<'a> {
    Embedded(&'a EmbeddedNode),
    ControlPlane(&'a ConfigAccess),
}

impl<'a> From<&'a EmbeddedNode> for RuntimeAccess<'a> {
    fn from(node: &'a EmbeddedNode) -> Self {
        Self::Embedded(node)
    }
}

impl<'a> From<&'a ConfigAccess> for RuntimeAccess<'a> {
    fn from(access: &'a ConfigAccess) -> Self {
        Self::ControlPlane(access)
    }
}

impl RuntimeAccess<'_> {
    pub(super) async fn interrupt(self, request_id: &str) -> Result<()> {
        match self {
            Self::Embedded(node) => gents::interrupt_request(node, request_id).await,
            Self::ControlPlane(access) => {
                let id = gents::graphql::escape_graphql_string(request_id);
                let result = self.query(&format!(
                    "{{ AgentRequest(filter: {{ request_id: {{ _eq: \"{id}\" }} }}) {{ _docID agent_did requester_did }} }}"
                )).await?;
                let rows = result["AgentRequest"]
                    .as_array()
                    .context("interrupt request rows missing")?;
                anyhow::ensure!(rows.len() == 1, "interrupt requires one exact request");
                let row = &rows[0];
                gents::interrupt::interrupt_request_by_doc_id_with_access(
                    access,
                    row["_docID"]
                        .as_str()
                        .context("request document ID missing")?,
                    row["agent_did"].as_str().context("request owner missing")?,
                    row["requester_did"].as_str(),
                )
                .await
            }
        }
    }

    pub(super) async fn query(self, query: &str) -> Result<Value> {
        match self {
            Self::Embedded(node) => {
                let response =
                    gents::graphql::graphql_with_transaction_retry(node, query, "eval evidence")
                        .await?;
                response.data.context("evidence query returned no data")
            }
            Self::ControlPlane(access) => {
                let response = access.execute(query).await?;
                response
                    .get("data")
                    .filter(|data| !data.is_null())
                    .cloned()
                    .context("evidence query returned no data")
            }
        }
    }
}

#[tokio::test]
async fn evidence_access_rejects_mutation_before_contacting_runtime() {
    let control = ConfigAccess::Graphql("http://127.0.0.1:1/api/v0/graphql".into());
    let result = RuntimeAccess::from(&control)
        .query("mutation { delete_AgentRequest { _docID } }")
        .await;
    assert!(result.is_err());
    assert!(format!("{:#}", result.unwrap_err()).contains("requires a GraphQL query document"));
}

#[tokio::test]
async fn embedded_and_http_evidence_have_the_same_data_shape() {
    use axum::{extract::State, routing::post, Json, Router};
    use std::sync::Arc;

    async fn query(State(node): State<Arc<EmbeddedNode>>, Json(body): Json<Value>) -> Json<Value> {
        let response = node.execute(body["query"].as_str().unwrap()).await;
        Json(serde_json::json!({"data": response.data, "errors": response.errors}))
    }

    let db = crate::support::test_db("eval-evidence-access").await;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/api/v0/graphql", listener.local_addr().unwrap());
    let router = Router::new()
        .route("/api/v0/graphql", post(query))
        .with_state(db.node.clone());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let access = ConfigAccess::Graphql(endpoint);
    let query = "{ AgentRequest { request_id lifecycle_state } }";
    let local = RuntimeAccess::from(db.node.as_ref()).query(query).await;
    let remote = RuntimeAccess::from(&access).query(query).await;
    server.abort();
    db.node.shutdown().await;
    assert_eq!(local.unwrap(), remote.unwrap());
}
