use std::collections::HashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;

// Re-export the canonical escaper instead of duplicating it, so test-support
// escaping can never drift from production (audit/code-review finding).
pub use gents::graphql::escape_graphql_string;

fn served_principals() -> &'static Mutex<HashMap<u16, String>> {
    static PRINCIPALS: OnceLock<Mutex<HashMap<u16, String>>> = OnceLock::new();
    PRINCIPALS.get_or_init(Default::default)
}

/// Record the principal of the home a test server serves on `port`, loading
/// its key into this process so fixture requests can sign as it. A served
/// home's node access control refuses anonymous writes; a home that is not
/// initialized yet is left anonymous.
pub fn register_served_home(home: &Path, port: u16) -> Result<()> {
    let Some(config) = gents::home::read_init_config::<Value, Value>(home)? else {
        return Ok(());
    };
    let Some(key_path) = config.key_path.as_deref().filter(|path| !path.is_empty()) else {
        return Ok(());
    };
    let identity = gents::KeyIdentity::load_existing(key_path, None)
        .with_context(|| format!("loading served home key {key_path}"))?;
    served_principals()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(port, gents::NodeIdentity::did(&identity).to_string());
    Ok(())
}

/// `graphql` acting as the principal of the test home served there, if any.
pub fn served_endpoint(graphql: &str) -> gents::config_client::GraphqlEndpoint {
    let principal = reqwest::Url::parse(graphql)
        .ok()
        .and_then(|url| url.port())
        .and_then(|port| {
            served_principals()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .get(&port)
                .cloned()
        });
    match principal {
        Some(did) => gents::config_client::GraphqlEndpoint::as_principal(graphql, did),
        None => gents::config_client::GraphqlEndpoint::anonymous(graphql),
    }
}

pub async fn graphql_query(graphql: &str, query: &str) -> Result<Value> {
    let access = gents::config_client::ConfigAccess::Graphql(served_endpoint(graphql));
    if query.trim_start().starts_with("mutation") {
        access.write("test.fixture", query).await
    } else {
        access.execute(query).await
    }
}

/// Write typed fixture payloads through the production transaction owner.
/// Variable expansion, conflict handling, and commit remain owned by Gents.
pub async fn graphql_mutation_with_variables(
    access: &gents::config_client::ConfigAccess,
    query: &str,
    variables: &Value,
) -> Result<Value> {
    access
        .transact("test.fixture.variables", |txn| {
            let query = query.to_owned();
            let variables = variables.clone();
            Box::pin(async move { txn.execute_with_variables(&query, &variables).await })
        })
        .await
}

pub fn first_graphql_row<'a>(response: &'a Value, field: &str) -> Result<&'a Value> {
    response
        .pointer(&format!("/data/{field}"))
        .and_then(Value::as_array)
        .and_then(|rows| rows.first())
        .ok_or_else(|| anyhow!("missing {field} row in GraphQL response: {response}"))
}

pub fn doc_id_from_create(response: &Value, field: &str) -> Result<String> {
    response
        .pointer(&format!("/data/{field}/0/_docID"))
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .with_context(|| format!("missing _docID in {field} response: {response}"))
}

pub async fn doc_id_for_tools(graphql: &str, tools_id: &str) -> Result<String> {
    let response = graphql_query(
        graphql,
        &format!(
            r#"{{
                Tools(filter: {{ tools_id: {{ _eq: "{}" }} }}, limit: 1) {{
                    _docID
                }}
            }}"#,
            escape_graphql_string(tools_id),
        ),
    )
    .await?;
    first_graphql_row(&response, "Tools")?
        .get("_docID")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .ok_or_else(|| anyhow!("Tools row missing _docID for {tools_id}"))
}

pub async fn exec(node: &gents::defra_node::EmbeddedNode, query: &str) -> Result<()> {
    let response = node.execute(query).await;
    if response.has_errors() {
        bail!("GraphQL mutation failed: {:?}", response.errors);
    }
    Ok(())
}
