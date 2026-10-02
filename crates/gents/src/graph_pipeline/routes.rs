//! One route list, shared by the materializer's identity preview and an
//! installation record: an entry or an edge, keyed by the same digest-scoped
//! id [`super::runtime::graph_trigger_id`] gives its `EventSource` and
//! delivery documents. The target decides what a route delivers into (a
//! `Trigger` for a Task target, a `Callback` and its `CallbackBinding` for a
//! Plugin target); keeping that mapping in one place is what keeps a
//! preview honest about what materializing the same plan actually writes.

use anyhow::Result;

use super::runtime::graph_trigger_id;
use super::types::{GraphPlan, PlannedEdge, PlannedEntry, StageTarget};
use crate::Collection;

/// One route a compiled plan materializes.
pub(super) struct PlannedRoute<'a> {
    pub(super) id: String,
    pub(super) node_id: &'a str,
    pub(super) target: &'a StageTarget,
}

/// The id of the route that delivers `entry` into its node.
pub(super) fn entry_route_id(digest: &str, entry: &PlannedEntry) -> Result<String> {
    graph_trigger_id(
        digest,
        &format!(
            "entry:{}:{}:{}",
            entry.name, entry.to.node_id, entry.to.port
        ),
    )
}

/// The id of the route that delivers the plan's `index`th edge.
pub(super) fn edge_route_id(digest: &str, index: usize, edge: &PlannedEdge) -> Result<String> {
    graph_trigger_id(
        digest,
        &format!(
            "edge:{index}:{}:{}:{}:{}",
            edge.from.node_id, edge.from.port, edge.to.node_id, edge.to.port,
        ),
    )
}

/// Every route `plan` materializes, entries then edges, in plan order.
pub(super) fn planned_routes(plan: &GraphPlan) -> Result<Vec<PlannedRoute<'_>>> {
    let mut routes = Vec::with_capacity(plan.entries.len() + plan.edges.len());
    for entry in &plan.entries {
        routes.push(PlannedRoute {
            id: entry_route_id(&plan.digest, entry)?,
            node_id: &entry.to.node_id,
            target: &entry.target,
        });
    }
    for (index, edge) in plan.edges.iter().enumerate() {
        routes.push(PlannedRoute {
            id: edge_route_id(&plan.digest, index, edge)?,
            node_id: &edge.to.node_id,
            target: &edge.target,
        });
    }
    Ok(routes)
}

/// Every document a materialized route delivers into: an `EventSource` for
/// every route, plus a `Trigger` for a Task target or a `Callback` and its
/// `CallbackBinding` for a Plugin target, mirroring
/// `runtime::planned_route_documents`'s own collection choice exactly (a
/// preview that predicted only `Trigger` for a plugin-target graph would
/// diverge from what materializing it actually writes).
pub(crate) fn revision_artifact_ids(plan: &GraphPlan) -> Result<Vec<(Collection, String)>> {
    let mut ids = Vec::new();
    for route in planned_routes(plan)? {
        ids.push((Collection::EventSource, route.id.clone()));
        match route.target {
            StageTarget::Task { .. } => ids.push((Collection::Trigger, route.id)),
            StageTarget::Plugin { .. } => {
                ids.push((Collection::Callback, route.id.clone()));
                ids.push((Collection::CallbackBinding, route.id));
            }
        }
    }
    Ok(ids)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document_config::ConcurrencyMode;
    use crate::graph_pipeline::types::{
        CapabilityManifestEntry, GraphLimits, PlannedEdge, PlannedEntry, PlannedNode, PortRef,
        COMPILER_VERSION,
    };

    /// A minimal plan with one Task-target entry and one Plugin-target edge,
    /// the two shapes `planned_route_documents` writes differently. Before
    /// this shared route list, `prospective_graph_artifact_identities`
    /// predicted `Trigger` for both, diverging from what a plugin-target
    /// route actually materializes (`Callback` + `CallbackBinding`).
    fn plan() -> GraphPlan {
        let digest = format!("sha256:{}", "a".repeat(64));
        GraphPlan {
            compiler_version: COMPILER_VERSION.to_owned(),
            graph_id: "route-test".to_owned(),
            digest,
            nodes: vec![
                PlannedNode {
                    node_id: "recon".to_owned(),
                    capability_id: "recon".to_owned(),
                    capability_revision: "1".to_owned(),
                    target: StageTarget::Task {
                        task_id: "recon-task".to_owned(),
                    },
                    output_ports: Vec::new(),
                    session: None,
                },
                PlannedNode {
                    node_id: "scan".to_owned(),
                    capability_id: "scan".to_owned(),
                    capability_revision: "1".to_owned(),
                    target: StageTarget::Plugin {
                        plugin: "acme/scan".to_owned(),
                        digest: Some(format!("sha256:{}", "b".repeat(64))),
                        max_attempts: None,
                    },
                    output_ports: Vec::new(),
                    session: None,
                },
            ],
            edges: vec![PlannedEdge {
                from: PortRef {
                    node_id: "recon".to_owned(),
                    port: "out".to_owned(),
                },
                to: PortRef {
                    node_id: "scan".to_owned(),
                    port: "in".to_owned(),
                },
                source_collection: "ReconResult".to_owned(),
                target: StageTarget::Plugin {
                    plugin: "acme/scan".to_owned(),
                    digest: Some(format!("sha256:{}", "b".repeat(64))),
                    max_attempts: None,
                },
                correlation_field: "correlation".to_owned(),
                delivery: None,
                concurrency: ConcurrencyMode::Parallel,
                predicate: None,
            }],
            entries: vec![PlannedEntry {
                name: "job".to_owned(),
                collection: "ReviewJob".to_owned(),
                schema: "review".to_owned(),
                input_contract: None,
                input_schema: None,
                prepare: None,
                to: PortRef {
                    node_id: "recon".to_owned(),
                    port: "in".to_owned(),
                },
                target: StageTarget::Task {
                    task_id: "recon-task".to_owned(),
                },
                correlation_field: "correlation".to_owned(),
            }],
            results: Vec::new(),
            capability_manifest: vec![
                CapabilityManifestEntry {
                    capability_id: "recon".to_owned(),
                    revision: "1".to_owned(),
                    target: StageTarget::Task {
                        task_id: "recon-task".to_owned(),
                    },
                },
                CapabilityManifestEntry {
                    capability_id: "scan".to_owned(),
                    revision: "1".to_owned(),
                    target: StageTarget::Plugin {
                        plugin: "acme/scan".to_owned(),
                        digest: Some(format!("sha256:{}", "b".repeat(64))),
                        max_attempts: None,
                    },
                },
            ],
            limits: GraphLimits {
                max_nodes: 8,
                max_edges: 8,
                max_depth: 8,
                max_fan_out: 8,
                max_total_invocations: 8,
                max_runtime_secs: 60,
            },
            package: None,
        }
    }

    #[test]
    fn a_task_target_route_is_a_trigger_and_a_plugin_target_route_is_a_callback() {
        let plan = plan();
        let ids = revision_artifact_ids(&plan).unwrap();
        let collections_for = |target_is_task: bool| -> Vec<Collection> {
            let index = if target_is_task { 0 } else { 1 };
            let route_id = planned_routes(&plan).unwrap().remove(index).id;
            ids.iter()
                .filter(|(_, id)| *id == route_id)
                .map(|(collection, _)| *collection)
                .collect()
        };
        let mut task_collections = collections_for(true);
        task_collections.sort();
        assert_eq!(
            task_collections,
            vec![Collection::EventSource, Collection::Trigger]
        );

        let mut plugin_collections = collections_for(false);
        plugin_collections.sort();
        assert_eq!(
            plugin_collections,
            vec![
                Collection::EventSource,
                Collection::Callback,
                Collection::CallbackBinding
            ]
        );
    }
}
