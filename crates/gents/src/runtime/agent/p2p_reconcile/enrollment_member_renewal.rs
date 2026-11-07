use std::collections::BTreeMap;
use std::sync::Arc;

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use defra_node::EmbeddedNode;
use gents_protocol::enrollment::{AuthorizationLease, AuthorizationLeaseState};

use crate::store::identity::NodeIdentity;

use super::enrollment_member_authority::{
    load_status_enrollment_approvals, EnrollmentAuthorityOutcome,
};
use super::enrollment_projection::ApprovedStatusEnrollment;

mod persisted;

pub(super) struct MemberRenewalState {
    peer_cursor: Option<String>,
    intent_cursors: BTreeMap<(String, u64), Option<String>>,
    last_intent_creation: BTreeMap<(String, u64), (std::time::Instant, u32)>,
    last_sweep: Option<std::time::Instant>,
    last_wake: Option<std::time::Instant>,
    sweep_interval: std::time::Duration,
}

impl Default for MemberRenewalState {
    fn default() -> Self {
        Self {
            peer_cursor: None,
            intent_cursors: BTreeMap::new(),
            last_intent_creation: BTreeMap::new(),
            last_sweep: None,
            last_wake: None,
            sweep_interval: std::time::Duration::from_secs(60),
        }
    }
}

impl MemberRenewalState {
    pub(super) fn due(&mut self) -> bool {
        let now = std::time::Instant::now();
        if self
            .last_sweep
            .is_some_and(|last| now.duration_since(last) < self.sweep_interval)
        {
            return false;
        }
        self.last_sweep = Some(now);
        true
    }

    pub(super) fn wait_duration(&self) -> std::time::Duration {
        self.last_sweep
            .map(|last| self.sweep_interval.saturating_sub(last.elapsed()))
            .unwrap_or(std::time::Duration::from_secs(60))
    }

    pub(super) fn defer_after_error(&mut self) {
        self.last_sweep = Some(std::time::Instant::now());
        self.sweep_interval = std::time::Duration::from_secs(60);
    }

    pub(super) fn wake(&mut self) {
        let now = std::time::Instant::now();
        let period = std::time::Duration::from_secs(1);
        if let Some(last) = self.last_wake {
            let delay = period.saturating_sub(now.duration_since(last));
            if !delay.is_zero() {
                if self.wait_duration() > delay {
                    self.last_sweep = Some(now);
                    self.sweep_interval = delay;
                }
                return;
            }
        }
        self.last_wake = Some(now);
        self.last_sweep = None;
    }
}

fn selection_start(due: &[(String, ApprovedStatusEnrollment)], after: Option<&str>) -> usize {
    after
        .and_then(|after| due.iter().position(|(peer, _)| peer.as_str() > after))
        .unwrap_or(0)
}

fn new_intent_cooldown(remaining_seconds: i64, attempts: u32) -> std::time::Duration {
    let remaining = remaining_seconds.max(1) as u64;
    let backoff = (3600u64 * (1u64 << attempts.min(5))).min(86_400);
    std::time::Duration::from_secs((remaining / 3).max(1).min(backoff))
}

fn renewal_due(approval: &ApprovedStatusEnrollment, now: DateTime<Utc>) -> bool {
    let lease = AuthorizationLease::at(&approval.authorization_expires_at, now);
    if lease.state != AuthorizationLeaseState::RenewalDue {
        return false;
    }
    let (Ok(issued), Ok(expires)) = (
        approval.decided_at.parse::<DateTime<Utc>>(),
        approval.authorization_expires_at.parse::<DateTime<Utc>>(),
    ) else {
        return false;
    };
    let lifetime = expires - issued;
    lifetime > chrono::Duration::zero() && lease.remaining <= lifetime / 2
}

fn next_sweep_interval(
    approval: &ApprovedStatusEnrollment,
    now: DateTime<Utc>,
) -> Option<std::time::Duration> {
    let issued = approval.decided_at.parse::<DateTime<Utc>>().ok()?;
    let expires = approval
        .authorization_expires_at
        .parse::<DateTime<Utc>>()
        .ok()?;
    let lifetime = (expires - issued).num_milliseconds();
    let remaining = (expires - now).num_milliseconds();
    if lifetime <= 0 || remaining <= 0 {
        return None;
    }
    let window = (gents_protocol::enrollment::ENROLLMENT_RENEWAL_WINDOW_SECONDS as i64) * 1000;
    let threshold = (lifetime / 2).min(window);
    let delay = if remaining > threshold {
        remaining - threshold
    } else {
        remaining / 4
    };
    Some(std::time::Duration::from_millis(
        delay.clamp(100, 60_000) as u64
    ))
}

pub async fn run_client_member_renewals(node: Arc<EmbeddedNode>, identity: Arc<dyn NodeIdentity>) {
    let mut state = MemberRenewalState::default();
    let mut changes = crate::subscribe_collections(&node, ["EnrollmentRenewalOffer"]).ok();
    loop {
        if state.due() {
            if let Err(error) = sweep_member_renewals(&node, identity.as_ref(), &mut state).await {
                state.defer_after_error();
                tracing::debug!(%error, "client enrollment renewal sweep failed");
            }
        }
        tokio::select! {
            _ = tokio::time::sleep(state.wait_duration()) => {}
            batch = async { changes.as_mut().expect("subscription is present").recv().await }, if changes.is_some() => {
                if batch.is_some() {
                    state.wake();
                } else {
                    changes = None;
                }
            }
        }
    }
}

pub(super) async fn sweep_member_renewals(
    node: &Arc<EmbeddedNode>,
    identity: &dyn NodeIdentity,
    state: &mut MemberRenewalState,
) -> Result<()> {
    let p2p = crate::store::identity::node_p2p(node).context("runtime P2P is unavailable")?;
    let local_peer = p2p.local_peer_id().await.map_err(anyhow::Error::msg)?;
    let (outcomes, _) = load_status_enrollment_approvals(node, identity, &local_peer).await?;
    let now = Utc::now();
    state.sweep_interval = outcomes
        .values()
        .filter_map(|outcome| match outcome {
            EnrollmentAuthorityOutcome::Current(approval) => next_sweep_interval(approval, now),
            _ => None,
        })
        .min()
        .unwrap_or(std::time::Duration::from_secs(60));
    let due: Vec<_> = outcomes
        .into_iter()
        .filter_map(|(peer, outcome)| match outcome {
            EnrollmentAuthorityOutcome::Current(approval) if renewal_due(&approval, now) => {
                Some((peer, approval))
            }
            _ => None,
        })
        .collect();
    if due.is_empty() {
        state.peer_cursor = None;
        state.intent_cursors.clear();
        state.last_intent_creation.clear();
        return Ok(());
    }
    state.intent_cursors.retain(|(peer, sequence), _| {
        due.iter().any(|(_, approval)| {
            &approval.server_peer == peer && approval.authorization_sequence == *sequence
        })
    });
    state.last_intent_creation.retain(|(peer, sequence), _| {
        due.iter().any(|(_, approval)| {
            &approval.server_peer == peer && approval.authorization_sequence == *sequence
        })
    });
    let start = selection_start(&due, state.peer_cursor.as_deref());
    let selected = due.iter().skip(start).take(4).collect::<Vec<_>>();
    state.peer_cursor = selected.last().map(|(peer, _)| peer.clone());
    for (_, approval) in selected {
        let key = (
            approval.server_peer.clone(),
            approval.authorization_sequence,
        );
        let remaining = AuthorizationLease::at(&approval.authorization_expires_at, Utc::now())
            .remaining
            .num_seconds();
        let previous = state.last_intent_creation.get(&key).copied();
        let attempts = previous.map_or(0, |(_, attempts)| attempts);
        let allow_create = previous
            .is_none_or(|(last, _)| last.elapsed() >= new_intent_cooldown(remaining, attempts));
        let cursor = state.intent_cursors.entry(key.clone()).or_default();
        match renew_member(node, identity, approval, &local_peer, cursor, allow_create).await {
            Ok(true) => {
                state
                    .last_intent_creation
                    .insert(key, (std::time::Instant::now(), attempts.saturating_add(1)));
            }
            Ok(false) => {}
            Err(error) => {
                tracing::debug!(server_peer = %approval.server_peer, error = %error, "member renewal did not complete");
            }
        }
    }
    Ok(())
}

async fn renew_member(
    node: &Arc<EmbeddedNode>,
    identity: &dyn NodeIdentity,
    approval: &ApprovedStatusEnrollment,
    local_peer: &str,
    cursor: &mut Option<String>,
    allow_create: bool,
) -> Result<bool> {
    if let Some((doc_id, intent)) =
        persisted::find_member_intent(node, identity, approval, local_peer, cursor).await?
    {
        *cursor = None;
        if super::enrollment_exchange::consume_renewal_offer(node, identity, &intent)
            .await?
            .is_none()
        {
            persisted::resend_member_intent(node, approval, &intent, &doc_id).await?;
        }
    } else if cursor.is_none() && allow_create {
        super::enrollment_candidate::request_renewal_offer(
            node,
            identity,
            &approval.network_id,
            &approval.admin_did,
            &approval.server_peer,
            &approval.server_ticket,
            approval.authorization_sequence,
        )
        .await?;
        return Ok(true);
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn peer_cursor_wraps_and_does_not_starve_later_peers() {
        let approval = ApprovedStatusEnrollment {
            network_id: "network".into(),
            request_id: "request".into(),
            server_peer: "peer".into(),
            server_ticket: "ticket".into(),
            admin_did: "admin".into(),
            owner_node: "owner".into(),
            request_digest: "digest".into(),
            authorization_sequence: 1,
            authorization_expires_at: "expiry".into(),
            decided_at: "decision".into(),
        };
        let peers: Vec<_> = (0..9)
            .map(|n| (format!("peer-{n}"), approval.clone()))
            .collect();
        assert_eq!(selection_start(&peers, None), 0);
        assert_eq!(selection_start(&peers, Some("peer-3")), 4);
        assert_eq!(selection_start(&peers, Some("peer-7")), 8);
        assert_eq!(selection_start(&peers, Some("peer-8")), 0);
    }

    #[test]
    fn intent_retry_cooldown_stays_bounded_by_lease() {
        assert_eq!(
            new_intent_cooldown(86400, 0),
            std::time::Duration::from_secs(3600)
        );
        assert_eq!(
            new_intent_cooldown(60, 0),
            std::time::Duration::from_secs(20)
        );
        assert_eq!(new_intent_cooldown(1, 0), std::time::Duration::from_secs(1));
        assert_eq!(
            new_intent_cooldown(604_800, 2),
            std::time::Duration::from_secs(14_400)
        );
        assert_eq!(
            new_intent_cooldown(604_800, 9),
            std::time::Duration::from_secs(86_400)
        );
    }

    #[test]
    fn unanswered_retry_delay_matches_the_model() {
        let raw = gents_lean_contract::run_prepared_generator(
            &gents_lean_contract::proofs_dir().unwrap(),
            "Proofs.Conformance.RenewalRetry:olean",
            "Proofs/Conformance/RenewalRetry.lean",
        )
        .unwrap();
        let cases: Vec<serde_json::Value> = serde_json::from_str(&raw).unwrap();
        assert_eq!(cases.len(), 25);
        for case in cases {
            let remaining = case["remaining"].as_i64().unwrap();
            let attempts = case["attempts"].as_u64().unwrap() as u32;
            assert_eq!(
                new_intent_cooldown(remaining, attempts).as_secs(),
                case["delay"].as_u64().unwrap(),
                "{case}"
            );
        }
    }

    #[test]
    fn a_received_offer_wakes_the_member_without_waiting_for_the_timer() {
        let mut state = MemberRenewalState::default();
        assert!(state.due());
        assert!(!state.due());
        state.wake();
        assert!(state.due());
        state.wake();
        assert!(state.wait_duration() <= std::time::Duration::from_secs(1));
    }

    #[test]
    fn short_lease_does_not_renew_repeatedly_on_creation() {
        let mut approval = ApprovedStatusEnrollment {
            network_id: "network".into(),
            request_id: "request".into(),
            server_peer: "peer".into(),
            server_ticket: "ticket".into(),
            admin_did: "admin".into(),
            owner_node: "owner".into(),
            request_digest: "digest".into(),
            authorization_sequence: 1,
            decided_at: "2026-11-01T00:00:00Z".into(),
            authorization_expires_at: "2026-11-04T00:00:00Z".into(),
        };
        assert!(!renewal_due(
            &approval,
            "2026-11-01T00:00:00Z".parse().unwrap()
        ));
        assert!(!renewal_due(
            &approval,
            "2026-11-02T11:59:59Z".parse().unwrap()
        ));
        assert!(renewal_due(
            &approval,
            "2026-11-02T12:00:00Z".parse().unwrap()
        ));
        assert!(!renewal_due(
            &approval,
            "2026-11-04T00:00:00Z".parse().unwrap()
        ));
        assert_eq!(
            next_sweep_interval(&approval, "2026-11-02T11:59:59Z".parse().unwrap()),
            Some(std::time::Duration::from_secs(1))
        );
        approval.authorization_expires_at = "2026-12-01T00:00:00Z".into();
        assert!(!renewal_due(
            &approval,
            "2026-11-23T23:59:59Z".parse().unwrap()
        ));
        assert!(renewal_due(
            &approval,
            "2026-11-24T00:00:00Z".parse().unwrap()
        ));
        approval.decided_at = "invalid".into();
        assert!(!renewal_due(
            &approval,
            "2026-11-24T00:00:00Z".parse().unwrap()
        ));
    }
}
