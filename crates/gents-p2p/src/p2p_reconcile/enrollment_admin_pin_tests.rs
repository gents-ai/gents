use std::sync::Arc;

use defra_node::Node as EmbeddedNode;
use gents_protocol::document_query::{fields::network_admin_pin, Query, Selection};
use gents_store::config_client::{ConfigAccess, GraphqlEndpoint, NodePermission};
use gents_store::graphql::escape_graphql_string;
use gents_store::identity::NodeIdentity as _;

struct PinFixture {
    node: Arc<EmbeddedNode>,
    runtime: identity::Did,
    runtime_access: ConfigAccess,
    user_access: ConfigAccess,
    parent: String,
    _directory: tempfile::TempDir,
}

impl PinFixture {
    async fn new() -> anyhow::Result<Self> {
        let directory = tempfile::tempdir()?;
        let runtime_path = directory.path().join("runtime");
        let runtime = gents_store::identity::KeyIdentity::load_or_create(&runtime_path, None)?;
        let runtime_key =
            gents_store::identity::load_file_identity(&runtime_path)?.private_key_bytes();
        let user = gents_store::identity::KeyIdentity::load_or_create(
            directory.path().join("user"),
            None,
        )?;
        let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
        let address = listener.local_addr()?;
        let node = Arc::new(
            EmbeddedNode::builder()
                .with_node_identity_did(runtime.did())
                .with_signing_key(defra_node::SigningKey::Ed25519(runtime_key))
                .with_node_acp_enabled()
                .with_http(defra_node::HttpConfig::with_listener(listener)?)
                .with_storage_backend(defra_node::StorageBackend::Regolith)
                .data_path(directory.path().join("database"))
                .build()
                .await?,
        );
        node.add_dac_policy_with_counter_as(
            &defra_core::RequestContext::default().with_identity(Some(runtime.did().to_owned())),
            gents_protocol::schemas::AGENT_OWNERSHIP_POLICY,
            0,
        )
        .await?;
        for collection in [
            gents_protocol::schemas::NODE,
            gents_protocol::schemas::NETWORK_ADMIN_PIN,
            gents_protocol::schemas::NETWORK_ENROLLMENT_REQUEST,
            gents_protocol::schemas::NETWORK_ENROLLMENT_DECISION,
            gents_protocol::schemas::NETWORK_AUTHORIZATION_REVISION,
        ] {
            node.add_collection(
                &gents_protocol::schemas::HomePolicyIds::node_store().render(collection),
            )
            .await?;
        }
        let runtime_did = identity::Did::new(runtime.did())?;
        let runtime_access = ConfigAccess::local_as(node.clone(), runtime_did.clone());
        let user_did = identity::Did::new(user.did())?;
        for permission in [
            NodePermission::DocumentRead,
            NodePermission::DocumentUpdate,
            NodePermission::DacRelationAdd,
            NodePermission::CollectionGet,
        ] {
            runtime_access
                .grant_node_permission(&user_did, permission)
                .await?;
        }
        let user_access = ConfigAccess::Graphql(
            GraphqlEndpoint::as_principal(format!("http://{address}/api/v0/graphql"), user.did())
                .with_client_signing_capability(user.client_signing_capability()?)?,
        );
        let parent = user_access
            .write(
                "test.admin_pin.node",
                &format!(
                    r#"mutation {{ create_Node(input: {{node_did:"{}"}}) {{_docID}} }}"#,
                    escape_graphql_string(runtime.did())
                ),
            )
            .await?;
        let parent = gents_protocol::graphql::extract_mutation_doc_id(&parent, "Node")?;
        user_access
            .add_actor_relationship("Node", &parent, "runtime_reader", &runtime_did)
            .await?;
        Ok(Self {
            node,
            runtime: runtime_did,
            runtime_access,
            user_access,
            parent,
            _directory: directory,
        })
    }

    async fn write_pin(
        &self,
        runtime_owned: bool,
        pin_key: &str,
        network_id: &str,
        admin_did: Option<&str>,
    ) -> anyhow::Result<()> {
        let f = escape_graphql_string;
        let admin_did = admin_did
            .map(|admin_did| format!(r#"admin_did:"{}", "#, f(admin_did)))
            .unwrap_or_default();
        let mutation = format!(
            r#"mutation {{ create_NetworkAdminPin(input: {{pin_key:"{}", network_id:"{}", {admin_did}offer_id:"{}", confirmed_at:"2026-01-01T00:00:00Z", parent_linked:false}}) {{_docID}} }}"#,
            f(pin_key),
            f(network_id),
            f(pin_key),
        );
        if runtime_owned {
            self.runtime_access
                .write("test.admin_pin.runtime", &mutation)
                .await?;
            return Ok(());
        }
        let pin = self
            .user_access
            .write("test.admin_pin.foreign", &mutation)
            .await?;
        let pin = gents_protocol::graphql::extract_mutation_doc_id(&pin, "NetworkAdminPin")?;
        self.user_access
            .add_resource_relationship("NetworkAdminPin", &pin, "parent", "nodes", &self.parent)
            .await?;
        let read = self
            .runtime_access
            .read(
                &Query::select(Selection::<network_admin_pin::Doc>::doc_id())
                    .doc_id(pin.as_str())
                    .render(),
            )
            .await?;
        anyhow::ensure!(
            gents_protocol::graphql::graphql_rows_from_response(&read, "NetworkAdminPin").len()
                == 1,
            "the runtime cannot read the foreign pin {pin}"
        );
        Ok(())
    }
}

#[tokio::test]
async fn admin_pinned_follows_the_offer_request_model() -> anyhow::Result<()> {
    use gents_lean_vocab::families::enrollment_auto_renewal_offer_request as family;
    let fixture = PinFixture::new().await?;
    for case in family::cases() {
        let scoped = |network: &str| format!("{}/{network}", case.input.name);
        for (index, pin) in case.input.pins.iter().enumerate() {
            fixture
                .write_pin(
                    pin.owner == case.input.runtime,
                    &format!("{}/{index}", case.input.name),
                    &scoped(&pin.network_id),
                    Some(&pin.admin_did),
                )
                .await?;
        }
        let requested = super::admin_pinned(
            &fixture.node,
            &scoped(&case.input.network_id),
            &case.input.admin_did,
        )
        .await?;
        case.check(family::OfferRequest { requested });
    }
    fixture
        .write_pin(true, "malformed/0", "malformed/network", Some("admin"))
        .await?;
    fixture
        .write_pin(false, "malformed/1", "malformed/network", None)
        .await?;
    assert!(super::admin_pinned(&fixture.node, "malformed/network", "admin").await?);
    Ok(())
}

#[tokio::test]
async fn enrollment_listing_lists_only_the_runtime_admin_pins() -> anyhow::Result<()> {
    let fixture = PinFixture::new().await?;
    fixture
        .write_pin(true, "listing/0", "listing", Some("admin"))
        .await?;
    fixture
        .write_pin(false, "listing/1", "listing", Some("other"))
        .await?;
    fixture
        .write_pin(false, "listing/2", "listing", None)
        .await?;
    let listing =
        crate::views::read_enrollment_listing(&fixture.runtime_access, &fixture.runtime).await?;
    let pins: Vec<_> = listing
        .pins
        .iter()
        .map(|pin| (pin.network_id.as_str(), pin.admin_did.as_str()))
        .collect();
    assert_eq!(pins, [("listing", "admin")]);
    Ok(())
}
