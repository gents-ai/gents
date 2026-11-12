use gents_protocol::document_query::{
    fields::{
        node as node_fields, tool_service_health_state as health, tool_service_registry as service,
    },
    Create, Input,
};
use gents_store::config_client::NodePermission;
use gents_store::identity::{KeyIdentity, NodeIdentity};
use gents_tools::mcp::views::{Health, ServiceInventory, ServicePage, Services};

#[derive(Clone, Copy, Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum Reader {
    Owner,
    Guest,
}

#[derive(Clone, Copy, Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum Node {
    Local,
    Other,
}

#[derive(Clone, Copy, Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum Selection {
    All,
    Known,
    Missing,
}

#[derive(Clone, Copy, Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum Cursor {
    Start,
    A,
    Z,
}

#[derive(Clone, Copy, Debug, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
enum PageSize {
    One,
    Two,
}

#[tokio::test]
async fn mcp_views_inventory_replays_on_served_node() -> anyhow::Result<()> {
    let db = gents_inference::test_support::test_db("mcp-views", Default::default()).await;
    let guest_home = tempfile::tempdir()?;
    let guest_path = guest_home.path().join("guest.key");
    let guest = tokio::task::spawn_blocking(move || KeyIdentity::load_or_create(guest_path, None))
        .await??;
    let owner = db.client_access(db.node_identity.as_ref());
    let guest_did = identity::Did::new(guest.did())?;
    for permission in [NodePermission::DocumentRead, NodePermission::CollectionGet] {
        owner.grant_node_permission(&guest_did, permission).await?;
    }
    for node in [db.node_identity.did(), guest.did()] {
        for (id, enabled) in [("z", Some(true)), ("d", Some(false)), ("a", None)] {
            let mutation = Create::new(
                Input::new()
                    .set(service::NODE_DID, node)
                    .set(service::SERVICE_ID, id)
                    .set_optional(service::ENABLED, enabled),
            )
            .render();
            let created = owner
                .write_rendered("test.mcp_views.fixture", &mutation)
                .await?;
            if id == "a" {
                let doc_id = gents_store::graphql::created_doc_id(&created, "ToolServiceRegistry")?;
                owner
                    .add_actor_relationship("ToolServiceRegistry", &doc_id, "reader", &guest_did)
                    .await?;
            }
        }
    }
    for node in [db.node_identity.did(), guest.did()] {
        let parent = Create::new(Input::new().set(node_fields::NODE_DID, node)).render();
        let parent = owner
            .write_rendered("test.mcp_views.parent", &parent)
            .await?;
        let parent_id = gents_store::graphql::created_doc_id(&parent, "Node")?;
        owner
            .add_actor_relationship("Node", &parent_id, "runtime_reader", &guest_did)
            .await?;
        for (service_id, status, updated_at, guest_readable) in [
            ("a", "stale", "2026-01-01T00:00:00Z", true),
            ("a", "healthy", "2026-01-02T00:00:00Z", false),
            ("z", "healthy", "2026-01-02T00:00:00Z", false),
        ] {
            let mutation = Create::new(
                Input::new()
                    .set(health::NODE_DID, node)
                    .set(health::SERVICE_ID, service_id)
                    .set(health::STATUS, status)
                    .set(health::UPDATED_AT, updated_at),
            )
            .render();
            let created = owner
                .write_rendered("test.mcp_views.health", &mutation)
                .await?;
            if guest_readable {
                let doc_id =
                    gents_store::graphql::created_doc_id(&created, "ToolServiceHealthState")?;
                owner
                    .add_resource_relationship(
                        "ToolServiceHealthState",
                        &doc_id,
                        "parent",
                        "nodes",
                        &parent_id,
                    )
                    .await?;
            }
        }
    }
    let guest_access = db.client_access(&guest);
    let cases = gents_lean_vocab::families::mcp_views_inventory::cases::<Node, Reader, Selection>();
    assert!(!cases.is_empty());
    for case in cases {
        let input = &case.input;
        let access = match input.reader {
            Reader::Owner => &owner,
            Reader::Guest => &guest_access,
        };
        let node = match input.node {
            Node::Local => db.node_identity.did(),
            Node::Other => guest.did(),
        };
        let service_id = match input.selection {
            Selection::All => None,
            Selection::Known => Some("z"),
            Selection::Missing => Some("missing"),
        };
        let rows = access
            .read_view(&Services {
                node_did: node,
                service_id,
                enabled_only: input.enabled_only,
            })
            .await?;
        case.check(rows.into_iter().map(|row| row.service_id).collect());
        let rows = access
            .read_view(&ServiceInventory {
                node_did: node,
                service_id,
                enabled_only: input.enabled_only,
            })
            .await?;
        case.check(rows.into_iter().map(|row| row.config.service_id).collect());
    }
    let pages = gents_lean_vocab::families::mcp_views_page::cases::<Cursor, Node, PageSize, Reader>();
    assert!(!pages.is_empty());
    for case in pages {
        let input = &case.input;
        let access = match input.reader {
            Reader::Owner => &owner,
            Reader::Guest => &guest_access,
        };
        let node = match input.node {
            Node::Local => db.node_identity.did(),
            Node::Other => guest.did(),
        };
        let cursor = match input.cursor {
            Cursor::Start => None,
            Cursor::A => Some("a"),
            Cursor::Z => Some("z"),
        };
        let limit = match input.page_size {
            PageSize::One => 1,
            PageSize::Two => 2,
        };
        let rows = access
            .read_view(&ServicePage {
                node_did: node,
                cursor,
                limit,
            })
            .await?;
        case.check(rows.into_iter().map(|row| row.service_id).collect());
    }
    let health_cases = gents_lean_vocab::families::mcp_views_health::cases::<Node, Reader>();
    assert!(!health_cases.is_empty());
    for case in health_cases {
        let access = match case.input.reader {
            Reader::Owner => &owner,
            Reader::Guest => &guest_access,
        };
        let node = match case.input.node {
            Node::Local => db.node_identity.did(),
            Node::Other => guest.did(),
        };
        let rows = access.read_view(&Health { node_did: node }).await?;
        case.check(
            rows.into_iter()
                .map(|row| format!("{}={}", row.service_id, row.status.unwrap_or_default()))
                .collect(),
        );
    }
    db.node.shutdown().await?;
    Ok(())
}
