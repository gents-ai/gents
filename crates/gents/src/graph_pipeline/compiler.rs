use std::collections::{BTreeMap, BTreeSet, VecDeque};

use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::types::{
    CapabilityManifestEntry, DeliveryMode, Diagnostic, DiagnosticCode, GraphIntent, GraphPlan,
    GroupCount, PackagePlan, PlannedEdge, PlannedEntry, PlannedNode, PlannedResult,
    PortCardinality, PortRef, PortSpec, ResultCardinality, StageCapability, COMPILER_VERSION,
};
use crate::graphql::{
    validate_collection_identifier, validate_graphql_filter_fragment, validate_graphql_name,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompilerPolicy {
    pub max_nodes: u32,
    pub max_edges: u32,
    pub max_depth: u32,
    pub max_fan_out: u32,
    pub max_total_invocations: u32,
    pub max_runtime_secs: u64,
    pub max_group_size: u32,
    pub max_group_timeout_secs: u64,
}

impl Default for CompilerPolicy {
    fn default() -> Self {
        Self {
            max_nodes: 64,
            max_edges: 256,
            max_depth: 32,
            max_fan_out: 16,
            max_total_invocations: 1_024,
            max_runtime_secs: 86_400,
            max_group_size: 256,
            max_group_timeout_secs: 86_400,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Error)]
#[error("graph intent failed whole-graph validation")]
pub struct GraphCompileError {
    pub diagnostics: Vec<Diagnostic>,
}

fn diagnostic(
    diagnostics: &mut Vec<Diagnostic>,
    code: DiagnosticCode,
    path: impl Into<String>,
    message: impl Into<String>,
) {
    diagnostics.push(Diagnostic {
        code,
        path: path.into(),
        message: message.into(),
    });
}

fn input_port<'a>(capability: &'a StageCapability, name: &str) -> Option<&'a PortSpec> {
    capability.input_ports.iter().find(|port| port.name == name)
}

fn output_port<'a>(capability: &'a StageCapability, name: &str) -> Option<&'a PortSpec> {
    capability
        .output_ports
        .iter()
        .find(|port| port.name == name)
}

fn delivery_sort_key(delivery: &DeliveryMode) -> (u8, u32, String, u64) {
    match delivery {
        DeliveryMode::PerDocument => (0, 0, String::new(), 0),
        DeliveryMode::PerGroup {
            expected: GroupCount::Static { count },
            timeout_secs,
        } => (1, *count, String::new(), timeout_secs.unwrap_or_default()),
        DeliveryMode::PerGroup {
            expected: GroupCount::SourceField { field },
            timeout_secs,
        } => (2, 0, field.clone(), timeout_secs.unwrap_or_default()),
    }
}

fn sorted_diagnostics(diagnostics: &mut [Diagnostic]) {
    diagnostics.sort_by(|left, right| {
        (&left.path, &left.code, &left.message).cmp(&(&right.path, &right.code, &right.message))
    });
}

fn check_requested_limits(
    intent: &GraphIntent,
    policy: &CompilerPolicy,
    diagnostics: &mut Vec<Diagnostic>,
) {
    let requested = &intent.limits;
    let platform = [
        ("max_nodes", requested.max_nodes, policy.max_nodes),
        ("max_edges", requested.max_edges, policy.max_edges),
        ("max_depth", requested.max_depth, policy.max_depth),
        ("max_fan_out", requested.max_fan_out, policy.max_fan_out),
    ];
    for (name, value, ceiling) in platform {
        if value > ceiling {
            diagnostic(
                diagnostics,
                DiagnosticCode::PlatformLimitExceeded,
                format!("/limits/{name}"),
                format!("requested {value}, platform ceiling is {ceiling}"),
            );
        }
    }
    if requested.max_runtime_secs == 0 || requested.max_runtime_secs > policy.max_runtime_secs {
        diagnostic(
            diagnostics,
            DiagnosticCode::PlatformLimitExceeded,
            "/limits/max_runtime_secs",
            format!(
                "requested {}, platform range is 1..={}",
                requested.max_runtime_secs, policy.max_runtime_secs
            ),
        );
    }

    let node_count = intent.nodes.len() as u32;
    if node_count > requested.max_nodes {
        diagnostic(
            diagnostics,
            DiagnosticCode::NodeLimitExceeded,
            "/nodes",
            format!(
                "graph has {node_count} nodes, limit is {}",
                requested.max_nodes
            ),
        );
    }
    let edge_count = intent.edges.len() as u32;
    if edge_count > requested.max_edges {
        diagnostic(
            diagnostics,
            DiagnosticCode::EdgeLimitExceeded,
            "/edges",
            format!(
                "graph has {edge_count} edges, limit is {}",
                requested.max_edges
            ),
        );
    }
}

fn validate_capability_ports(
    capability: &StageCapability,
    node_path: &str,
    diagnostics: &mut Vec<Diagnostic>,
) {
    for (kind, ports) in [
        ("input_ports", &capability.input_ports),
        ("output_ports", &capability.output_ports),
    ] {
        let mut seen = BTreeSet::new();
        for port in ports {
            if !seen.insert(&port.name) {
                diagnostic(
                    diagnostics,
                    DiagnosticCode::DuplicatePort,
                    format!("{node_path}/capability/{kind}"),
                    format!(
                        "capability {}@{} declares port {:?} more than once",
                        capability.capability_id, capability.revision, port.name
                    ),
                );
            }
            if validate_collection_identifier(&port.collection).is_err() {
                diagnostic(
                    diagnostics,
                    DiagnosticCode::InvalidCollection,
                    format!("{node_path}/capability/{kind}/{}", port.name),
                    format!(
                        "capability {}@{} declares invalid collection {:?}",
                        capability.capability_id, capability.revision, port.collection
                    ),
                );
            }
            if validate_graphql_name(&port.correlation_field).is_err() {
                diagnostic(
                    diagnostics,
                    DiagnosticCode::InvalidCorrelationField,
                    format!("{node_path}/capability/{kind}/{}", port.name),
                    format!(
                        "capability {}@{} declares invalid correlation field {:?}",
                        capability.capability_id, capability.revision, port.correlation_field
                    ),
                );
            }
        }
    }
}

/// Compile an untrusted intent without performing I/O.
///
/// Capabilities must come from the caller-visible, operator-approved catalog.
/// Empty `allowed_callers` lists deny access. Every diagnostic is stable-sorted
/// so a model can repair a proposal deterministically.
pub fn compile_graph(
    intent: &GraphIntent,
    capabilities: &[StageCapability],
    caller_did: &str,
    policy: &CompilerPolicy,
) -> Result<GraphPlan, GraphCompileError> {
    let mut diagnostics = Vec::new();
    if intent.graph_id.trim().is_empty() {
        diagnostic(
            &mut diagnostics,
            DiagnosticCode::EmptyGraphId,
            "/graph_id",
            "graph_id must not be empty",
        );
    }
    if intent.nodes.is_empty() {
        diagnostic(
            &mut diagnostics,
            DiagnosticCode::EmptyGraph,
            "/nodes",
            "a graph must contain at least one node",
        );
    }
    check_requested_limits(intent, policy, &mut diagnostics);

    let mut catalog = BTreeMap::new();
    for capability in capabilities {
        let key = (
            capability.capability_id.as_str(),
            capability.revision.as_str(),
        );
        if catalog.insert(key, capability).is_some() {
            diagnostic(
                &mut diagnostics,
                DiagnosticCode::DuplicateCapability,
                format!(
                    "/capabilities/{}@{}",
                    capability.capability_id, capability.revision
                ),
                "the capability catalog contains a duplicate id and revision",
            );
        }
    }
    let capability_ids: BTreeSet<&str> = capabilities
        .iter()
        .map(|capability| capability.capability_id.as_str())
        .collect();

    let mut nodes = BTreeSet::new();
    let mut resolved = BTreeMap::new();
    for (index, node) in intent.nodes.iter().enumerate() {
        let node_path = format!("/nodes/{index}");
        if !nodes.insert(node.node_id.as_str()) {
            diagnostic(
                &mut diagnostics,
                DiagnosticCode::DuplicateNode,
                format!("{node_path}/node_id"),
                format!("node {:?} is declared more than once", node.node_id),
            );
            continue;
        }
        let Some(capability) = catalog.get(&(
            node.capability_id.as_str(),
            node.capability_revision.as_str(),
        )) else {
            let (code, message) = if capability_ids.contains(node.capability_id.as_str()) {
                (
                    DiagnosticCode::CapabilityRevisionMismatch,
                    format!(
                        "capability {:?} has no approved revision {:?}",
                        node.capability_id, node.capability_revision
                    ),
                )
            } else {
                (
                    DiagnosticCode::UnknownCapability,
                    format!(
                        "capability {:?} is not in the approved catalog",
                        node.capability_id
                    ),
                )
            };
            diagnostic(
                &mut diagnostics,
                code,
                format!("{node_path}/capability_id"),
                message,
            );
            continue;
        };
        validate_capability_ports(capability, &node_path, &mut diagnostics);
        if !capability
            .allowed_callers
            .iter()
            .any(|allowed| allowed == caller_did)
        {
            diagnostic(
                &mut diagnostics,
                DiagnosticCode::UnauthorizedCapability,
                format!("{node_path}/capability_id"),
                format!(
                    "caller {caller_did:?} may not compose capability {:?}",
                    capability.capability_id
                ),
            );
        }
        resolved.insert(node.node_id.as_str(), *capability);
    }

    // EventTrigger routes by physical collection, not producer node. Reusing
    // one output collection for two graph nodes would make `from.node_id`
    // decorative, so v1 rejects that ambiguity instead of approximating it.
    let mut output_collections = BTreeMap::new();
    for (node_id, capability) in &resolved {
        for port in &capability.output_ports {
            if let Some(previous) = output_collections.insert(port.collection.as_str(), *node_id) {
                if previous != *node_id {
                    diagnostic(
                        &mut diagnostics,
                        DiagnosticCode::DuplicateOutputCollection,
                        format!("/nodes/{node_id}/outputs/{}", port.name),
                        format!(
                            "output collection {:?} is already produced by node {previous:?}",
                            port.collection
                        ),
                    );
                }
            }
        }
    }

    let mut incoming: BTreeMap<PortRef, u32> = BTreeMap::new();
    let mut adjacency: BTreeMap<&str, BTreeSet<&str>> = nodes
        .iter()
        .copied()
        .map(|node_id| (node_id, BTreeSet::new()))
        .collect();
    let mut fan_out: BTreeMap<&str, u32> = BTreeMap::new();

    for (index, edge) in intent.edges.iter().enumerate() {
        let path = format!("/edges/{index}");
        let source_node = nodes.contains(edge.from.node_id.as_str());
        let target_node = nodes.contains(edge.to.node_id.as_str());
        if !source_node {
            diagnostic(
                &mut diagnostics,
                DiagnosticCode::UnknownNode,
                format!("{path}/from/node_id"),
                format!("unknown source node {:?}", edge.from.node_id),
            );
        }
        if !target_node {
            diagnostic(
                &mut diagnostics,
                DiagnosticCode::UnknownNode,
                format!("{path}/to/node_id"),
                format!("unknown target node {:?}", edge.to.node_id),
            );
        }
        let source_port = resolved
            .get(edge.from.node_id.as_str())
            .and_then(|capability| output_port(capability, &edge.from.port));
        let target_port = resolved
            .get(edge.to.node_id.as_str())
            .and_then(|capability| input_port(capability, &edge.to.port));
        if source_node && source_port.is_none() {
            diagnostic(
                &mut diagnostics,
                DiagnosticCode::UnknownPort,
                format!("{path}/from/port"),
                format!("unknown output port {:?}", edge.from.port),
            );
        }
        if target_node && target_port.is_none() {
            diagnostic(
                &mut diagnostics,
                DiagnosticCode::UnknownPort,
                format!("{path}/to/port"),
                format!("unknown input port {:?}", edge.to.port),
            );
        }
        if let (Some(source_port), Some(target_port)) = (source_port, target_port) {
            if source_port.schema != target_port.schema
                || source_port.collection != target_port.collection
            {
                diagnostic(
                    &mut diagnostics,
                    DiagnosticCode::SchemaMismatch,
                    path.clone(),
                    format!(
                        "source collection/schema {:?}/{:?} does not match target {:?}/{:?}",
                        source_port.collection,
                        source_port.schema,
                        target_port.collection,
                        target_port.schema
                    ),
                );
            }
            if source_port.correlation_field != target_port.correlation_field {
                diagnostic(
                    &mut diagnostics,
                    DiagnosticCode::CorrelationMismatch,
                    path.clone(),
                    format!(
                        "source correlation field {:?} does not match target {:?}",
                        source_port.correlation_field, target_port.correlation_field
                    ),
                );
            }
            let cardinality_valid = match &edge.delivery {
                DeliveryMode::PerDocument => source_port.cardinality == target_port.cardinality,
                DeliveryMode::PerGroup { .. } => {
                    source_port.cardinality == PortCardinality::One
                        && target_port.cardinality == PortCardinality::Many
                }
            };
            if !cardinality_valid {
                diagnostic(
                    &mut diagnostics,
                    DiagnosticCode::CardinalityMismatch,
                    format!("{path}/delivery"),
                    "delivery mode is incompatible with the connected port cardinalities",
                );
            }
        }
        if let DeliveryMode::PerGroup {
            expected,
            timeout_secs,
        } = &edge.delivery
        {
            match expected {
                GroupCount::Static { count } if *count < 2 || *count > policy.max_group_size => {
                    diagnostic(
                        &mut diagnostics,
                        DiagnosticCode::InvalidGroupSize,
                        format!("{path}/delivery/expected/count"),
                        format!(
                            "per-group size must be between 2 and {}",
                            policy.max_group_size
                        ),
                    );
                }
                GroupCount::SourceField { field } if validate_graphql_name(field).is_err() => {
                    diagnostic(
                        &mut diagnostics,
                        DiagnosticCode::InvalidGroupCountField,
                        format!("{path}/delivery/expected/field"),
                        "source-field group count must be a GraphQL field identifier",
                    );
                }
                _ => {}
            }
            if timeout_secs
                .is_some_and(|seconds| seconds == 0 || seconds > policy.max_group_timeout_secs)
            {
                diagnostic(
                    &mut diagnostics,
                    DiagnosticCode::InvalidGroupTimeout,
                    format!("{path}/delivery/timeout_secs"),
                    format!(
                        "group timeout must be in 1..={} seconds when present",
                        policy.max_group_timeout_secs
                    ),
                );
            }
        }
        if let Some(predicate) = edge.predicate.as_deref() {
            if validate_graphql_filter_fragment(predicate).is_err() {
                diagnostic(
                    &mut diagnostics,
                    DiagnosticCode::InvalidPredicate,
                    format!("{path}/predicate"),
                    "predicate is not a safe GraphQL filter fragment",
                );
            }
        }
        if target_port.is_some() {
            *incoming.entry(edge.to.clone()).or_default() += 1;
        }
        if source_node && target_node {
            adjacency
                .entry(edge.from.node_id.as_str())
                .or_default()
                .insert(edge.to.node_id.as_str());
            *fan_out.entry(edge.from.node_id.as_str()).or_default() += 1;
        }
    }

    let mut entry_names = BTreeSet::new();
    let mut entry_nodes = BTreeSet::new();
    for (index, entry) in intent.entries.iter().enumerate() {
        let path = format!("/entries/{index}");
        if !entry_names.insert(entry.name.as_str()) {
            diagnostic(
                &mut diagnostics,
                DiagnosticCode::DuplicateEntry,
                format!("{path}/name"),
                format!("entry {:?} is declared more than once", entry.name),
            );
        }
        let target_node = nodes.contains(entry.to.node_id.as_str());
        if !target_node {
            diagnostic(
                &mut diagnostics,
                DiagnosticCode::UnknownNode,
                format!("{path}/to/node_id"),
                format!("unknown target node {:?}", entry.to.node_id),
            );
            continue;
        }
        let target_port = resolved
            .get(entry.to.node_id.as_str())
            .and_then(|capability| input_port(capability, &entry.to.port));
        let Some(target_port) = target_port else {
            diagnostic(
                &mut diagnostics,
                DiagnosticCode::UnknownPort,
                format!("{path}/to/port"),
                format!("unknown input port {:?}", entry.to.port),
            );
            continue;
        };
        if entry.schema != target_port.schema {
            diagnostic(
                &mut diagnostics,
                DiagnosticCode::SchemaMismatch,
                format!("{path}/schema"),
                format!(
                    "entry schema {:?} does not match target schema {:?}",
                    entry.schema, target_port.schema
                ),
            );
        }
        if entry.collection != target_port.collection {
            diagnostic(
                &mut diagnostics,
                DiagnosticCode::SchemaMismatch,
                format!("{path}/collection"),
                format!(
                    "entry collection {:?} does not match approved target collection {:?}",
                    entry.collection, target_port.collection
                ),
            );
        }
        if target_port.cardinality != PortCardinality::One {
            diagnostic(
                &mut diagnostics,
                DiagnosticCode::CardinalityMismatch,
                format!("{path}/to"),
                "a v1 entry binds exactly one document and requires a one-valued input",
            );
        }
        *incoming.entry(entry.to.clone()).or_default() += 1;
        entry_nodes.insert(entry.to.node_id.as_str());
    }

    if !intent.results.iter().any(|result| result.terminal) {
        diagnostic(
            &mut diagnostics,
            DiagnosticCode::MissingTerminalResult,
            "/results",
            "a graph must declare at least one terminal result so every completed run can reach a durable terminal state",
        );
    }
    let mut result_names = BTreeSet::new();
    for (index, result) in intent.results.iter().enumerate() {
        let path = format!("/results/{index}");
        if !result_names.insert(result.name.as_str()) {
            diagnostic(
                &mut diagnostics,
                DiagnosticCode::DuplicateResult,
                format!("{path}/name"),
                format!("result {:?} is declared more than once", result.name),
            );
        }
        let source_node = nodes.contains(result.from.node_id.as_str());
        if !source_node {
            diagnostic(
                &mut diagnostics,
                DiagnosticCode::UnknownNode,
                format!("{path}/from/node_id"),
                format!("unknown result node {:?}", result.from.node_id),
            );
        } else if resolved
            .get(result.from.node_id.as_str())
            .and_then(|capability| output_port(capability, &result.from.port))
            .is_none()
        {
            diagnostic(
                &mut diagnostics,
                DiagnosticCode::UnknownPort,
                format!("{path}/from/port"),
                format!("unknown result output port {:?}", result.from.port),
            );
        }
        let count = match result.cardinality {
            ResultCardinality::Exactly { count } | ResultCardinality::AtMost { count } => count,
        };
        if count == 0 || count > policy.max_total_invocations {
            diagnostic(
                &mut diagnostics,
                DiagnosticCode::InvalidResultCardinality,
                format!("{path}/cardinality/count"),
                format!(
                    "result cardinality must be in 1..={}, found {count}",
                    policy.max_total_invocations
                ),
            );
        }
    }

    for (node_id, capability) in &resolved {
        for port in capability.input_ports.iter().filter(|port| port.required) {
            let port_ref = PortRef {
                node_id: (*node_id).to_owned(),
                port: port.name.clone(),
            };
            match incoming.get(&port_ref).copied().unwrap_or_default() {
                0 => diagnostic(
                    &mut diagnostics,
                    DiagnosticCode::MissingInputBinding,
                    format!("/nodes/{node_id}/inputs/{}", port.name),
                    "required input has no entry or inbound edge",
                ),
                1 => {}
                count => diagnostic(
                    &mut diagnostics,
                    DiagnosticCode::MultipleInputBindings,
                    format!("/nodes/{node_id}/inputs/{}", port.name),
                    format!("required input has {count} bindings; exactly one is allowed"),
                ),
            }
        }
    }

    for (node_id, count) in fan_out {
        if count > intent.limits.max_fan_out {
            diagnostic(
                &mut diagnostics,
                DiagnosticCode::FanOutLimitExceeded,
                format!("/nodes/{node_id}/fan_out"),
                format!(
                    "node fan-out is {count}, limit is {}",
                    intent.limits.max_fan_out
                ),
            );
        }
    }

    let mut reachable = entry_nodes.clone();
    let mut queue: VecDeque<&str> = entry_nodes.into_iter().collect();
    while let Some(node_id) = queue.pop_front() {
        for target in adjacency.get(node_id).into_iter().flatten() {
            if reachable.insert(target) {
                queue.push_back(target);
            }
        }
    }
    for node_id in &nodes {
        if !reachable.contains(node_id) {
            diagnostic(
                &mut diagnostics,
                DiagnosticCode::UnreachableNode,
                format!("/nodes/{node_id}"),
                "node is not reachable from an entry binding",
            );
        }
    }

    let mut indegree: BTreeMap<&str, u32> =
        nodes.iter().copied().map(|node_id| (node_id, 0)).collect();
    for targets in adjacency.values() {
        for target in targets {
            *indegree.entry(target).or_default() += 1;
        }
    }
    let mut topo_queue: VecDeque<&str> = indegree
        .iter()
        .filter_map(|(node_id, degree)| (*degree == 0).then_some(*node_id))
        .collect();
    let mut visited = 0_usize;
    let mut depth: BTreeMap<&str, u32> = indegree
        .keys()
        .copied()
        .map(|node_id| (node_id, 1))
        .collect();
    while let Some(node_id) = topo_queue.pop_front() {
        visited += 1;
        let source_depth = depth.get(node_id).copied().unwrap_or(1);
        for target in adjacency.get(node_id).into_iter().flatten() {
            depth
                .entry(target)
                .and_modify(|value| *value = (*value).max(source_depth.saturating_add(1)));
            let degree = indegree.entry(target).or_default();
            *degree -= 1;
            if *degree == 0 {
                topo_queue.push_back(target);
            }
        }
    }
    if visited != nodes.len() {
        diagnostic(
            &mut diagnostics,
            DiagnosticCode::Cycle,
            "/edges",
            "v1 graph intents must be acyclic",
        );
    } else if let Some(actual_depth) = depth.values().max().copied() {
        if actual_depth > intent.limits.max_depth {
            diagnostic(
                &mut diagnostics,
                DiagnosticCode::DepthLimitExceeded,
                "/limits/max_depth",
                format!(
                    "graph depth is {actual_depth}, limit is {}",
                    intent.limits.max_depth
                ),
            );
        }
    }

    if !diagnostics.is_empty() {
        sorted_diagnostics(&mut diagnostics);
        return Err(GraphCompileError { diagnostics });
    }

    let mut planned_nodes: Vec<PlannedNode> = intent
        .nodes
        .iter()
        .map(|node| {
            let capability = resolved[node.node_id.as_str()];
            PlannedNode {
                node_id: node.node_id.clone(),
                capability_id: capability.capability_id.clone(),
                capability_revision: capability.revision.clone(),
                task_id: capability.task_id.clone(),
            }
        })
        .collect();
    planned_nodes.sort_by(|left, right| left.node_id.cmp(&right.node_id));

    let mut edges: Vec<PlannedEdge> = intent
        .edges
        .iter()
        .map(|edge| {
            let source_capability = resolved[edge.from.node_id.as_str()];
            let source_port = output_port(source_capability, &edge.from.port)
                .expect("validated source port must resolve");
            PlannedEdge {
                from: edge.from.clone(),
                to: edge.to.clone(),
                source_collection: source_port.collection.clone(),
                target_task_id: resolved[edge.to.node_id.as_str()].task_id.clone(),
                correlation_field: source_port.correlation_field.clone(),
                delivery: edge.delivery.clone(),
                concurrency: edge.concurrency.clone(),
                predicate: edge.predicate.clone(),
            }
        })
        .collect();
    edges.sort_by(|left, right| {
        (
            &left.from,
            &left.to,
            delivery_sort_key(&left.delivery),
            &left.predicate,
        )
            .cmp(&(
                &right.from,
                &right.to,
                delivery_sort_key(&right.delivery),
                &right.predicate,
            ))
    });
    let mut entries: Vec<PlannedEntry> = intent
        .entries
        .iter()
        .map(|entry| {
            let target_capability = resolved[entry.to.node_id.as_str()];
            let target_port = input_port(target_capability, &entry.to.port)
                .expect("validated target port must resolve");
            PlannedEntry {
                name: entry.name.clone(),
                collection: target_port.collection.clone(),
                schema: target_port.schema.clone(),
                input_contract: entry.input_contract.clone(),
                to: entry.to.clone(),
                target_task_id: target_capability.task_id.clone(),
                correlation_field: target_port.correlation_field.clone(),
            }
        })
        .collect();
    entries.sort_by(|left, right| {
        (&left.name, &left.schema, &left.to).cmp(&(&right.name, &right.schema, &right.to))
    });

    let mut results: Vec<PlannedResult> = intent
        .results
        .iter()
        .map(|result| {
            let source_capability = resolved[result.from.node_id.as_str()];
            let source_port = output_port(source_capability, &result.from.port)
                .expect("validated result port must resolve");
            PlannedResult {
                name: result.name.clone(),
                from: result.from.clone(),
                collection: source_port.collection.clone(),
                schema: source_port.schema.clone(),
                correlation_field: source_port.correlation_field.clone(),
                cardinality: result.cardinality.clone(),
                terminal: result.terminal,
            }
        })
        .collect();
    results.sort_by(|left, right| (&left.name, &left.from).cmp(&(&right.name, &right.from)));

    let mut manifest: Vec<CapabilityManifestEntry> = resolved
        .values()
        .map(|capability| CapabilityManifestEntry {
            capability_id: capability.capability_id.clone(),
            revision: capability.revision.clone(),
            task_id: capability.task_id.clone(),
        })
        .collect();
    manifest.sort_by(|left, right| {
        (&left.capability_id, &left.revision, &left.task_id).cmp(&(
            &right.capability_id,
            &right.revision,
            &right.task_id,
        ))
    });
    manifest.dedup();
    let mut plan = GraphPlan {
        compiler_version: COMPILER_VERSION.to_owned(),
        graph_id: intent.graph_id.clone(),
        digest: String::new(),
        nodes: planned_nodes,
        edges,
        entries,
        results,
        capability_manifest: manifest,
        limits: intent.limits.clone(),
        package: None,
    };
    plan.digest = graph_plan_digest(&plan);
    Ok(plan)
}

/// Recompute the content identity of a plan while excluding its `digest`
/// field. Publication controllers use this to reject a tampered plan before
/// materializing any artifact.
pub fn graph_plan_digest(plan: &GraphPlan) -> String {
    #[derive(Serialize)]
    struct DigestPayload<'a> {
        compiler_version: &'a str,
        graph_id: &'a str,
        nodes: &'a [PlannedNode],
        edges: &'a [PlannedEdge],
        entries: &'a [PlannedEntry],
        results: &'a [PlannedResult],
        capability_manifest: &'a [CapabilityManifestEntry],
        limits: &'a super::types::GraphLimits,
        package: &'a Option<PackagePlan>,
    }

    let bytes = serde_json::to_vec(&DigestPayload {
        compiler_version: &plan.compiler_version,
        graph_id: &plan.graph_id,
        nodes: &plan.nodes,
        edges: &plan.edges,
        entries: &plan.entries,
        results: &plan.results,
        capability_manifest: &plan.capability_manifest,
        limits: &plan.limits,
        package: &plan.package,
    })
    .expect("GraphPlan contains only infallibly serializable fields");
    format!("sha256:{:x}", Sha256::digest(bytes))
}

/// Bind typed package/configuration provenance to an already validated graph.
/// Package validation owns uniqueness and digest checks; this function only
/// canonicalizes order and advances the immutable revision identity.
pub fn bind_package_plan(mut plan: GraphPlan, mut package: PackagePlan) -> GraphPlan {
    package.artifacts.sort();
    package.required_schema_digests.sort();
    plan.package = Some(package);
    plan.digest.clear();
    plan.digest = graph_plan_digest(&plan);
    plan
}

pub fn verify_graph_plan_digest(plan: &GraphPlan) -> bool {
    plan.digest == graph_plan_digest(plan)
}
