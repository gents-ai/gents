use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
use defra_node::Node as EmbeddedNode;
use gents_protocol::network_token::{derive_network_id, NetworkRecord};
use gents_store::graphql::{escape_graphql_string, graphql_with_transaction_retry, rows};
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

pub async fn ensure_enrollment_network(
    node: &Arc<EmbeddedNode>,
    identity: &dyn NodeIdentity,
    display_name: &str,
) -> Result<NetworkRecord> {
    let response = graphql_with_transaction_retry(
        node,
        "{ Network { network_id admin_did display_name default_template created_at admin_sig } }",
        "loading enrollment Network",
    )
    .await?;
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
                network_id: derive_network_id(identity.did(), "default"),
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
            let network_id = escape_graphql_string(&record.network_id);
            let admin_did = escape_graphql_string(&record.admin_did);
            let display_name = escape_graphql_string(&record.display_name);
            let default_template = escape_graphql_string(&record.default_template);
            let created_at = escape_graphql_string(&record.created_at);
            let admin_sig = escape_graphql_string(&bs58::encode(&record.sig).into_string());
            let mutation = format!(
                r#"mutation {{
                    create_Network(input: {{
                        network_id: "{network_id}",
                        admin_did: "{admin_did}",
                        display_name: "{display_name}",
                        default_template: "{default_template}",
                        created_at: "{created_at}",
                        admin_sig: "{admin_sig}"
                    }}) {{ _docID }}
                }}"#
            );
            anyhow::ensure!(
                node.node_identity_did() == Some(identity.did()),
                "enrollment Network author must be the local runtime Node"
            );
            gents_store::config_client::ConfigAccess::write_local(
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
