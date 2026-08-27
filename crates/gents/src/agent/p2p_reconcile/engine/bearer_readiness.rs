use anyhow::{Context, Result};
use chrono::{SecondsFormat, Utc};
use gents_protocol::bearer_token::{derive_bearer_readiness_key, BearerPairingReadyRecord};
use serde::Deserialize;

use crate::graphql::escape_graphql_string;

use super::super::graphql_helpers::{ensure_no_errors, rows};
use super::super::templates::{conjunctive_string_eq, resolve_template};
use super::super::{PairingApplied, PairingDesired};
use super::GraphqlPairingStateStore;

impl GraphqlPairingStateStore {
    async fn bearer_readiness_is_current(
        &self,
        readiness_key: &str,
        expected: &BearerPairingReadyRecord,
    ) -> Result<(bool, usize)> {
        let readiness_key = escape_graphql_string(readiness_key);
        let query = format!(
            r#"{{
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
        let response = self.node.execute(&query).await;
        ensure_no_errors(&response, "query BearerPairingReady")?;
        let rows = rows::<BearerPairingReadyRow>(&response, "BearerPairingReady")?;
        let row_count = rows.len();
        let mut current = false;
        for row in &rows {
            let existing = match bearer_pairing_ready_record(row) {
                Ok(Some(existing)) => existing,
                Ok(None) => continue,
                Err(error) => {
                    tracing::warn!(
                        target: "gents::agent::p2p_reconcile::engine",
                        error = %error,
                        "failed to decode existing bearer readiness acknowledgement"
                    );
                    continue;
                }
            };
            if existing.issuer_did != expected.issuer_did
                || existing.claimant_did != expected.claimant_did
                || existing.peer_id != expected.peer_id
                || existing.address != expected.address
                || existing.template != expected.template
            {
                continue;
            }
            match self
                .identity
                .verify(
                    &existing.issuer_did,
                    &existing.signing_payload(),
                    &existing.sig,
                )
                .await
            {
                Ok(true) => current = true,
                Ok(false) => {}
                Err(error) => tracing::warn!(
                    target: "gents::agent::p2p_reconcile::engine",
                    error = %error,
                    claimant_did = %existing.claimant_did,
                    "failed to verify existing bearer readiness acknowledgement"
                ),
            }
        }
        Ok((current, row_count))
    }

    pub(super) async fn upsert_bearer_readiness(
        &self,
        peer_id: &str,
        claimant_did: &str,
        address: &str,
        template: &str,
    ) -> Result<()> {
        let readiness_key = derive_bearer_readiness_key(self.identity.did(), claimant_did);
        let mut record = BearerPairingReadyRecord {
            issuer_did: self.identity.did().to_string(),
            claimant_did: claimant_did.to_string(),
            peer_id: peer_id.to_string(),
            address: address.to_string(),
            template: template.to_string(),
            acknowledged_at: Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
            sig: Vec::new(),
        };
        let (current, row_count) = self
            .bearer_readiness_is_current(&readiness_key, &record)
            .await?;
        if current && row_count == 1 {
            return Ok(());
        }
        record.sig = self
            .identity
            .sign(&record.signing_payload())
            .await
            .context("signing bearer pairing readiness acknowledgement")?;
        let mutation = bearer_pairing_ready_upsert_mutation(&readiness_key, &record);
        crate::graphql::graphql_mutation_with_transaction_retry(
            &self.node,
            &mutation,
            "upsert BearerPairingReady",
        )
        .await
        .map(|_| ())
    }

    pub(super) async fn delete_bearer_readiness_for_peer(&self, peer_id: &str) -> Result<()> {
        let peer_id = escape_graphql_string(peer_id);
        let issuer_did = escape_graphql_string(self.identity.did());
        let query = format!(
            r#"{{
                BearerPairingReady(
                    filter: {{
                        peer_id: {{ _eq: "{peer_id}" }},
                        issuer_did: {{ _eq: "{issuer_did}" }}
                    }},
                    limit: 1
                ) {{ _docID }}
            }}"#
        );
        let response = self.node.execute(&query).await;
        ensure_no_errors(&response, "query BearerPairingReady for deletion")?;
        if rows::<DocIdRow>(&response, "BearerPairingReady")?.is_empty() {
            return Ok(());
        }

        let mutation = format!(
            r#"mutation {{
                delete_BearerPairingReady(
                    filter: {{
                        peer_id: {{ _eq: "{peer_id}" }},
                        issuer_did: {{ _eq: "{issuer_did}" }}
                    }}
                ) {{ _docID }}
            }}"#
        );
        crate::graphql::graphql_mutation_with_transaction_retry(
            &self.node,
            &mutation,
            "delete BearerPairingReady",
        )
        .await
        .map(|_| ())
    }
}

pub(super) fn earned_bearer_readiness(
    desired: Option<&PairingDesired>,
    applied: &PairingApplied,
    local_did: &str,
) -> Option<(String, String, String)> {
    let desired = desired?;
    let template_id = desired
        .template_ids
        .iter()
        .filter(|id| super::super::templates::conversation_like(id))
        .max()?;
    if desired.replicator_addresses.len() != 1
        || applied.replicator_addresses != desired.replicator_addresses
        || applied.replicator_filter != desired.replicator_filter
    {
        return None;
    }
    let template = resolve_template(template_id)?;
    if !template
        .collections
        .iter()
        .all(|collection| desired.replicator_collections.contains(*collection))
    {
        return None;
    }
    let readiness_filter = desired.replicator_filter.get("BearerPairingReady")?;
    let Some(claimant_did) = conjunctive_string_eq(readiness_filter, "claimant_did") else {
        tracing::warn!(
            target: "gents::agent::p2p_reconcile::engine",
            "BearerPairingReady filter has no unambiguous claimant_did equality; \
             withholding readiness"
        );
        return None;
    };
    let claimant_did = claimant_did.trim();
    if claimant_did.is_empty() {
        return None;
    }
    let expected = super::super::policy::resolve_template_filters(
        template,
        super::super::policy::PairingDirection::RuntimeToClient,
        claimant_did,
        local_did,
    );
    if expected.iter().any(|(collection, predicate)| {
        let Some(actual) = desired.replicator_filter.get(collection) else {
            return true;
        };
        if collection == "BearerPairingReady" {
            conjunctive_string_eq(actual, "claimant_did") != Some(claimant_did)
        } else {
            actual != predicate
        }
    }) {
        return None;
    }
    let address = desired
        .replicator_addresses
        .iter()
        .next()?
        .trim()
        .to_string();
    (!address.is_empty()).then(|| (claimant_did.to_string(), address, template.id.to_string()))
}

fn bearer_pairing_ready_record(
    row: &BearerPairingReadyRow,
) -> Result<Option<BearerPairingReadyRecord>> {
    let required = |value: Option<&str>| {
        value
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_owned)
    };
    let Some(issuer_did) = required(row.issuer_did.as_deref()) else {
        return Ok(None);
    };
    let Some(claimant_did) = required(row.claimant_did.as_deref()) else {
        return Ok(None);
    };
    let Some(peer_id) = required(row.peer_id.as_deref()) else {
        return Ok(None);
    };
    let Some(address) = required(row.address.as_deref()) else {
        return Ok(None);
    };
    let Some(template) = required(row.template.as_deref()) else {
        return Ok(None);
    };
    let Some(acknowledged_at) = required(row.acknowledged_at.as_deref()) else {
        return Ok(None);
    };
    let Some(issuer_sig) = required(row.issuer_sig.as_deref()) else {
        return Ok(None);
    };
    let sig = bs58::decode(issuer_sig)
        .into_vec()
        .context("decoding BearerPairingReady.issuer_sig")?;
    Ok(Some(BearerPairingReadyRecord {
        issuer_did,
        claimant_did,
        peer_id,
        address,
        template,
        acknowledged_at,
        sig,
    }))
}

pub fn bearer_pairing_ready_upsert_mutation(
    readiness_key: &str,
    record: &BearerPairingReadyRecord,
) -> String {
    let readiness_key = escape_graphql_string(readiness_key);
    let issuer_did = escape_graphql_string(&record.issuer_did);
    let claimant_did = escape_graphql_string(&record.claimant_did);
    let peer_id = escape_graphql_string(&record.peer_id);
    let address = escape_graphql_string(&record.address);
    let template = escape_graphql_string(&record.template);
    let acknowledged_at = escape_graphql_string(&record.acknowledged_at);
    let issuer_sig = escape_graphql_string(&bs58::encode(&record.sig).into_string());
    format!(
        r#"mutation {{
            delete_BearerPairingReady(
                filter: {{ readiness_key: {{ _eq: "{readiness_key}" }} }}
            ) {{ _docID }}
            upsert_BearerPairingReady(
                filter: {{ readiness_key: {{ _eq: "{readiness_key}" }} }},
                add: {{
                    readiness_key: "{readiness_key}",
                    issuer_did: "{issuer_did}",
                    claimant_did: "{claimant_did}",
                    peer_id: "{peer_id}",
                    address: "{address}",
                    template: "{template}",
                    acknowledged_at: "{acknowledged_at}",
                    issuer_sig: "{issuer_sig}"
                }},
                update: {{
                    peer_id: "{peer_id}",
                    address: "{address}",
                    template: "{template}",
                    acknowledged_at: "{acknowledged_at}",
                    issuer_sig: "{issuer_sig}"
                }}
            ) {{ _docID }}
        }}"#
    )
}

#[derive(Deserialize)]
struct BearerPairingReadyRow {
    issuer_did: Option<String>,
    claimant_did: Option<String>,
    peer_id: Option<String>,
    address: Option<String>,
    template: Option<String>,
    acknowledged_at: Option<String>,
    issuer_sig: Option<String>,
}

#[derive(Debug, Deserialize)]
struct DocIdRow {
    #[serde(rename = "_docID")]
    _doc_id: String,
}
