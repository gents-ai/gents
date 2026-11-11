use anyhow::{Context, Result};
use gents_protocol::row::NodeReadinessRow;
use gents_store::config_client::{ConfigAccess, GraphqlEndpoint};
use serde_json::Value;

pub async fn load_live_node_readiness(
    graphql: &GraphqlEndpoint,
    node_did: &str,
) -> Result<Option<NodeReadinessRow>> {
    load_node_readiness(&ConfigAccess::Graphql(graphql.clone()), node_did).await
}

pub async fn load_node_readiness(
    access: &ConfigAccess,
    node_did: &str,
) -> Result<Option<NodeReadinessRow>> {
    use gents_protocol::document_query::{
        fields::node_readiness as fields, Filter, Query, Selection,
    };
    let rendered = Query::select(
        Selection::new()
            .field(fields::NODE_DID)
            .field(fields::SNAPSHOT_JSON)
            .field(fields::UPDATED_AT),
    )
    .filter(Filter::eq(fields::NODE_DID, node_did))
    .limit(1)
    .render();
    gents_protocol::graphql::graphql_rows_from_response(
        &access.read(&rendered).await?,
        "NodeReadiness",
    )
    .into_iter()
    .next()
    .map(|row| serde_json::from_value(row).context("decoding node readiness row"))
    .transpose()
}

/// A local operator control must not follow a status endpoint to another host
/// or accept an unbounded response before deciding whether to retire a runtime.
pub async fn fetch_local_operator_status(
    graphql: &GraphqlEndpoint,
    timeout: std::time::Duration,
) -> Result<Value> {
    const MAX_STATUS_BYTES: usize = 2 * 1024 * 1024;
    let status_url = runtime_status_url(graphql)?;
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(timeout)
        .build()
        .context("building local runtime status client")?;
    let mut response = graphql
        .authorize(client.get(&status_url))?
        .send()
        .await
        .context("reading local runtime status")?;
    anyhow::ensure!(
        response.status().is_success(),
        "local runtime status was refused"
    );
    let mut body = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .context("reading local runtime status")?
    {
        anyhow::ensure!(
            body.len().saturating_add(chunk.len()) <= MAX_STATUS_BYTES,
            "local runtime status is too large"
        );
        body.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&body).context("decoding local runtime status")
}

pub fn runtime_status_url(graphql: &GraphqlEndpoint) -> Result<String> {
    let mut url = reqwest::Url::parse(graphql.url()).context("parsing GraphQL endpoint URL")?;
    url.set_path("/status");
    url.set_query(None);
    url.set_fragment(None);
    Ok(url.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn local_operator_status_refuses_redirects_and_oversized_responses() {
        use axum::{routing::get, Router};
        let app = Router::new()
            .route(
                "/status",
                get(|| async { axum::response::Redirect::temporary("/other") }),
            )
            .route("/other", get(|| async { "{}" }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let endpoint = GraphqlEndpoint::anonymous(format!("http://{address}/api/v0/graphql"));
        assert!(
            fetch_local_operator_status(&endpoint, std::time::Duration::from_secs(2))
                .await
                .is_err()
        );
        server.abort();

        let app = Router::new().route("/status", get(|| async { "x".repeat(2 * 1024 * 1024 + 1) }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let endpoint = GraphqlEndpoint::anonymous(format!("http://{address}/api/v0/graphql"));
        assert!(
            fetch_local_operator_status(&endpoint, std::time::Duration::from_secs(2))
                .await
                .is_err()
        );
        server.abort();
    }
}
