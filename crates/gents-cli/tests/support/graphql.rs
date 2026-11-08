use rapidhash::RapidHashMap;
use std::path::Path;
use std::sync::{Mutex, OnceLock};

use anyhow::{anyhow, bail, Context, Result};
use serde_json::Value;

pub use gents::store::graphql::escape_graphql_string;

fn served_principals(
) -> &'static Mutex<RapidHashMap<u16, gents::store::config_client::ClientSigningCapability>> {
    static PRINCIPALS: OnceLock<
        Mutex<RapidHashMap<u16, gents::store::config_client::ClientSigningCapability>>,
    > = OnceLock::new();
    PRINCIPALS.get_or_init(Default::default)
}

/// The CLI's client key is the home's User: `gents init` creates the Node and
/// its Agent as that DID, so fixtures observe and author as it, never as the
/// Node key. `user_home` is the `HOME` the CLI processes ran under.
pub fn register_served_home(
    user_home: &Path,
    gents_home: &Path,
    envs: &[(&str, &str)],
    port: u16,
) -> Result<()> {
    register_node_principal(gents_home, port)?;
    let paths = cli_client_paths(user_home, envs);
    if !paths.identity_key_path().exists() {
        return Ok(());
    }
    let identity = gents_desktop_core::client::PrincipalIdentity::load_existing(&paths)
        .with_context(|| format!("loading CLI client key under {}", paths.root().display()))?;
    served_principals()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(
            port,
            gents::store::config_client::ClientSigningCapability::from_ed25519_key(
                identity.private_key_bytes(),
            )?,
        );
    Ok(())
}

fn node_principals(
) -> &'static Mutex<RapidHashMap<u16, gents::store::config_client::ClientSigningCapability>> {
    static PRINCIPALS: OnceLock<
        Mutex<RapidHashMap<u16, gents::store::config_client::ClientSigningCapability>>,
    > = OnceLock::new();
    PRINCIPALS.get_or_init(Default::default)
}

fn register_node_principal(gents_home: &Path, port: u16) -> Result<()> {
    let Some(config) = gents::store::local::home::read_init_config::<Value, Value>(gents_home)?
    else {
        return Ok(());
    };
    let Some(key_path) = config.key_path.as_deref().filter(|path| !path.is_empty()) else {
        return Ok(());
    };
    let identity = gents::KeyIdentity::load_existing(key_path, None)
        .with_context(|| format!("loading served home key {key_path}"))?;
    node_principals()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(port, identity.client_signing_capability()?);
    Ok(())
}

/// The served Node's own principal, which alone may call the runtime's
/// node-principal HTTP routes (`/metrics`, `/self`, `/fleet`, ...).
pub fn node_principal_endpoint(graphql: &str) -> gents::store::config_client::GraphqlEndpoint {
    endpoint_for(graphql, node_principals())
}

/// Mirrors the CLI's client-key discovery for a process run with
/// `HOME=user_home` and `envs`.
pub fn cli_client_paths(
    user_home: &Path,
    envs: &[(&str, &str)],
) -> gents_desktop_core::client::DesktopPaths {
    let desktop_root = envs
        .iter()
        .rev()
        .find(|(name, _)| *name == gents_desktop_core::client::DESKTOP_HOME_ENV)
        .map(|(_, value)| std::path::PathBuf::from(value))
        .or_else(|| {
            std::env::var_os(gents_desktop_core::client::DESKTOP_HOME_ENV)
                .filter(|value| !value.is_empty())
                .map(std::path::PathBuf::from)
        })
        .unwrap_or_else(|| user_data_local_dir(user_home, envs).join("gents").join("desktop"));
    gents_desktop_core::client::DesktopPaths::from_root(desktop_root.join("cli"))
}

/// The operator's User DID: the CLI client key a CLI process with this
/// `HOME` signs with.
pub fn cli_user_did(cli_home: &Path) -> Result<String> {
    let paths = cli_client_paths(cli_home, &[]);
    let identity = gents_desktop_core::client::PrincipalIdentity::load_existing(&paths)
        .with_context(|| format!("loading the CLI User key under {}", paths.root().display()))?;
    Ok(identity.did().to_owned())
}

#[cfg(target_os = "macos")]
fn user_data_local_dir(user_home: &Path, _envs: &[(&str, &str)]) -> std::path::PathBuf {
    user_home.join("Library").join("Application Support")
}

#[cfg(not(target_os = "macos"))]
fn user_data_local_dir(user_home: &Path, envs: &[(&str, &str)]) -> std::path::PathBuf {
    envs.iter()
        .rev()
        .find(|(name, _)| *name == "XDG_DATA_HOME")
        .map(|(_, value)| std::path::PathBuf::from(value))
        .or_else(|| std::env::var_os("XDG_DATA_HOME").map(std::path::PathBuf::from))
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| user_home.join(".local").join("share"))
}

pub fn served_endpoint(graphql: &str) -> gents::store::config_client::GraphqlEndpoint {
    endpoint_for(graphql, served_principals())
}

fn endpoint_for(
    graphql: &str,
    principals: &Mutex<RapidHashMap<u16, gents::store::config_client::ClientSigningCapability>>,
) -> gents::store::config_client::GraphqlEndpoint {
    let principal = reqwest::Url::parse(graphql)
        .ok()
        .and_then(|url| url.port())
        .and_then(|port| {
            principals
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .get(&port)
                .cloned()
        });
    match principal {
        Some(capability) => {
            gents::store::config_client::GraphqlEndpoint::as_principal(graphql, capability.did())
                .with_client_signing_capability(capability)
                .expect("served endpoint uses the capability's own DID")
        }
        None => gents::store::config_client::GraphqlEndpoint::anonymous(graphql),
    }
}

/// Document reads and fixture writes as the operator's User.
pub async fn graphql_query(graphql: &str, query: &str) -> Result<Value> {
    execute_as(served_endpoint(graphql), query).await
}

/// Seeds or reads rows the runtime itself owns and observes as its Node.
pub async fn node_graphql_query(graphql: &str, query: &str) -> Result<Value> {
    execute_as(node_principal_endpoint(graphql), query).await
}

async fn execute_as(
    endpoint: gents::store::config_client::GraphqlEndpoint,
    query: &str,
) -> Result<Value> {
    let access = gents::store::config_client::ConfigAccess::Graphql(endpoint);
    if query.trim_start().starts_with("mutation") {
        Ok(access.write("test.fixture", query).await?)
    } else {
        Ok(access.execute(query).await?)
    }
}

pub async fn graphql_mutation_with_variables(
    access: &gents::store::config_client::ConfigAccess,
    query: &str,
    variables: &Value,
) -> Result<Value> {
    Ok(access
        .transact_write("test.fixture.variables", |txn| {
            let query = query.to_owned();
            let variables = variables.clone();
            Box::pin(async move { txn.execute_with_variables(&query, &variables).await })
        })
        .await?)
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

pub async fn exec(node: &gents::defra_node::Node, query: &str) -> Result<()> {
    let response = node.execute(query).await;
    if response.has_errors() {
        bail!("GraphQL mutation failed: {:?}", response.errors);
    }
    Ok(())
}
