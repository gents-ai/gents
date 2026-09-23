use anyhow::{Context, Result};
use gents::{defra_node::EmbeddedNode, ConfigAccess};
use serde_json::Value;

/// Runtime observation access. Container trials use the production HTTP adapter;
/// embedded trials retain their existing node and transaction handling.
#[derive(Clone, Copy)]
pub(super) enum RuntimeAccess<'a> {
    Embedded(&'a std::sync::Arc<EmbeddedNode>),
    ControlPlane(&'a ConfigAccess),
}

impl<'a> From<&'a std::sync::Arc<EmbeddedNode>> for RuntimeAccess<'a> {
    fn from(node: &'a std::sync::Arc<EmbeddedNode>) -> Self {
        Self::Embedded(node)
    }
}

impl<'a> From<&'a ConfigAccess> for RuntimeAccess<'a> {
    fn from(access: &'a ConfigAccess) -> Self {
        Self::ControlPlane(access)
    }
}

impl RuntimeAccess<'_> {
    pub(super) async fn timeline(self, request_id: &str) -> Result<gents::RunTimelineRows> {
        match self {
            Self::Embedded(node) => {
                gents::run_timeline_fetch::load_run_timeline_rows(
                    &ConfigAccess::Local(node.clone()),
                    request_id,
                )
                .await
            }
            Self::ControlPlane(access) => {
                gents::run_timeline_fetch::load_run_timeline_rows(access, request_id).await
            }
        }
    }

    pub(super) async fn terminal_answer(self, request_id: &str) -> Result<String> {
        let id = gents::graphql::escape_graphql_string(request_id);
        let response = self.query(&format!(
            r#"{{ AgentRequest(filter: {{ request_id: {{ _eq: "{id}" }} }}) {{ _docID agent_did requester_did lifecycle_state terminal_output }} }}"#
        )).await?;
        let rows = response["AgentRequest"]
            .as_array()
            .context("answer request rows missing")?;
        anyhow::ensure!(
            rows.len() == 1,
            "answer requires one exact physical request"
        );
        let row = &rows[0];
        let state = gents_protocol::request_lifecycle::RequestLifecycleState::parse(
            row["lifecycle_state"]
                .as_str()
                .context("answer lifecycle missing")?,
        )?;
        if !state.is_terminal() {
            return Ok(String::new());
        }
        let selection: gents_protocol::output::TerminalOutput =
            serde_json::from_value(row["terminal_output"].clone())
                .context("terminal answer selection missing or malformed")?;
        let gents_protocol::output::TerminalOutput::Message { message_doc_id } = selection else {
            return Ok(String::new());
        };
        let owner = row["agent_did"].as_str().context("answer owner missing")?;
        let requester = row["requester_did"].as_str();
        let (header, native) = match self {
            Self::Embedded(node) => {
                gents::session::load_canonical_message_from_node(
                    node.as_ref(),
                    &message_doc_id,
                    owner,
                    requester,
                )
                .await?
            }
            Self::ControlPlane(access) => {
                gents::session::load_canonical_message(access, &message_doc_id, owner, requester)
                    .await?
            }
        };
        let request_doc_id = row["_docID"]
            .as_str()
            .context("answer request identity missing")?;
        anyhow::ensure!(
            header.request_doc_id.as_deref() == Some(request_doc_id)
                && matches!(native, gents_protocol::message::Message::Assistant { .. }),
            "terminal answer does not belong to the exact request"
        );
        Ok(gents_protocol::transcript::present_message(&native).body_markdown)
    }

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
async fn terminal_answer_distinguishes_pending_no_message_and_invalid_evidence() {
    use axum::{routing::post, Json, Router};
    use serde_json::json;

    // These are read-adapter observations, not fixtures claiming that a
    // lifecycle transition produced malformed state.
    let base = json!({
        "_docID": "physical-request", "agent_did": "did:test:owner",
        "requester_did": null, "lifecycle_state": "pending", "terminal_output": null
    });
    let mut no_message = base.clone();
    no_message["lifecycle_state"] = json!("completed");
    no_message["terminal_output"] =
        serde_json::to_value(gents_protocol::output::TerminalOutput::NoMessage).unwrap();
    let mut missing_selection = no_message.clone();
    missing_selection["terminal_output"] = Value::Null;
    let mut bad_state = base.clone();
    bad_state["lifecycle_state"] = json!("unknown");

    for (rows, succeeds) in [
        (json!([base.clone()]), true),
        (json!([no_message]), true),
        (json!([missing_selection]), false),
        (json!([bad_state]), false),
        (json!([]), false),
        (json!([base.clone(), base]), false),
    ] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let access = ConfigAccess::Graphql(format!(
            "http://{}/api/v0/graphql",
            listener.local_addr().unwrap()
        ));
        let router = Router::new().route(
            "/api/v0/graphql",
            post(move || {
                let rows = rows.clone();
                async move { Json(json!({"data": {"AgentRequest": rows}})) }
            }),
        );
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        let result = RuntimeAccess::from(&access)
            .terminal_answer("logical-request")
            .await;
        server.abort();
        assert_eq!(result.is_ok(), succeeds, "{result:?}");
        if let Ok(answer) = result {
            assert!(answer.is_empty());
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
    let local = RuntimeAccess::from(&db.node).query(query).await;
    let remote = RuntimeAccess::from(&access).query(query).await;
    server.abort();
    db.node.shutdown().await;
    assert_eq!(local.unwrap(), remote.unwrap());
}
