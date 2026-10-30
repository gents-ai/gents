use std::collections::{BTreeMap, BTreeSet};

use anyhow::Result;
use gents::graphql::{graphql_with_transaction_retry, rows};
use serde_json::Value;

use super::super::super::principal_identity::PrincipalIdentity;
use super::projection::project_desktop_approval;
use super::reconcile::{
    ApprovedStatusEnrollment, DesktopApprovalProjection, EnrollmentAuthorityOutcome,
};
use super::rows::{
    EnrollmentDecisionRow, EnrollmentPinRow, EnrollmentRequestRow, EnrollmentRevisionRow,
    EnrollmentRouteReceiptRow, STATUS_ENROLLMENT_QUERY,
};

pub(super) async fn load_status_enrollment_approvals(
    node: &defra_node::EmbeddedNode,
    principal: &PrincipalIdentity,
    local_peer_id: &str,
) -> Result<(
    BTreeMap<String, EnrollmentAuthorityOutcome>,
    BTreeSet<String>,
)> {
    let response = graphql_with_transaction_retry(
        node,
        STATUS_ENROLLMENT_QUERY,
        "load status enrollment approvals",
    )
    .await?;
    let mut conflicts = BTreeMap::<String, Vec<String>>::new();
    let mut generational_conflicts = BTreeMap::<String, Vec<(Option<u64>, String)>>::new();
    let mut request_scopes = BTreeMap::<String, (String, String)>::new();
    let mut requests = Vec::new();
    let raw_requests = rows::<Value>(&response, "NetworkEnrollmentRequest")?;
    for raw in &raw_requests {
        let relevant = raw_string(raw, "candidate_did") == Some(principal.did())
            || raw_string(raw, "candidate_peer") == Some(local_peer_id);
        if !relevant {
            continue;
        }
        let (Some(request_id), Some(server_peer), Some(network_id)) = (
            raw_string(raw, "request_id"),
            raw_string(raw, "server_peer"),
            raw_string(raw, "network_id"),
        ) else {
            continue;
        };
        let scope = (server_peer.to_string(), network_id.to_string());
        if let Some(previous) = request_scopes.insert(request_id.to_string(), scope.clone()) {
            if previous != scope {
                add_scoped_conflict(
                    &mut conflicts,
                    &previous.0,
                    "request identity is bound to another enrolled server",
                );
                add_scoped_conflict(
                    &mut conflicts,
                    &scope.0,
                    "request identity is bound to another enrolled server",
                );
            }
        }
    }
    for raw in raw_requests {
        let relevant = raw_string(&raw, "candidate_did") == Some(principal.did())
            || raw_string(&raw, "candidate_peer") == Some(local_peer_id);
        if !relevant {
            continue;
        }
        let request_id = raw_string(&raw, "request_id").map(str::to_string);
        let Some(server_peer) = raw_string(&raw, "server_peer")
            .map(str::to_string)
            .or_else(|| {
                request_id
                    .as_ref()
                    .and_then(|request_id| request_scopes.get(request_id))
                    .map(|scope| scope.0.clone())
            })
        else {
            tracing::warn!("quarantining unattributable malformed local enrollment request");
            continue;
        };
        let Some(_network_id) = raw_string(&raw, "network_id") else {
            add_scoped_conflict(
                &mut conflicts,
                &server_peer,
                "local enrollment request has no network_id",
            );
            continue;
        };
        let Some(request_id) = request_id else {
            add_scoped_conflict(
                &mut conflicts,
                &server_peer,
                "local enrollment request has no request_id",
            );
            continue;
        };
        match serde_json::from_value::<EnrollmentRequestRow>(raw) {
            Ok(row) => requests.push(row),
            Err(error) => add_scoped_conflict(
                &mut conflicts,
                &server_peer,
                format!("malformed local enrollment request {request_id}: {error}"),
            ),
        }
    }

    let network_servers = request_scopes.values().fold(
        BTreeMap::<String, Vec<String>>::new(),
        |mut by_network, (server_peer, network_id)| {
            by_network
                .entry(network_id.clone())
                .or_default()
                .push(server_peer.clone());
            by_network
        },
    );
    let mut pins = BTreeMap::<String, Vec<String>>::new();
    for raw in rows::<Value>(&response, "NetworkAdminPin")? {
        let Some(network_id) = raw_string(&raw, "network_id").map(str::to_string) else {
            continue;
        };
        let Some(targets) = network_servers.get(&network_id) else {
            continue;
        };
        match serde_json::from_value::<EnrollmentPinRow>(raw) {
            Ok(row) => pins.entry(row.network_id).or_default().push(row.admin_did),
            Err(error) => {
                for server_peer in targets {
                    add_scoped_conflict(
                        &mut conflicts,
                        server_peer,
                        format!("malformed admin pin for network {network_id}: {error}"),
                    );
                }
            }
        }
    }

    let mut decisions = Vec::new();
    for raw in rows::<Value>(&response, "NetworkEnrollmentDecision")? {
        let targets = raw_authority_targets(
            &raw,
            &request_scopes,
            &network_servers,
            principal.did(),
            local_peer_id,
        );
        if targets.is_empty() {
            continue;
        }
        let sequence = raw
            .get("authorization_sequence")
            .and_then(Value::as_i64)
            .and_then(|value| u64::try_from(value).ok());
        let Some(request_id) = raw_string(&raw, "request_id").map(str::to_string) else {
            for server_peer in targets {
                add_generational_conflict(
                    &mut generational_conflicts,
                    &server_peer,
                    sequence,
                    "malformed enrollment decision has no request_id".to_string(),
                );
            }
            continue;
        };
        match serde_json::from_value::<EnrollmentDecisionRow>(raw) {
            Ok(row) => decisions.push(row),
            Err(error) => {
                for server_peer in targets {
                    add_generational_conflict(
                        &mut generational_conflicts,
                        &server_peer,
                        sequence,
                        format!("malformed decision for request {request_id}: {error}"),
                    );
                }
            }
        }
    }

    let mut revisions = Vec::new();
    for raw in rows::<Value>(&response, "NetworkAuthorizationRevision")? {
        let targets = raw_authority_targets(
            &raw,
            &request_scopes,
            &network_servers,
            principal.did(),
            local_peer_id,
        );
        if targets.is_empty() {
            continue;
        }
        let sequence = raw
            .get("sequence")
            .and_then(Value::as_i64)
            .and_then(|value| u64::try_from(value).ok());
        match serde_json::from_value::<EnrollmentRevisionRow>(raw) {
            Ok(row) if row.to_record().is_ok() => revisions.push(row),
            Ok(row) => {
                let error = row
                    .to_record()
                    .expect_err("invalid revision was preflighted");
                for server_peer in targets {
                    add_generational_conflict(
                        &mut generational_conflicts,
                        &server_peer,
                        sequence,
                        format!("malformed authorization revision: {error}"),
                    );
                }
            }
            Err(error) => {
                for server_peer in targets {
                    add_generational_conflict(
                        &mut generational_conflicts,
                        &server_peer,
                        sequence,
                        format!("malformed authorization revision: {error}"),
                    );
                }
            }
        }
    }

    let mut receipts = Vec::new();
    for raw in rows::<Value>(&response, "NetworkEnrollmentRouteReceipt")? {
        let targets = raw_authority_targets(
            &raw,
            &request_scopes,
            &network_servers,
            principal.did(),
            local_peer_id,
        );
        if targets.is_empty() {
            continue;
        }
        let sequence = raw
            .get("authorization_sequence")
            .and_then(Value::as_i64)
            .and_then(|value| u64::try_from(value).ok());
        match serde_json::from_value::<EnrollmentRouteReceiptRow>(raw) {
            Ok(row) if row.to_record().is_ok() => receipts.push(row),
            Ok(row) => {
                let error = row
                    .to_record()
                    .expect_err("invalid receipt was preflighted");
                for server_peer in targets {
                    add_generational_conflict(
                        &mut generational_conflicts,
                        &server_peer,
                        sequence,
                        format!("malformed enrollment route receipt: {error}"),
                    );
                }
            }
            Err(error) => {
                for server_peer in targets {
                    add_generational_conflict(
                        &mut generational_conflicts,
                        &server_peer,
                        sequence,
                        format!("malformed enrollment route receipt: {error}"),
                    );
                }
            }
        }
    }

    let mut approved = Vec::new();
    let mut authorized_server_peers = BTreeSet::new();
    for request_row in &requests {
        let server_peer = request_row.server_peer.clone();
        match project_desktop_approval(
            request_row,
            &requests,
            &decisions,
            &revisions,
            &receipts,
            &pins,
            principal,
            local_peer_id,
        )
        .await
        {
            Ok(DesktopApprovalProjection::Routed(approval)) => {
                authorized_server_peers.insert(server_peer);
                approved.push(*approval);
            }
            Ok(DesktopApprovalProjection::CurrentWithoutRoute) => {
                authorized_server_peers.insert(server_peer);
            }
            Ok(DesktopApprovalProjection::Absent) => {}
            Err(error) => add_scoped_conflict(
                &mut conflicts,
                &server_peer,
                format!("invalid enrollment authority: {error:#}"),
            ),
        }
    }
    apply_current_generational_conflicts(&approved, generational_conflicts, &mut conflicts);
    Ok((
        scoped_authority_outcomes(approved, conflicts),
        authorized_server_peers,
    ))
}

fn add_generational_conflict(
    conflicts: &mut BTreeMap<String, Vec<(Option<u64>, String)>>,
    server_peer: &str,
    generation: Option<u64>,
    reason: String,
) {
    if !server_peer.is_empty() {
        conflicts
            .entry(server_peer.to_string())
            .or_default()
            .push((generation, reason));
    }
}

pub(super) fn apply_current_generational_conflicts(
    approved: &[ApprovedStatusEnrollment],
    generational: BTreeMap<String, Vec<(Option<u64>, String)>>,
    conflicts: &mut BTreeMap<String, Vec<String>>,
) {
    let current = approved
        .iter()
        .map(|approval| {
            (
                approval.server_peer.as_str(),
                approval.authorization_sequence,
            )
        })
        .collect::<BTreeMap<_, _>>();
    for (server_peer, witnesses) in generational {
        for (generation, reason) in witnesses {
            let superseded = generation.is_some_and(|generation| {
                current
                    .get(server_peer.as_str())
                    .is_some_and(|current| *current > generation)
            });
            if !superseded {
                add_scoped_conflict(conflicts, &server_peer, reason);
            }
        }
    }
}

pub(super) fn scoped_authority_outcomes(
    mut approved: Vec<ApprovedStatusEnrollment>,
    mut conflicts: BTreeMap<String, Vec<String>>,
) -> BTreeMap<String, EnrollmentAuthorityOutcome> {
    approved.sort_by(|left, right| {
        (
            &left.server_peer,
            &left.request_digest,
            left.authorization_sequence,
        )
            .cmp(&(
                &right.server_peer,
                &right.request_digest,
                right.authorization_sequence,
            ))
    });
    for pair in approved.windows(2) {
        if pair[0].server_peer == pair[1].server_peer {
            add_scoped_conflict(
                &mut conflicts,
                &pair[0].server_peer,
                "one enrolled server peer has multiple current authorization generations",
            );
        }
    }
    let mut owner_peers = BTreeMap::<String, Vec<String>>::new();
    for approval in &approved {
        owner_peers
            .entry(approval.owner_node.clone())
            .or_default()
            .push(approval.server_peer.clone());
    }
    for peers in owner_peers.values() {
        if peers
            .iter()
            .collect::<std::collections::BTreeSet<_>>()
            .len()
            > 1
        {
            for peer in peers {
                add_scoped_conflict(
                    &mut conflicts,
                    peer,
                    "one enrolled owner node has multiple current transport routes",
                );
            }
        }
    }
    let mut outcomes = BTreeMap::new();
    for approval in approved {
        outcomes
            .entry(approval.server_peer.clone())
            .or_insert(EnrollmentAuthorityOutcome::Current(approval));
    }
    for (peer_id, reasons) in conflicts {
        outcomes.insert(
            peer_id,
            EnrollmentAuthorityOutcome::Conflicted {
                reason: reasons.join("; "),
            },
        );
    }
    outcomes
}

fn raw_string<'a>(value: &'a Value, field: &str) -> Option<&'a str> {
    value.get(field)?.as_str()
}

fn add_scoped_conflict(
    conflicts: &mut BTreeMap<String, Vec<String>>,
    server_peer: &str,
    reason: impl Into<String>,
) {
    if server_peer.is_empty() {
        return;
    }
    let reason = reason.into();
    let reasons = conflicts.entry(server_peer.to_string()).or_default();
    if !reasons.contains(&reason) {
        reasons.push(reason);
    }
}

pub(super) fn raw_authority_targets(
    raw: &Value,
    request_scopes: &BTreeMap<String, (String, String)>,
    network_servers: &BTreeMap<String, Vec<String>>,
    principal_did: &str,
    local_peer_id: &str,
) -> Vec<String> {
    if let Some(request_id) = raw_string(raw, "request_id") {
        if let Some((server_peer, _)) = request_scopes.get(request_id) {
            return vec![server_peer.clone()];
        }
    }
    let local_member = raw_string(raw, "member_did") == Some(principal_did)
        || raw_string(raw, "member_peer") == Some(local_peer_id)
        || raw_string(raw, "candidate_did") == Some(principal_did)
        || raw_string(raw, "candidate_peer") == Some(local_peer_id);
    if !local_member {
        return Vec::new();
    }
    raw_string(raw, "network_id")
        .and_then(|network_id| network_servers.get(network_id))
        .cloned()
        .unwrap_or_default()
}
