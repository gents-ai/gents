use std::sync::Arc;

use serde_json::json;

use crate::store::config_client::{ConfigAccess, GraphqlEndpoint};
use crate::store::identity::NodeIdentity;

pub(crate) struct HeldEvalFixture {
    pub(crate) access: ConfigAccess,
    pub(crate) node_did: String,
    pub(crate) user_did: String,
    pub(crate) other_did: String,
    _node: Arc<crate::defra_node::Node>,
    _user: crate::KeyIdentity,
    _other: crate::KeyIdentity,
    _runtime: crate::KeyIdentity,
    _keys: tempfile::TempDir,
}

impl HeldEvalFixture {
    pub(crate) async fn new() -> anyhow::Result<Self> {
        let keys = tempfile::tempdir()?;
        let user = crate::KeyIdentity::load_or_create(keys.path().join("user.key"), None)?;
        let runtime = crate::KeyIdentity::load_or_create(keys.path().join("runtime.key"), None)?;
        let other = crate::KeyIdentity::load_or_create(keys.path().join("other.key"), None)?;
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        let node = Arc::new(
            crate::defra_node::Node::builder()
                .enable_signing()
                .with_node_identity_did(runtime.did())
                .with_http(crate::defra_node::HttpConfig::with_listener(listener)?)
                .build()
                .await?,
        );
        crate::ensure_runtime_schemas(&node).await?;
        let access = ConfigAccess::Graphql(
            GraphqlEndpoint::as_principal(format!("http://{address}/api/v0/graphql"), user.did())
                .with_client_signing_capability(user.client_signing_capability()?)?
                .with_client_signed_agent_creates(),
        );
        let input = json!({"input": {"node_did": runtime.did()}});
        access
            .transact_write("test.eval.held_user_node", |txn| {
                let input = &input;
                Box::pin(async move {
                    txn.execute_client_signed_create(
                        "mutation($input: NodeMutationInputArg!) { create_Node(input: $input) { _docID } }",
                        input,
                        gents_protocol::schemas::NODE,
                    )
                    .await?;
                    Ok::<(), anyhow::Error>(())
                })
            })
            .await?;
        Ok(Self {
            access,
            node_did: runtime.did().to_owned(),
            user_did: user.did().to_owned(),
            other_did: other.did().to_owned(),
            _node: node,
            _user: user,
            _other: other,
            _runtime: runtime,
            _keys: keys,
        })
    }
}
