use super::runtime::{graph_trigger_id, GraphPipelineError};
use super::types::{GraphPlan, PlannedEdge, PlannedEntry, StageTarget};
use crate::Collection;

pub(super) struct PlannedRoute<'a> {
    pub(super) id: String,
    pub(super) node_id: &'a str,
    pub(super) target: &'a StageTarget,
}

pub(super) fn entry_route_id(
    digest: &str,
    entry: &PlannedEntry,
) -> Result<String, GraphPipelineError> {
    graph_trigger_id(
        digest,
        &format!(
            "entry:{}:{}:{}",
            entry.name, entry.to.node_id, entry.to.port
        ),
    )
}

pub(super) fn edge_route_id(
    digest: &str,
    index: usize,
    edge: &PlannedEdge,
) -> Result<String, GraphPipelineError> {
    graph_trigger_id(
        digest,
        &format!(
            "edge:{index}:{}:{}:{}:{}",
            edge.from.node_id, edge.from.port, edge.to.node_id, edge.to.port,
        ),
    )
}

pub(super) fn planned_routes(
    plan: &GraphPlan,
) -> Result<Vec<PlannedRoute<'_>>, GraphPipelineError> {
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

pub(crate) fn revision_artifact_ids(
    plan: &GraphPlan,
) -> Result<Vec<(Collection, String)>, GraphPipelineError> {
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
