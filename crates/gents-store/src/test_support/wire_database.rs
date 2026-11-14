use serde_json::Value;

pub type CollectionHook = std::sync::Arc<
    dyn Fn(String) -> futures::future::BoxFuture<'static, Result<(), axum::http::StatusCode>>
        + Send
        + Sync,
>;

/// A database answered on the wire for an owner whose reads are typed views
/// in read-only transactions: a transaction begins, runs and ends as DefraDB's
/// HTTP API has it, and each view root is answered with the rows held for its
/// collection, whatever the view's position or filter; a collection held as
/// `null` has its root omitted, as native ACP hides one. `hook` runs before a
/// collection is answered and can delay it or fail the request.
pub struct WireDatabase {
    pub graphql: String,
    server: tokio::task::JoinHandle<()>,
    _reader: tempfile::TempDir,
    reader_did: String,
}

impl WireDatabase {
    pub async fn spawn(rows: Value, hook: CollectionHook) -> Self {
        use crate::identity::NodeIdentity as _;
        use axum::routing::post;

        let state = std::sync::Arc::new((rows, hook));
        let router = axum::Router::new()
            .route(
                "/api/v0/tx",
                post(|| async { axum::Json(serde_json::json!({ "id": "wire" })) }),
            )
            .route(
                "/api/v0/tx/{id}",
                post(|| async { axum::Json(serde_json::json!({})) })
                    .delete(|| async { axum::Json(serde_json::json!({})) }),
            )
            .route("/api/v0/graphql", post(answer_views))
            .with_state(state);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let graphql = format!("http://{}/api/v0/graphql", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, router).await;
        });
        let reader = tempfile::tempdir().unwrap();
        let reader_did =
            crate::identity::KeyIdentity::load_or_create(reader.path().join("reader.key"), None)
                .unwrap()
                .did()
                .to_owned();
        Self {
            graphql,
            server,
            _reader: reader,
            reader_did,
        }
    }

    pub async fn serving(rows: Value) -> Self {
        Self::spawn(rows, std::sync::Arc::new(|_| Box::pin(async { Ok(()) }))).await
    }

    /// An endpoint that reads as an explicit actor, as every view requires.
    pub fn reader(&self) -> crate::config_client::GraphqlEndpoint {
        crate::config_client::GraphqlEndpoint::with_authenticated_delegation(
            self.graphql.clone(),
            "Bearer fixture",
            self.reader_did.parse().unwrap(),
        )
        .unwrap()
    }
}

impl Drop for WireDatabase {
    fn drop(&mut self) {
        self.server.abort();
    }
}

async fn answer_views(
    axum::extract::State(state): axum::extract::State<std::sync::Arc<(Value, CollectionHook)>>,
    axum::Json(body): axum::Json<Value>,
) -> Result<axum::Json<Value>, axum::http::StatusCode> {
    let (rows, hook) = state.as_ref();
    let query = body["query"].as_str().unwrap_or_default();
    for (_, collection) in view_roots(query) {
        hook(collection.to_owned()).await?;
    }
    Ok(axum::Json(answer(rows, query)))
}

fn view_roots(query: &str) -> Vec<(&str, &str)> {
    query
        .match_indices("view_")
        .filter_map(|(start, _)| {
            let (alias, rest) = query[start..].split_once(": ")?;
            let collection = rest
                .split(|character: char| !character.is_ascii_alphanumeric() && character != '_')
                .next()?;
            Some((alias, collection))
        })
        .collect()
}

/// The response to `query` from `rows`, as [`WireDatabase`] answers it.
pub fn answer(rows: &Value, query: &str) -> Value {
    let mut data = serde_json::Map::new();
    for (alias, collection) in view_roots(query) {
        match rows.get(collection) {
            Some(Value::Null) => {}
            Some(held) => {
                data.insert(alias.to_owned(), held.clone());
            }
            None => {
                data.insert(alias.to_owned(), Value::Array(Vec::new()));
            }
        }
    }
    serde_json::json!({ "data": data })
}
