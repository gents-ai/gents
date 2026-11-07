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

fn served_runtimes(
) -> &'static Mutex<RapidHashMap<u16, gents::store::config_client::ClientSigningCapability>> {
    static RUNTIMES: OnceLock<
        Mutex<RapidHashMap<u16, gents::store::config_client::ClientSigningCapability>>,
    > = OnceLock::new();
    RUNTIMES.get_or_init(Default::default)
}

/// The served home's Node key, for fixtures standing in for the runtime's
/// own observations (backend catalogs, readiness history).
pub fn register_served_home(home: &Path, port: u16) -> Result<()> {
    let Some(config) = gents::store::local::home::read_init_config::<Value, Value>(home)? else {
        return Ok(());
    };
    let Some(key_path) = config.key_path.as_deref().filter(|path| !path.is_empty()) else {
        return Ok(());
    };
    let identity = gents::KeyIdentity::load_existing(key_path, None)
        .with_context(|| format!("loading served home key {key_path}"))?;
    served_runtimes()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(port, identity.client_signing_capability()?);
    Ok(())
}

/// Acts as the served runtime's Node; see [`register_served_home`]. Its
/// operational routes (health, status, operator HTTP) admit only that Node.
pub fn served_endpoint(graphql: &str) -> gents::store::config_client::GraphqlEndpoint {
    endpoint_from(served_runtimes(), graphql)
}

/// Test reads and fixture writes on a served home act as the operator: the
/// CLI's User client key held under the CLI process's `HOME` (or its
/// `GENTS_DESKTOP_HOME`), the same key the CLI, desktop and shims sign with.
pub fn register_served_client(cli_home: &Path, envs: &[(&str, &str)], port: u16) -> Result<()> {
    let desktop = envs
        .iter()
        .find(|(name, _)| *name == "GENTS_DESKTOP_HOME")
        .map(|(_, root)| std::path::PathBuf::from(root))
        .unwrap_or_else(|| desktop_root_under(cli_home, envs));
    let paths = gents_desktop_core::client::DesktopPaths::from_root(desktop.join("cli"));
    if !paths.identity_key_path().exists() {
        return Ok(());
    }
    let identity = gents_desktop_core::client::PrincipalIdentity::load_existing(&paths)
        .with_context(|| format!("loading the CLI User key under {}", paths.root().display()))?;
    let capability = gents::store::config_client::ClientSigningCapability::from_ed25519_key(
        identity.private_key_bytes(),
    )?;
    served_principals()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(port, capability);
    Ok(())
}

/// The operator's User DID: the CLI client key a CLI process with this
/// `HOME` signs with.
pub fn cli_user_did(cli_home: &Path) -> Result<String> {
    let paths = gents_desktop_core::client::DesktopPaths::from_root(
        desktop_root_under(cli_home, &[]).join("cli"),
    );
    let identity = gents_desktop_core::client::PrincipalIdentity::load_existing(&paths)
        .with_context(|| format!("loading the CLI User key under {}", paths.root().display()))?;
    Ok(identity.did().to_owned())
}

/// `dirs::data_local_dir()` as the CLI child resolves it from its own `HOME`.
fn desktop_root_under(cli_home: &Path, envs: &[(&str, &str)]) -> std::path::PathBuf {
    let data = if cfg!(target_os = "macos") {
        cli_home.join("Library/Application Support")
    } else {
        envs.iter()
            .find(|(name, _)| *name == "XDG_DATA_HOME")
            .map(|(_, root)| std::path::PathBuf::from(root))
            .unwrap_or_else(|| cli_home.join(".local/share"))
    };
    data.join("gents").join("desktop")
}

/// Acts as the operator's User; see [`register_served_client`].
pub fn served_client_endpoint(graphql: &str) -> gents::store::config_client::GraphqlEndpoint {
    endpoint_from(served_principals(), graphql)
}

fn endpoint_from(
    registry: &Mutex<RapidHashMap<u16, gents::store::config_client::ClientSigningCapability>>,
    graphql: &str,
) -> gents::store::config_client::GraphqlEndpoint {
    let principal = reqwest::Url::parse(graphql)
        .ok()
        .and_then(|url| url.port())
        .and_then(|port| {
            registry
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
    let access =
        gents::store::config_client::ConfigAccess::Graphql(served_client_endpoint(graphql));
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
