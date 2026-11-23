use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
use defra_node::Node as EmbeddedNode;
use gents_protocol::network_token::{derive_network_id, NetworkRecord};
use gents_store::graphql::{read_rendered, rows};
use gents_store::identity::NodeIdentity;
use serde::Deserialize;

#[derive(Deserialize)]
struct NetworkRow {
    network_id: String,
    admin_did: String,
    display_name: String,
    default_template: String,
    created_at: String,
    admin_sig: String,
}

/// The runtime's enrollment root, created on first serve, or `None` on a home
/// that serves without one and writes no `Network` row
/// (`Identity.NodePublication.enrollmentRoot`).
pub async fn ensure_enrollment_network(
    node: &Arc<EmbeddedNode>,
    identity: &dyn NodeIdentity,
    display_name: &str,
) -> Result<Option<NetworkRecord>> {
    use gents_store::local::document_acp::{node_publication, Publication};
    if node_publication(node).await? == Publication::AwaitsTransportGrants {
        return Ok(None);
    }
    load_or_create(node, identity, display_name).await.map(Some)
}

async fn load_or_create(
    node: &Arc<EmbeddedNode>,
    identity: &dyn NodeIdentity,
    display_name: &str,
) -> Result<NetworkRecord> {
    use gents_protocol::document_query::{fields::network as fields, Filter, Query, Selection};

    let network_id = derive_network_id(identity.did(), "default");
    let query = Query::select(
        Selection::new()
            .field(fields::NETWORK_ID)
            .field(fields::ADMIN_DID)
            .field(fields::DISPLAY_NAME)
            .field(fields::DEFAULT_TEMPLATE)
            .field(fields::CREATED_AT)
            .field(fields::ADMIN_SIG),
    )
    .filter(Filter::eq(fields::NETWORK_ID, &network_id))
    .filter(Filter::eq(fields::ADMIN_DID, identity.did()))
    .render();
    let response = read_rendered(node, &query, "loading local enrollment Network").await?;
    let existing = rows::<NetworkRow>(&response, "Network")?;
    match existing.as_slice() {
        [row] => {
            let record = NetworkRecord {
                network_id: row.network_id.clone(),
                admin_did: row.admin_did.clone(),
                display_name: row.display_name.clone(),
                default_template: row.default_template.clone(),
                created_at: row.created_at.clone(),
                sig: bs58::decode(&row.admin_sig)
                    .into_vec()
                    .context("decoding enrollment Network signature")?,
            };
            anyhow::ensure!(
                record.admin_did == identity.did(),
                "existing Network admin {} does not match local identity {}",
                record.admin_did,
                identity.did()
            );
            anyhow::ensure!(
                identity
                    .verify(&record.admin_did, &record.signing_payload(), &record.sig)
                    .await?,
                "existing Network signature is invalid"
            );
            Ok(record)
        }
        [] => {
            let now = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
            let mut record = NetworkRecord {
                network_id,
                admin_did: identity.did().to_string(),
                display_name: display_name.to_string(),
                default_template: "conversation".to_string(),
                created_at: now,
                sig: Vec::new(),
            };
            record.sig = identity
                .sign(&record.signing_payload())
                .await
                .context("signing enrollment Network")?;
            let mutation = {
                use gents_protocol::document_query::{fields::network as fields, Create, Input};
                let mut input = Input::new()
                    .set(fields::NETWORK_ID, &record.network_id)
                    .set(fields::ADMIN_DID, &record.admin_did)
                    .set(fields::DISPLAY_NAME, &record.display_name)
                    .set(fields::DEFAULT_TEMPLATE, &record.default_template)
                    .set(fields::CREATED_AT, &record.created_at)
                    .set(fields::ADMIN_SIG, bs58::encode(&record.sig).into_string());
                if let Some(parent) =
                    gents_store::local::document_acp::transport_publisher_node(node)?
                {
                    use gents_protocol::document_query::{kind, Field};
                    input = input.set(Field::<_, kind::Str>::new("publisher_node_id"), parent);
                }
                Create::new(input).render()
            };
            anyhow::ensure!(
                node.node_identity_did() == Some(identity.did()),
                "enrollment Network author must be the local runtime Node"
            );
            gents_store::config_client::ConfigAccess::write_local_rendered(
                node,
                "cli.enrollment.create_agent_network",
                &mutation,
            )
            .await?;
            Ok(record)
        }
        rows => anyhow::bail!("expected one enrollment Network, found {}", rows.len()),
    }
}
