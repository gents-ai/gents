use std::io::{self, Write};
use std::path::Path;

use anyhow::{bail, Context, Result};
use chrono::{SecondsFormat, Utc};
use gents::{graphql::escape_graphql_string, AgentIdentity};
use gents_protocol::network_token::{
    derive_network_id, encode_pointer, EndpointRecord, MembershipRecord, NetworkPointer,
    NetworkRecord, NETWORK_POINTER_VERSION,
};
use serde_json::{json, Value};

use crate::cli::args::{P2pNetworkCreateArgs, P2pNetworkGrantArgs, P2pNetworkRevokeArgs};
use crate::cli::output_format::OutputFormat;
use crate::config_writes::ConfigAccess;
use crate::{print_json, resolve_config_access, resolve_graphql_endpoint};

use super::invite::resolve_home_identity;
use super::membership_records::{load_membership_record, write_membership};
use super::network_records::{
    ensure_local_admin, load_optional_network_record, load_single_network_record,
    write_agent_network,
};
use super::output::load_live_http_p2p_status;

pub(super) async fn p2p_network_create(args: P2pNetworkCreateArgs) -> Result<()> {
    let graphql = resolve_graphql_endpoint(args.graphql.as_deref(), args.home.as_deref())?;
    let (access, home_dir) =
        resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
    let identity = resolve_home_identity(args.home.as_deref())
        .context("resolving local agent identity for network creation")?;
    let admin_did = identity.did().to_string();

    if let Some(existing) = load_optional_network_record(&access).await? {
        bail!(
            "a network already exists on this node (network_id={}); create is singleton",
            existing.network_id
        );
    }

    let now = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let network_id = derive_network_id(&admin_did, &args.name);
    let default_template = "network-control".to_string();
    let mut network = NetworkRecord {
        network_id: network_id.clone(),
        admin_did: admin_did.clone(),
        display_name: args.name.clone(),
        default_template,
        created_at: now.clone(),
        sig: Vec::new(),
    };
    network.sig = identity
        .sign(&network.signing_payload())
        .await
        .context("signing AgentNetwork record")?;
    write_agent_network(&access, &network).await?;

    let mut membership = MembershipRecord {
        network_id: network_id.clone(),
        member_did: admin_did.clone(),
        status: "active".to_string(),
        granted_at: now.clone(),
        revoked_at: String::new(),
        sig: Vec::new(),
    };
    membership.sig = identity
        .sign(&membership.signing_payload())
        .await
        .context("signing admin self NetworkMembership record")?;
    write_membership(&access, &membership).await?;

    let endpoint =
        publish_self_endpoint(&access, args.home.as_deref(), &graphql, identity.as_ref()).await?;
    let pointer = signed_network_pointer(&network_id, &admin_did, &endpoint.address, &identity)
        .await
        .context("signing network pointer")?;

    print_network_create(
        args.output,
        json!({
            "status": "network_created",
            "home": home_dir,
            "graphql": graphql,
            "network_id": network_id,
            "admin_did": admin_did,
            "display_name": args.name,
            "default_template": network.default_template,
            "endpoint": {
                "did": endpoint.did,
                "node_id": endpoint.node_id,
                "address": endpoint.address,
                "updated_at": endpoint.updated_at,
            },
            "pointer": encode_pointer(&pointer)?,
        }),
    )
}

pub(super) async fn p2p_network_grant(args: P2pNetworkGrantArgs) -> Result<()> {
    let (access, home_dir) =
        resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
    let identity = resolve_home_identity(args.home.as_deref())
        .context("resolving local agent identity for network grant")?;
    let network = load_single_network_record(&access).await?;
    ensure_local_admin(identity.as_ref(), &network)?;

    let now = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let mut membership = MembershipRecord {
        network_id: network.network_id.clone(),
        member_did: args.member_did.clone(),
        status: "active".to_string(),
        granted_at: now.clone(),
        revoked_at: String::new(),
        sig: Vec::new(),
    };
    membership.sig = identity
        .sign(&membership.signing_payload())
        .await
        .with_context(|| format!("signing NetworkMembership grant for {}", args.member_did))?;
    write_membership(&access, &membership).await?;

    print_membership(
        args.output,
        json!({
            "status": "membership_granted",
            "home": home_dir,
            "network_id": network.network_id,
            "admin_did": network.admin_did,
            "member_did": args.member_did,
            "membership_status": "active",
        }),
    )
}

pub(super) async fn p2p_network_revoke(args: P2pNetworkRevokeArgs) -> Result<()> {
    let (access, home_dir) =
        resolve_config_access(args.home.as_deref(), args.graphql.as_deref()).await?;
    let identity = resolve_home_identity(args.home.as_deref())
        .context("resolving local agent identity for network revoke")?;
    let network = load_single_network_record(&access).await?;
    ensure_local_admin(identity.as_ref(), &network)?;

    let now = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
    let existing = load_membership_record(&access, &network.network_id, &args.member_did).await?;
    let granted_at = existing
        .map(|row| row.granted_at)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| now.clone());
    let mut membership = MembershipRecord {
        network_id: network.network_id.clone(),
        member_did: args.member_did.clone(),
        status: "revoked".to_string(),
        granted_at,
        revoked_at: now.clone(),
        sig: Vec::new(),
    };
    membership.sig = identity
        .sign(&membership.signing_payload())
        .await
        .with_context(|| format!("signing NetworkMembership revoke for {}", args.member_did))?;
    write_membership(&access, &membership).await?;

    print_membership(
        args.output,
        json!({
            "status": "membership_revoked",
            "home": home_dir,
            "network_id": network.network_id,
            "admin_did": network.admin_did,
            "member_did": args.member_did,
            "membership_status": "revoked",
            "revoked_at": now,
        }),
    )
}

async fn publish_self_endpoint(
    access: &ConfigAccess,
    home: Option<&Path>,
    graphql: &str,
    identity: &dyn AgentIdentity,
) -> Result<EndpointRecord> {
    let p2p_status = load_live_http_p2p_status(home, graphql).await;
    let peer_id = p2p_status
        .get("p2p_peer_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .context("runtime did not report a P2P peer id; is P2P enabled?")?;
    let address = endpoint_address(&p2p_status, peer_id)
        .context("runtime did not report a shareable P2P address")?;
    let mut endpoint = EndpointRecord {
        did: identity.did().to_string(),
        node_id: peer_id.to_string(),
        address,
        updated_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        sig: Vec::new(),
    };
    endpoint.sig = identity
        .sign(&endpoint.signing_payload())
        .await
        .context("signing PeerEndpoint record")?;
    write_endpoint(access, &endpoint).await?;
    Ok(endpoint)
}

fn endpoint_address(p2p_status: &Value, peer_id: &str) -> Option<String> {
    let raw = p2p_status
        .get("p2p_shareable_address")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .or_else(|| {
            p2p_status
                .get("p2p_listen_addresses")
                .and_then(Value::as_array)
                .and_then(|rows| rows.iter().find_map(Value::as_str))
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map(ToOwned::to_owned)
        })?;
    if raw.starts_with('/') && !raw.contains("/p2p/") {
        Some(format!("{raw}/p2p/{peer_id}"))
    } else {
        Some(raw)
    }
}

async fn write_endpoint(access: &ConfigAccess, record: &EndpointRecord) -> Result<()> {
    let did = escaped(&record.did);
    let node_id = escaped(&record.node_id);
    let address = escaped(&record.address);
    let updated_at = escaped(&record.updated_at);
    let binding_sig = escaped(&encode_sig(&record.sig));
    let mutation = format!(
        r#"mutation {{
            upsert_PeerEndpoint(
                filter: {{ did: {{ _eq: "{did}" }} }},
                add: {{
                    did: "{did}",
                    node_id: "{node_id}",
                    address: "{address}",
                    updated_at: "{updated_at}",
                    binding_sig: "{binding_sig}"
                }},
                update: {{
                    node_id: "{node_id}",
                    address: "{address}",
                    updated_at: "{updated_at}",
                    binding_sig: "{binding_sig}"
                }}
            ) {{ _docID }}
        }}"#
    );
    access
        .execute(&mutation)
        .await
        .context("writing PeerEndpoint row")?;
    Ok(())
}

async fn signed_network_pointer(
    network_id: &str,
    admin_did: &str,
    admin_ticket: &str,
    identity: &std::sync::Arc<dyn AgentIdentity>,
) -> Result<NetworkPointer> {
    let mut pointer = NetworkPointer {
        v: NETWORK_POINTER_VERSION,
        network_id: network_id.to_string(),
        admin_did: admin_did.to_string(),
        admin_ticket: admin_ticket.to_string(),
        issued_at: Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true),
        nonce: uuid::Uuid::new_v4().to_string(),
        sig: Vec::new(),
    };
    pointer.sig = identity.sign(&pointer.signing_payload()).await?;
    Ok(pointer)
}

fn encode_sig(sig: &[u8]) -> String {
    bs58::encode(sig).into_string()
}

fn escaped(value: &str) -> String {
    escape_graphql_string(value)
}

fn print_network_create(output: OutputFormat, value: Value) -> Result<()> {
    match output.ensure_supported(
        "p2p network create",
        &[OutputFormat::Text, OutputFormat::Json],
    )? {
        OutputFormat::Json => print_json(&value),
        OutputFormat::Text => {
            let mut stdout = io::stdout();
            writeln!(
                stdout,
                "created network {} (admin {})",
                value["network_id"].as_str().unwrap_or_default(),
                value["admin_did"].as_str().unwrap_or_default()
            )?;
            writeln!(stdout, "{}", value["pointer"].as_str().unwrap_or_default())?;
            stdout.flush().context("flushing network create output")
        }
        _ => unreachable!("ensure_supported restricts p2p network create output formats"),
    }
}

fn print_membership(output: OutputFormat, value: Value) -> Result<()> {
    let command = match value.get("status").and_then(Value::as_str) {
        Some("membership_granted") => "p2p network grant",
        Some("membership_revoked") => "p2p network revoke",
        _ => "p2p network membership",
    };
    match output.ensure_supported(command, &[OutputFormat::Text, OutputFormat::Json])? {
        OutputFormat::Json => print_json(&value),
        OutputFormat::Text => {
            let mut stdout = io::stdout();
            writeln!(
                stdout,
                "{} {} in {}",
                value["member_did"].as_str().unwrap_or_default(),
                value["membership_status"].as_str().unwrap_or_default(),
                value["network_id"].as_str().unwrap_or_default()
            )?;
            stdout.flush().context("flushing network membership output")
        }
        _ => unreachable!("ensure_supported restricts p2p network membership output formats"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_address_appends_peer_id_to_plain_multiaddr() {
        let status = json!({
            "p2p_shareable_address": "/ip4/127.0.0.1/tcp/1234",
        });
        assert_eq!(
            endpoint_address(&status, "peer-a").as_deref(),
            Some("/ip4/127.0.0.1/tcp/1234/p2p/peer-a")
        );
    }

    #[test]
    fn endpoint_address_preserves_shareable_address_with_peer_id() {
        let status = json!({
            "p2p_shareable_address": "/ip4/127.0.0.1/tcp/1234/p2p/peer-a",
        });
        assert_eq!(
            endpoint_address(&status, "peer-a").as_deref(),
            Some("/ip4/127.0.0.1/tcp/1234/p2p/peer-a")
        );
    }
}
