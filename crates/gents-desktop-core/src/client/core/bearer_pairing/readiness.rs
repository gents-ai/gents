use std::time::Duration;

use anyhow::{bail, Context, Result};
use defra_node::EmbeddedNode;
use gents::graphql::escape_graphql_string;
use gents::identity::AgentIdentity;
use gents_protocol::bearer_token::{derive_bearer_readiness_key, BearerPairingReadyRecord};
use gents_protocol::network_token::{EndpointRecord, MembershipRecord};
use serde::Deserialize;
use tokio::time::{sleep, Instant};

use super::ensure_no_errors;

const BEARER_GRANT_POLL_INTERVAL: Duration = Duration::from_millis(250);

pub(super) async fn wait_for_bearer_readiness(
    node: &EmbeddedNode,
    identity: &dyn AgentIdentity,
    issuer_did: &str,
    network_id: &str,
    template: &str,
    local_endpoint: &EndpointRecord,
    wait_timeout: Duration,
) -> Result<()> {
    let deadline = Instant::now() + wait_timeout;
    loop {
        if observe_bearer_pairing_readiness(
            node,
            identity,
            issuer_did,
            network_id,
            template,
            local_endpoint,
        )
        .await?
        {
            return Ok(());
        }

        if Instant::now() >= deadline {
            bail!(
                "timed out after {}s waiting for the issuer-signed membership grant and reciprocal-replication acknowledgement; verify that the server is running with P2P, bearer-claim, reciprocal, and pairing reconcilers enabled, then relaunch to resume the saved pairing (mint a fresh invite only if the server rejected this nonce)",
                wait_timeout.as_secs()
            );
        }
        sleep(BEARER_GRANT_POLL_INTERVAL).await;
    }
}

pub(in crate::client::core) async fn observe_bearer_pairing_readiness(
    node: &EmbeddedNode,
    identity: &dyn AgentIdentity,
    issuer_did: &str,
    network_id: &str,
    template: &str,
    local_endpoint: &EndpointRecord,
) -> Result<bool> {
    let escaped_network_id = escape_graphql_string(network_id);
    let escaped_member_did = escape_graphql_string(identity.did());
    let readiness_key =
        escape_graphql_string(&derive_bearer_readiness_key(issuer_did, identity.did()));
    let query = format!(
        r#"{{
            NetworkMembership(
                filter: {{
                    network_id: {{ _eq: "{escaped_network_id}" }},
                    member_did: {{ _eq: "{escaped_member_did}" }}
                }},
                limit: 1
            ) {{
                network_id
                member_did
                status
                granted_at
                revoked_at
                admin_sig
            }}
            BearerPairingReady(
                filter: {{ readiness_key: {{ _eq: "{readiness_key}" }} }}
            ) {{
                issuer_did
                claimant_did
                peer_id
                address
                template
                acknowledged_at
                issuer_sig
            }}
        }}"#
    );
    let response = node.execute(&query).await;
    ensure_no_errors(
        &response,
        "checking issuer-signed bearer readiness evidence",
    )?;
    let membership_rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("NetworkMembership"))
        .cloned()
        .unwrap_or_else(|| serde_json::Value::Array(Vec::new()));
    let membership_rows = serde_json::from_value::<Vec<MembershipObservationRow>>(membership_rows)
        .context("decoding the replicated bearer membership grant")?;
    let Some(membership) = membership_rows.first() else {
        return Ok(false);
    };
    verify_active_membership_row(identity, issuer_did, network_id, identity.did(), membership)
        .await?;

    let readiness_rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("BearerPairingReady"))
        .cloned()
        .unwrap_or_else(|| serde_json::Value::Array(Vec::new()));
    let readiness_rows =
        serde_json::from_value::<Vec<BearerPairingReadyObservationRow>>(readiness_rows)
            .context("decoding the replicated bearer readiness acknowledgement")?;
    if readiness_rows.is_empty() {
        return Ok(false);
    }
    for readiness in &readiness_rows {
        match verify_bearer_pairing_ready_row(
            identity,
            issuer_did,
            identity.did(),
            template,
            local_endpoint,
            readiness,
        )
        .await
        {
            Ok(()) => return Ok(true),
            Err(error) => tracing::warn!(
                target: "gents_desktop_core::client::core::bearer_pairing",
                error = %error,
                issuer_did,
                "ignoring invalid bearer pairing readiness acknowledgement"
            ),
        }
    }
    Ok(false)
}

pub(super) async fn verify_bearer_pairing_ready_row(
    identity: &dyn AgentIdentity,
    issuer_did: &str,
    claimant_did: &str,
    expected_template: &str,
    local_endpoint: &EndpointRecord,
    row: &BearerPairingReadyObservationRow,
) -> Result<()> {
    let record = BearerPairingReadyRecord {
        issuer_did: required_membership_field(
            row.issuer_did.as_deref(),
            "BearerPairingReady.issuer_did",
        )?,
        claimant_did: required_membership_field(
            row.claimant_did.as_deref(),
            "BearerPairingReady.claimant_did",
        )?,
        peer_id: required_membership_field(row.peer_id.as_deref(), "BearerPairingReady.peer_id")?,
        address: required_membership_field(row.address.as_deref(), "BearerPairingReady.address")?,
        template: required_membership_field(
            row.template.as_deref(),
            "BearerPairingReady.template",
        )?,
        acknowledged_at: required_membership_field(
            row.acknowledged_at.as_deref(),
            "BearerPairingReady.acknowledged_at",
        )?,
        sig: bs58::decode(required_membership_field(
            row.issuer_sig.as_deref(),
            "BearerPairingReady.issuer_sig",
        )?)
        .into_vec()
        .context("decoding the bearer readiness signature")?,
    };
    // Iroh tickets include a mutable route set; node_id is the stable endpoint identity.
    if record.issuer_did != issuer_did
        || record.claimant_did != claimant_did
        || record.peer_id != local_endpoint.node_id
        || record.template != expected_template
    {
        bail!(
            "bearer readiness acknowledgement does not match issuer, claimant, or endpoint identity; pairing rejected"
        );
    }
    match identity
        .verify(issuer_did, &record.signing_payload(), &record.sig)
        .await
    {
        Ok(true) => Ok(()),
        Ok(false) => bail!(
            "bearer readiness acknowledgement signature is invalid for issuer {}; pairing rejected",
            issuer_did
        ),
        Err(error) => bail!(
            "bearer readiness acknowledgement signature is invalid for issuer {}: {}",
            issuer_did,
            error
        ),
    }
}

pub(super) async fn verify_active_membership_row(
    identity: &dyn AgentIdentity,
    issuer_did: &str,
    expected_network_id: &str,
    expected_member_did: &str,
    row: &MembershipObservationRow,
) -> Result<()> {
    let record = MembershipRecord {
        network_id: required_membership_field(
            row.network_id.as_deref(),
            "NetworkMembership.network_id",
        )?,
        member_did: required_membership_field(
            row.member_did.as_deref(),
            "NetworkMembership.member_did",
        )?,
        status: required_membership_field(row.status.as_deref(), "NetworkMembership.status")?,
        granted_at: required_membership_field(
            row.granted_at.as_deref(),
            "NetworkMembership.granted_at",
        )?,
        revoked_at: row.revoked_at.clone().unwrap_or_default(),
        sig: bs58::decode(required_membership_field(
            row.admin_sig.as_deref(),
            "NetworkMembership.admin_sig",
        )?)
        .into_vec()
        .context("decoding the membership grant signature")?,
    };
    if record.network_id != expected_network_id || record.member_did != expected_member_did {
        bail!(
            "replicated membership grant targets network {} and member {}, expected network {} and member {}; pairing rejected",
            record.network_id,
            record.member_did,
            expected_network_id,
            expected_member_did
        );
    }
    if record.status.trim() != "active" {
        bail!(
            "membership grant for {} is {}; an active grant is required before chat can start",
            expected_member_did,
            record.status.trim()
        );
    }
    match identity
        .verify(issuer_did, &record.signing_payload(), &record.sig)
        .await
    {
        Ok(true) => Ok(()),
        Ok(false) => bail!(
            "membership grant signature is invalid for issuer {}; pairing rejected",
            issuer_did
        ),
        Err(error) => bail!(
            "membership grant signature is invalid for issuer {}: {}",
            issuer_did,
            error
        ),
    }
}

fn required_membership_field(value: Option<&str>, field: &str) -> Result<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
        .with_context(|| format!("{field} is missing from the replicated grant"))
}

#[derive(Debug, Deserialize)]
pub(super) struct MembershipObservationRow {
    pub(super) network_id: Option<String>,
    pub(super) member_did: Option<String>,
    pub(super) status: Option<String>,
    pub(super) granted_at: Option<String>,
    pub(super) revoked_at: Option<String>,
    pub(super) admin_sig: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(super) struct BearerPairingReadyObservationRow {
    pub(super) issuer_did: Option<String>,
    pub(super) claimant_did: Option<String>,
    pub(super) peer_id: Option<String>,
    pub(super) address: Option<String>,
    pub(super) template: Option<String>,
    pub(super) acknowledged_at: Option<String>,
    pub(super) issuer_sig: Option<String>,
}
