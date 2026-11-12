use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::templates::{to_replication_filters, PairingFilters};

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PairingDesired {
    pub replicator_addresses: BTreeSet<String>,
    #[serde(default)]
    pub replicator_collections: BTreeSet<String>,
    #[serde(default)]
    pub replicator_filter: PairingFilters,
    #[serde(default)]
    pub template_ids: BTreeSet<String>,
}

impl PairingDesired {
    pub fn has_wiring(&self) -> bool {
        !self.replicator_addresses.is_empty()
    }

    pub fn uses_agent_target_template(&self) -> bool {
        self.template_ids
            .iter()
            .any(|template| template.starts_with("agent-target-"))
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PairingActual {
    pub replicator_addresses: BTreeSet<String>,
    #[serde(default)]
    pub replicator_collections: BTreeMap<String, BTreeSet<String>>,
    #[serde(default)]
    pub replicator_filters: BTreeMap<String, defra_p2p_adapter::ReplicationFilters>,
    /// Unknown filters do not cause repeated reinstalls, but cannot certify a
    /// scoped route as matching.
    #[serde(default)]
    pub replicator_filters_unobserved: BTreeSet<String>,
    /// Keep the durable route key alongside transport-reported addresses to
    /// adopt restored peers without accepting stale ticket projections.
    #[serde(default)]
    pub replicator_observed_addresses: BTreeMap<String, BTreeSet<String>>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct PairingApplied {
    pub replicator_addresses: BTreeSet<String>,
    #[serde(default)]
    pub replicator_filter: PairingFilters,
}

impl PairingApplied {
    pub fn is_empty(&self) -> bool {
        self.replicator_addresses.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiffOp {
    InstallReplicator(String),
    TeardownReplicator(String),
}

pub fn compute_pairing_diff(desired: &PairingDesired, actual: &PairingActual) -> Vec<DiffOp> {
    let mut ops = Vec::new();
    for r in desired
        .replicator_addresses
        .difference(&actual.replicator_addresses)
    {
        ops.push(DiffOp::InstallReplicator(r.clone()));
    }
    for r in actual
        .replicator_addresses
        .difference(&desired.replicator_addresses)
    {
        ops.push(DiffOp::TeardownReplicator(r.clone()));
    }
    ops
}

pub fn compute_owned_pairing_diff(
    desired: &PairingDesired,
    actual: &PairingActual,
    applied: &PairingApplied,
) -> Vec<DiffOp> {
    let mut ops = Vec::new();
    // A data-plane install can precede a merged control-plane layer, so compare
    // live carried collections to catch this ordering race.
    let filter_changed = desired.replicator_filter != applied.replicator_filter;
    let desired_live_filters = to_replication_filters(&desired.replicator_filter).ok();
    let desired_replicator_collections = &desired.replicator_collections;
    for r in applied
        .replicator_addresses
        .difference(&desired.replicator_addresses)
    {
        ops.push(DiffOp::TeardownReplicator(r.clone()));
    }
    for r in actual
        .replicator_addresses
        .intersection(&applied.replicator_addresses)
        .filter(|r| desired.replicator_addresses.contains(*r))
    {
        let collections_changed = actual
            .replicator_collections
            .get(r)
            .is_some_and(|carried| carried != desired_replicator_collections);
        let live_filter_changed = if actual.replicator_filters_unobserved.contains(r) {
            !desired.replicator_filter.is_empty()
        } else {
            actual
                .replicator_filters
                .get(r)
                .map(|filter| {
                    desired_live_filters
                        .as_ref()
                        .is_none_or(|desired| !replication_filters_equivalent(desired, filter))
                })
                .unwrap_or(!desired.replicator_filter.is_empty())
        };
        let endpoint_changed =
            actual
                .replicator_observed_addresses
                .get(r)
                .is_some_and(|observed| {
                    observed.len() != 1
                        || observed
                            .iter()
                            .any(|address| !transport_addresses_equivalent(address, r))
                });
        if filter_changed || live_filter_changed || collections_changed || endpoint_changed {
            ops.push(DiffOp::TeardownReplicator(r.clone()));
            ops.push(DiffOp::InstallReplicator(r.clone()));
        }
    }
    for r in desired
        .replicator_addresses
        .difference(&actual.replicator_addresses)
    {
        ops.push(DiffOp::InstallReplicator(r.clone()));
    }
    ops
}

pub fn owned_pairing_live_matches(
    desired: &PairingDesired,
    actual: &PairingActual,
    applied: &PairingApplied,
) -> bool {
    compute_owned_pairing_diff(desired, actual, applied).is_empty()
        && (desired.replicator_filter.is_empty()
            || desired
                .replicator_addresses
                .iter()
                .all(|address| !actual.replicator_filters_unobserved.contains(address)))
}

fn replication_filters_equivalent(
    desired: &defra_p2p_adapter::ReplicationFilters,
    live: &defra_p2p_adapter::ReplicationFilters,
) -> bool {
    desired.len() == live.len()
        && desired.iter().all(|(collection, desired_filter)| {
            live.get(collection).is_some_and(|live_filter| {
                replication_filter_equivalent(desired_filter, live_filter)
            })
        })
}

fn replication_filter_equivalent(
    left: &defra_p2p_adapter::ReplicationFilter,
    right: &defra_p2p_adapter::ReplicationFilter,
) -> bool {
    if left == right {
        return true;
    }
    matches!(
        (scalar_equality(left), scalar_equality(right)),
        (Some(left), Some(right)) if left == right
    )
}

fn scalar_equality(
    filter: &defra_p2p_adapter::ReplicationFilter,
) -> Option<(&str, &serde_json::Value)> {
    if filter.conditions.is_none() && !filter.field.is_empty() {
        return Some((&filter.field, &filter.value));
    }
    let conditions = filter.conditions.as_ref()?;
    if conditions.len() != 1 {
        return None;
    }
    let (field, operation) = conditions.iter().next()?;
    let operation = operation.as_object()?;
    if operation.len() != 1 {
        return None;
    }
    operation.get("_eq").map(|value| (field.as_str(), value))
}

fn transport_addresses_equivalent(left: &str, right: &str) -> bool {
    if left == right {
        return true;
    }
    if let (Ok((left_peer, left_dial)), Ok((right_peer, right_dial))) = (
        p2p::iroh::parse_public_peer_addr(left),
        p2p::iroh::parse_public_peer_addr(right),
    ) {
        if left_peer == right_peer && (left_dial.is_empty() || right_dial.is_empty()) {
            return true;
        }
    }
    match (
        super::TransportEndpoint::parse(left.to_string()),
        super::TransportEndpoint::parse(right.to_string()),
    ) {
        (Ok(left), Ok(right)) => left.equivalent_to(&right),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use gents_lean_vocab::families::pairing_owned_diff_cases as family;

    use super::super::templates::{equality_filter, filter_conditions};
    use super::*;

    #[derive(Debug, Clone, Copy, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum LiveForm {
        Scalar,
        Predicate,
    }

    #[derive(Debug, Clone, Copy, Serialize, Deserialize)]
    #[serde(rename_all = "snake_case")]
    enum OpKind {
        Install,
        Teardown,
    }

    fn texts(addresses: &[family::Address]) -> BTreeSet<String> {
        addresses
            .iter()
            .map(|address| address.text.clone())
            .collect()
    }

    fn filters(scopes: &[family::Scope]) -> PairingFilters {
        scopes
            .iter()
            .map(|scope| {
                (
                    scope.collection.clone(),
                    equality_filter(&scope.field, &scope.value),
                )
            })
            .collect()
    }

    fn reported(live: &family::LiveScope<LiveForm>) -> defra_p2p_adapter::ReplicationFilter {
        let scope = &live.scope;
        match live.form {
            LiveForm::Scalar => defra_p2p_adapter::ReplicationFilter::eq(
                scope.field.as_str(),
                serde_json::Value::String(scope.value.clone()),
            ),
            LiveForm::Predicate => defra_p2p_adapter::ReplicationFilter::predicate(
                filter_conditions(&equality_filter(&scope.field, &scope.value))
                    .expect("an equality scope has conditions"),
            ),
        }
    }

    #[test]
    fn owned_pairing_diff_matches_lean() {
        family::replay::<LiveForm, OpKind>(|input| {
            let desired = PairingDesired {
                replicator_addresses: texts(&input.desired.routes),
                replicator_collections: input.desired.collections.iter().cloned().collect(),
                replicator_filter: filters(&input.desired.filter),
                template_ids: BTreeSet::new(),
            };
            let applied = PairingApplied {
                replicator_addresses: texts(&input.applied.routes),
                replicator_filter: filters(&input.applied.filter),
            };
            let mut actual = PairingActual::default();
            for route in &input.live {
                let address = route.address.text.clone();
                actual.replicator_addresses.insert(address.clone());
                if let Some(collections) = &route.collections {
                    actual
                        .replicator_collections
                        .insert(address.clone(), collections.iter().cloned().collect());
                }
                if route.filter_unobserved {
                    actual.replicator_filters_unobserved.insert(address.clone());
                }
                if let Some(scopes) = &route.filter {
                    actual.replicator_filters.insert(
                        address.clone(),
                        scopes
                            .iter()
                            .map(|live| (live.scope.collection.clone(), reported(live)))
                            .collect(),
                    );
                }
                if let Some(observed) = &route.observed {
                    actual
                        .replicator_observed_addresses
                        .insert(address, texts(observed));
                }
            }
            family::Output {
                ops: compute_owned_pairing_diff(&desired, &actual, &applied)
                    .into_iter()
                    .map(|op| match op {
                        DiffOp::InstallReplicator(address) => family::Op {
                            kind: OpKind::Install,
                            address,
                        },
                        DiffOp::TeardownReplicator(address) => family::Op {
                            kind: OpKind::Teardown,
                            address,
                        },
                    })
                    .collect(),
                live_matches: owned_pairing_live_matches(&desired, &actual, &applied),
            }
        });
    }
}
