use std::time::Duration;

use anyhow::{Context, Result};
use chrono::Utc;
use defra_p2p_adapter::TransportPeerId;
use gents_protocol::enrollment::EnrollmentRenewalPolicy;
use gents_store::graphql::rows;
use serde::Deserialize;
use tokio::time::timeout;

use super::GraphqlEnrollmentStore;

#[derive(Deserialize)]
struct RenewalPolicyRow {
    #[serde(rename = "_docID")]
    doc_id: String,
    enabled: Option<bool>,
    lease_seconds: Option<i64>,
}

impl GraphqlEnrollmentStore {
    pub async fn issue_renewal_offer(
        &self,
        intent: &gents_protocol::enrollment::RenewalIntent,
    ) -> Result<gents_protocol::enrollment::RenewalOfferResponse> {
        let _guard = self.decision_lock.lock().await;
        let policy = self.renewal_policy(&intent.network_id).await?;
        anyhow::ensure!(policy.enabled, "automatic enrollment renewal is disabled");
        let projection = self.load_projection().await?;
        let active = projection
            .current_for_renewal_intent(intent, Utc::now())?
            .clone();
        anyhow::ensure!(
            self.identity
                .verify(
                    &intent.member_did,
                    &intent.signing_payload(),
                    &intent.member_sig
                )
                .await?,
            "renewal intent signature is invalid"
        );
        let p2p =
            crate::node_access::operations(&self.node).context("runtime P2P is unavailable")?;
        let local_peer = timeout(Duration::from_secs(20), p2p.local_peer_id())
            .await?
            .map_err(anyhow::Error::msg)?;
        anyhow::ensure!(
            local_peer == intent.server_peer,
            "renewal intent targets another server"
        );
        timeout(
            Duration::from_secs(20),
            p2p.connect_peer(&active.request.candidate_ticket),
        )
        .await?
        .map_err(anyhow::Error::msg)?;
        let peer = TransportPeerId::new(intent.member_peer.clone()).map_err(anyhow::Error::msg)?;
        let resolved = timeout(Duration::from_secs(20), p2p.resolve_peer_identity(&peer))
            .await?
            .map_err(anyhow::Error::msg)?
            .context("member has no authenticated transport identity")?;
        anyhow::ensure!(
            resolved.to_string() == intent.member_did,
            "renewal transport identity mismatch"
        );
        let issuer = super::super::enrollment_offer::EnrollmentOfferIssuer::new(
            self.node.clone(),
            self.identity.clone(),
            p2p,
            intent.network_id.clone(),
            active.request.owner_node_did.clone(),
            active.request.profile.clone(),
        );
        let offer = issuer
            .mint_renewal(
                &intent.member_did,
                &intent.member_peer,
                intent.predecessor_sequence,
            )
            .await?;
        let latest = self.load_projection().await?;
        anyhow::ensure!(
            latest.current_for_renewal_intent(intent, Utc::now())? == &active,
            "enrollment generation changed while issuing renewal offer"
        );
        let mut response = gents_protocol::enrollment::RenewalOfferResponse {
            intent_id: intent.intent_id(),
            member_did: intent.member_did.clone(),
            offer_token: offer.token,
            admin_did: self.identity.did().to_owned(),
            admin_sig: Vec::new(),
        };
        response.admin_sig = self.identity.sign(&response.signing_payload()).await?;
        response.validate_for(intent, Utc::now())?;
        Ok(response)
    }

    async fn read_renewal_policy_row(&self, network_id: &str) -> Result<Option<RenewalPolicyRow>> {
        let network = gents_store::graphql::escape_graphql_string(network_id);
        let query = format!(
            r#"{{ EnrollmentRenewalPolicy(filter: {{ network_id: {{ _eq: "{network}" }} }}) {{ _docID enabled lease_seconds }} }}"#
        );
        let response = gents_store::graphql::graphql_with_transaction_retry(
            &self.node,
            &query,
            "load local enrollment renewal policy",
        )
        .await?;
        let mut policies = rows::<RenewalPolicyRow>(&response, "EnrollmentRenewalPolicy")?;
        anyhow::ensure!(
            policies.len() <= 1,
            "conflicting enrollment renewal policies"
        );
        Ok(policies.pop())
    }

    /// Callers must first authenticate the operator. Policy writes share the
    /// enrollment decision lock so a disable cannot race an automatic grant.
    pub async fn set_renewal_policy(&self, policy: EnrollmentRenewalPolicy) -> Result<()> {
        if let Some(lease) = policy.lease_seconds {
            anyhow::ensure!(
                (1..=gents_protocol::enrollment::MAX_ENROLLMENT_AUTHORIZATION_LEASE_SECONDS)
                    .contains(&lease),
                "renewal lease is outside the protocol limit"
            );
        }
        let _guard = self.decision_lock.lock().await;
        let projection = self.load_projection().await?;
        anyhow::ensure!(
            projection.conflict.is_none(),
            "enrollment authority is conflicted"
        );
        let network_id = projection.network_id.context("no enrollment network")?;
        let current = self.read_renewal_policy_row(&network_id).await?;
        let lease = policy
            .lease_seconds
            .map_or_else(|| "null".to_owned(), |value| value.to_string());
        let fields = format!("enabled: {}, lease_seconds: {lease}", policy.enabled);
        let runtime = identity::Did::new(self.identity.did())?;
        let access =
            gents_store::config_client::ConfigAccess::local_as(self.node.clone(), runtime.clone());
        match current {
            Some(row) => {
                let mutation = format!(
                    r#"mutation {{ update_EnrollmentRenewalPolicy(docID: "{}", input: {{ {fields} }}) {{ _docID }} }}"#,
                    gents_store::graphql::escape_graphql_string(&row.doc_id)
                );
                access
                    .write("enrollment.set_renewal_policy", &mutation)
                    .await?;
            }
            None => {
                let mutation = format!(
                    r#"mutation {{ create_EnrollmentRenewalPolicy(input: {{ network_id: "{}", {fields}, parent_linked: false }}) {{ _docID }} }}"#,
                    gents_store::graphql::escape_graphql_string(&network_id)
                );
                access
                    .transact_write("enrollment.set_renewal_policy", |txn| {
                        let mutation = &mutation;
                        let runtime = &runtime;
                        Box::pin(async move {
                            txn.validate_runtime_actor(runtime)?;
                            let response = txn.execute(mutation).await?;
                            let doc = gents_protocol::graphql::extract_mutation_doc_id(
                                &response,
                                "EnrollmentRenewalPolicy",
                            )?;
                            gents_store::config_client::maybe_link_deferred_operation_in_txn(
                                txn,
                                "EnrollmentRenewalPolicy",
                                &doc,
                                runtime,
                            )
                            .await?;
                            Ok::<_, anyhow::Error>(())
                        })
                    })
                    .await?;
            }
        }
        Ok(())
    }

    pub(in crate::p2p_reconcile) async fn renewal_policy(
        &self,
        network_id: &str,
    ) -> Result<EnrollmentRenewalPolicy> {
        let Some(row) = self.read_renewal_policy_row(network_id).await? else {
            return Ok(EnrollmentRenewalPolicy::default());
        };
        let lease_seconds = row
            .lease_seconds
            .map(u64::try_from)
            .transpose()
            .context("invalid enrollment renewal lease")?;
        if let Some(lease) = lease_seconds {
            anyhow::ensure!(
                (1..=gents_protocol::enrollment::MAX_ENROLLMENT_AUTHORIZATION_LEASE_SECONDS)
                    .contains(&lease),
                "renewal lease is outside the protocol limit"
            );
        }
        Ok(EnrollmentRenewalPolicy {
            enabled: row.enabled.unwrap_or(false),
            lease_seconds,
        })
    }
}
