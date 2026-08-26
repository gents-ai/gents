use anyhow::{bail, Context, Result};
use gents::{graphql::escape_graphql_string, AgentIdentity};
use gents_protocol::network_token::NetworkRecord;
use serde_json::Value;

use crate::config_writes::ConfigAccess;
use crate::graphql_rows;

pub(super) async fn load_single_network_record(access: &ConfigAccess) -> Result<NetworkRecord> {
    match load_optional_network_record(access).await? {
        Some(record) => Ok(record),
        None => bail!("no AgentNetwork exists on this node; run `p2p network create` first"),
    }
}

pub(super) async fn load_optional_network_record(
    access: &ConfigAccess,
) -> Result<Option<NetworkRecord>> {
    collapse_network_rows(agent_network_rows(access).await?)
}

fn collapse_network_rows(rows: Vec<Value>) -> Result<Option<NetworkRecord>> {
    let mut records = rows
        .iter()
        .map(network_record_from_row)
        .collect::<Result<Vec<_>>>()?;
    let Some(first) = records.pop() else {
        return Ok(None);
    };
    if records.iter().any(|record| record != &first) {
        bail!(
            "conflicting AgentNetwork documents exist on this node; refusing to choose a control-plane root"
        );
    }
    if !records.is_empty() {
        tracing::warn!(
            network_id = %first.network_id,
            duplicate_count = records.len() + 1,
            "collapsing byte-identical replicated AgentNetwork documents"
        );
    }
    Ok(Some(first))
}

fn network_record_from_row(row: &Value) -> Result<NetworkRecord> {
    Ok(NetworkRecord {
        network_id: required_string(row, "network_id")?,
        admin_did: required_string(row, "admin_did")?,
        display_name: required_string(row, "display_name")?,
        default_template: required_string(row, "default_template")?,
        created_at: required_string(row, "created_at")?,
        sig: decode_sig(&required_string(row, "admin_sig")?)?,
    })
}

async fn agent_network_rows(access: &ConfigAccess) -> Result<Vec<Value>> {
    graphql_rows(
        access,
        "AgentNetwork",
        r#"query {
            AgentNetwork {
                network_id
                admin_did
                display_name
                default_template
                created_at
                admin_sig
            }
        }"#,
    )
    .await
    .context("loading AgentNetwork rows")
}

pub(super) fn ensure_local_admin(
    identity: &dyn AgentIdentity,
    network: &NetworkRecord,
) -> Result<()> {
    if identity.did() != network.admin_did {
        bail!(
            "local DID {} is not the network admin {}; refusing admin write",
            identity.did(),
            network.admin_did
        );
    }
    Ok(())
}

pub(super) async fn write_agent_network(
    access: &ConfigAccess,
    record: &NetworkRecord,
) -> Result<()> {
    let network_id = escaped(&record.network_id);
    let admin_did = escaped(&record.admin_did);
    let display_name = escaped(&record.display_name);
    let default_template = escaped(&record.default_template);
    let created_at = escaped(&record.created_at);
    let admin_sig = escaped(&bs58::encode(&record.sig).into_string());
    let mutation = format!(
        r#"mutation {{
            upsert_AgentNetwork(
                filter: {{ network_id: {{ _eq: "{network_id}" }} }},
                add: {{
                    network_id: "{network_id}",
                    admin_did: "{admin_did}",
                    display_name: "{display_name}",
                    default_template: "{default_template}",
                    created_at: "{created_at}",
                    admin_sig: "{admin_sig}"
                }},
                update: {{
                    admin_did: "{admin_did}",
                    display_name: "{display_name}",
                    default_template: "{default_template}",
                    created_at: "{created_at}",
                    admin_sig: "{admin_sig}"
                }}
            ) {{ _docID }}
        }}"#
    );
    access
        .execute(&mutation)
        .await
        .context("writing AgentNetwork row")?;
    Ok(())
}

fn required_string(row: &Value, field: &str) -> Result<String> {
    row.get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .with_context(|| format!("AgentNetwork row missing {field}"))
}

fn decode_sig(sig: &str) -> Result<Vec<u8>> {
    bs58::decode(sig)
        .into_vec()
        .context("decoding base58 AgentNetwork signature")
}

fn escaped(value: &str) -> String {
    escape_graphql_string(value)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn row(display_name: &str) -> Value {
        json!({
            "network_id": "network-a",
            "admin_did": "did:key:admin",
            "display_name": display_name,
            "default_template": "network-control",
            "created_at": "2026-08-26T00:00:00Z",
            "admin_sig": bs58::encode([1_u8, 2, 3]).into_string(),
        })
    }

    #[test]
    fn identical_replicated_network_documents_collapse_to_one_root() {
        let record = collapse_network_rows(vec![row("Network A"), row("Network A")])
            .expect("identical rows are safe")
            .expect("network root");

        assert_eq!(record.network_id, "network-a");
        assert_eq!(record.display_name, "Network A");
    }

    #[test]
    fn conflicting_network_documents_fail_closed() {
        let error = collapse_network_rows(vec![row("Network A"), row("Network B")])
            .expect_err("conflicting roots must not be selected");

        assert!(error.to_string().contains("conflicting AgentNetwork"));
    }
}
