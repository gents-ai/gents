use std::net::SocketAddr;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use gents::agent::p2p_reconcile::{EmbeddedRemoteP2pAdmin, RemoteP2pAdmin};
use gents::config_client::ConfigAccess;
use gents::defra_node::{EmbeddedNode, HttpConfig, StorageBackend};
use gents::{KeyIdentity, NodeIdentity};

const SCHEMA: &str = "type NodeAccessProbe { name: String }";

fn free_address() -> SocketAddr {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    drop(listener);
    address
}

async fn served_home(data_dir: &Path, principal: &str, address: SocketAddr) -> Arc<EmbeddedNode> {
    let node = EmbeddedNode::builder()
        .data_path(data_dir)
        .with_storage_backend(StorageBackend::Regolith)
        .with_http(HttpConfig::with_addr(address))
        .with_p2p(crate::support::test_p2p_config(
            &crate::support::TestP2pAdmission::default(),
            data_dir,
        ))
        .with_node_identity_did(principal)
        .with_node_acp_enabled()
        .build()
        .await
        .expect("served home node");
    let endpoint = format!("http://{address}/api/v0/graphql");
    tokio::time::timeout(Duration::from_secs(10), async {
        while !gents_protocol::graphql::graphql_endpoint_available(
            &endpoint,
            gents_protocol::graphql::GraphqlRequestOptions {
                timeout: Duration::from_secs(1),
                max_attempts: 1,
                retry_backoff: Duration::ZERO,
            },
        )
        .await
        {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("served home HTTP listener");
    Arc::new(node)
}

fn insert(name: &str) -> String {
    format!(
        r#"mutation {{ create_NodeAccessProbe(input: {{ name: "{}" }}) {{ _docID }} }}"#,
        gents::graphql::escape_graphql_string(name)
    )
}

async fn names(access: &ConfigAccess) -> Result<Vec<String>> {
    let response = access
        .execute("{ NodeAccessProbe { name } }")
        .await
        .context("reading probe rows")?;
    let mut names: Vec<String> = response["data"]["NodeAccessProbe"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|row| row["name"].as_str().map(ToOwned::to_owned))
        .collect();
    names.sort();
    Ok(names)
}

async fn assert_access_control(
    node: &Arc<EmbeddedNode>,
    principal: &str,
    address: SocketAddr,
    phase: &str,
) {
    let endpoint = format!("http://{address}/api/v0/graphql");
    let anonymous = ConfigAccess::graphql(endpoint.clone());
    let refused = anonymous
        .write("test.nac.anonymous", &insert("anonymous"))
        .await
        .expect_err("an anonymous HTTP mutation must be refused");
    assert!(
        refused.to_string().contains("not authorized"),
        "unexpected refusal: {refused:#}"
    );
    let refused_schema = anonymous
        .add_schema("type NodeAccessIntruder { name: String }")
        .await
        .expect_err("an anonymous HTTP schema change must be refused");
    assert!(
        refused_schema.to_string().contains("401"),
        "unexpected schema refusal: {refused_schema:#}"
    );

    let signed = ConfigAccess::graphql_as(endpoint, principal);
    signed
        .write("test.nac.signed", &insert(&format!("signed-{phase}")))
        .await
        .expect("the home principal may write over HTTP");
    signed
        .transact("test.nac.signed_txn", |txn| {
            Box::pin(async move { txn.execute(&insert(&format!("signed-txn-{phase}"))).await })
        })
        .await
        .expect("the home principal may write in an HTTP transaction");

    let admin = EmbeddedRemoteP2pAdmin::new(Arc::clone(node));
    admin
        .add_p2p_collections(&["NodeAccessProbe".to_string()])
        .await
        .expect("in-process P2P administration acts as the node");
    assert!(admin
        .list_p2p_collections()
        .await
        .expect("in-process P2P listing acts as the node")
        .iter()
        .any(|collection| !collection.is_empty()));
}

/// Serve the home for one phase on its own runtime. Dropping the runtime
/// stops the node's detached HTTP and P2P tasks, which is what releases the
/// store for the next opener.
fn serve_phase(
    data_dir: &Path,
    principal: &str,
    phase: impl for<'a> FnOnce(
        &'a Arc<EmbeddedNode>,
        SocketAddr,
    )
        -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<()>> + 'a>>,
) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()?;
    let result = runtime.block_on(async {
        let address = free_address();
        let node = served_home(data_dir, principal, address).await;
        let result = phase(&node, address).await;
        node.shutdown().await;
        result
    });
    drop(runtime);
    result
}

/// A NAC-enabled served home refuses anonymous HTTP writes and schema
/// changes, admits its principal's signed requests and the node's own
/// in-process P2P administration, and keeps doing so after the existing store
/// is reopened.
#[test]
fn served_home_admits_only_its_principal_across_restart() -> Result<()> {
    let home = tempfile::tempdir()?;
    let identity = KeyIdentity::load_or_create(home.path().join("principal.key"), None)?;
    let principal = identity.did().to_string();
    let data_dir = home.path().join("data");

    serve_phase(&data_dir, &principal, |node, address| {
        let principal = principal.clone();
        Box::pin(async move {
            node.add_schema(SCHEMA).await?;
            assert_access_control(node, &principal, address, "first").await;
            Ok(())
        })
    })?;
    serve_phase(&data_dir, &principal, |node, address| {
        let principal = principal.clone();
        Box::pin(async move {
            assert_access_control(node, &principal, address, "restarted").await;
            let signed =
                ConfigAccess::graphql_as(format!("http://{address}/api/v0/graphql"), &principal);
            assert_eq!(
                names(&signed).await?,
                [
                    "signed-first",
                    "signed-restarted",
                    "signed-txn-first",
                    "signed-txn-restarted"
                ]
            );
            Ok(())
        })
    })
}
