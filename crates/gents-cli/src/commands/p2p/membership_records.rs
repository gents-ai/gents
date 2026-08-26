use anyhow::{Context, Result};
use gents::graphql::escape_graphql_string;
use gents_protocol::network_token::{derive_membership_key, MembershipRecord};
use serde_json::Value;

use crate::config_writes::ConfigAccess;
use crate::graphql_rows;

pub(super) async fn write_membership(
    access: &ConfigAccess,
    record: &MembershipRecord,
) -> Result<()> {
    let membership_key = escaped(&derive_membership_key(
        &record.network_id,
        &record.member_did,
    ));
    let network_id = escaped(&record.network_id);
    let member_did = escaped(&record.member_did);
    let status = escaped(&record.status);
    let granted_at = escaped(&record.granted_at);
    let revoked_at = escaped(&record.revoked_at);
    let admin_sig = escaped(&bs58::encode(&record.sig).into_string());
    let mutation = format!(
        r#"mutation {{
            upsert_NetworkMembership(
                filter: {{ membership_key: {{ _eq: "{membership_key}" }} }},
                add: {{
                    membership_key: "{membership_key}",
                    network_id: "{network_id}",
                    member_did: "{member_did}",
                    status: "{status}",
                    granted_at: "{granted_at}",
                    revoked_at: "{revoked_at}",
                    admin_sig: "{admin_sig}"
                }},
                update: {{
                    network_id: "{network_id}",
                    member_did: "{member_did}",
                    status: "{status}",
                    granted_at: "{granted_at}",
                    revoked_at: "{revoked_at}",
                    admin_sig: "{admin_sig}"
                }}
            ) {{ _docID }}
        }}"#
    );
    access
        .execute(&mutation)
        .await
        .context("writing NetworkMembership row")?;
    Ok(())
}

pub(super) async fn load_membership_record(
    access: &ConfigAccess,
    network_id: &str,
    member_did: &str,
) -> Result<Option<MembershipRecord>> {
    let membership_key = escaped(&derive_membership_key(network_id, member_did));
    let query = format!(
        r#"query {{
            NetworkMembership(filter: {{ membership_key: {{ _eq: "{membership_key}" }} }}, limit: 1) {{
                membership_key
                network_id
                member_did
                status
                granted_at
                revoked_at
                admin_sig
            }}
        }}"#
    );
    graphql_rows(access, "NetworkMembership", &query)
        .await
        .context("loading NetworkMembership row")?
        .into_iter()
        .next()
        .map(|row| {
            Ok(MembershipRecord {
                network_id: required_string(&row, "network_id")?,
                member_did: required_string(&row, "member_did")?,
                status: required_string(&row, "status")?,
                granted_at: required_string(&row, "granted_at")?,
                revoked_at: row
                    .get("revoked_at")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                sig: bs58::decode(&required_string(&row, "admin_sig")?)
                    .into_vec()
                    .context("decoding base58 NetworkMembership signature")?,
            })
        })
        .transpose()
}

fn required_string(row: &Value, field: &str) -> Result<String> {
    row.get(field)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .with_context(|| format!("NetworkMembership row missing {field}"))
}

fn escaped(value: &str) -> String {
    escape_graphql_string(value)
}
