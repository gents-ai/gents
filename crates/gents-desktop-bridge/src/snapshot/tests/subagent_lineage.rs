#[path = "../../../../../crates/gents/src/lean_vocab_test.rs"]
mod lean_vocab_test;

use crate::types::{SubagentEdgeView, SubagentNodeView, SubagentTreeView};
use lean_vocab_test::{lean_r5_cross_deployment_cases, LeanR5CrossDeploymentCase};

/// Build a UI-shaped `SubagentTreeView` from a Lean R5 cross-deployment case.
///
/// The desktop panel renders this exact shape — the runtime `/subagents/tree`
/// handler emits it, and `SubagentLineageView` consumes it. Synthesizing it
/// here from the Lean witness lets us assert that every UI-renderable
/// invariant the contract describes (deployment split, bridge metadata,
/// caused-by lineage) survives the trip into the operator surface.
fn subagent_tree_view_from_lean_case(case: &LeanR5CrossDeploymentCase) -> SubagentTreeView {
    SubagentTreeView {
        partial_errors: Vec::new(),
        root_request_id: case.parent_request_id.clone(),
        nodes: vec![
            SubagentNodeView {
                resolved_via: None,
                request_id: case.parent_request_id.clone(),
                session_id: None,
                agent_did: Some(case.parent_deployment.clone()),
                behavior_id: None,
                lifecycle_state: Some("Processing".to_string()),
                status: Some("processing".to_string()),
                subagent_depth: Some(0),
                caused_by_parent_request_id: None,
                caused_by_parent_tool_call_id: None,
                backend_id: None,
            },
            SubagentNodeView {
                resolved_via: None,
                request_id: case.child_request_id.clone(),
                session_id: None,
                agent_did: Some(case.child_deployment.clone()),
                behavior_id: Some(case.target_behavior_id.clone()),
                lifecycle_state: Some("Processing".to_string()),
                status: Some("processing".to_string()),
                subagent_depth: Some(1),
                caused_by_parent_request_id: case
                    .caused_by_parent_request_id_matches
                    .then(|| case.parent_request_id.clone()),
                caused_by_parent_tool_call_id: case
                    .caused_by_parent_tool_call_id_matches
                    .then(|| case.parent_tool_call_id.clone()),
                backend_id: None,
            },
        ],
        edges: vec![SubagentEdgeView {
            parent_request_id: case.parent_request_id.clone(),
            child_request_id: case.child_request_id.clone(),
            parent_tool_call_id: Some(case.parent_tool_call_id.clone()),
            tool_name: Some(case.action.clone()),
            await_mode: Some(case.await_mode.clone()),
            cancel_policy: Some(case.cancel_policy.clone()),
            lifecycle_state: Some("running".to_string()),
        }],
        truncated: false,
    }
}

#[test]
fn subagent_tree_view_consumes_generated_r5_cross_deployment_contract_cases() {
    let cases = lean_r5_cross_deployment_cases();
    assert!(
        !cases.is_empty(),
        "Lean R5 contract should emit cross-deployment cases"
    );

    let mut cross_seen = false;
    let mut local_seen = false;

    for case in cases {
        assert_eq!(case.action, "spawn_subagent", "{}", case.name);
        assert_eq!(case.await_mode, "background", "{}", case.name);
        assert_eq!(case.cancel_policy, "cascade", "{}", case.name);

        let view = subagent_tree_view_from_lean_case(case);
        assert_eq!(
            view.root_request_id, case.parent_request_id,
            "{}",
            case.name
        );
        assert_eq!(view.nodes.len(), 2, "{}", case.name);
        assert_eq!(view.edges.len(), 1, "{}", case.name);
        assert!(!view.truncated, "{} should not be truncated", case.name);

        let parent = view
            .nodes
            .iter()
            .find(|node| node.request_id == case.parent_request_id)
            .expect("parent node present");
        let child = view
            .nodes
            .iter()
            .find(|node| node.request_id == case.child_request_id)
            .expect("child node present");
        let edge = view.edges.first().expect("bridge edge present");

        assert_eq!(
            parent.agent_did.as_deref(),
            Some(case.parent_deployment.as_str())
        );
        assert_eq!(
            child.agent_did.as_deref(),
            Some(case.child_deployment.as_str())
        );
        assert_eq!(edge.tool_name.as_deref(), Some("spawn_subagent"));
        assert_eq!(edge.await_mode.as_deref(), Some("background"));
        assert_eq!(edge.cancel_policy.as_deref(), Some("cascade"));
        assert_eq!(
            edge.parent_tool_call_id.as_deref(),
            Some(case.parent_tool_call_id.as_str())
        );

        if case.cross_deployment_routing_fired {
            cross_seen = true;
            assert_ne!(
                parent.agent_did, child.agent_did,
                "{}: cross_deployment_routing_fired requires parent.agentDid != child.agentDid",
                case.name
            );
            assert!(
                case.child_owned_by_target_deployment,
                "{}: cross-deployment routing implies child owned by target",
                case.name
            );
        }
        if case.single_deployment_fallback {
            local_seen = true;
            assert_eq!(
                parent.agent_did, child.agent_did,
                "{}: single_deployment_fallback requires parent and child on same deployment",
                case.name
            );
        }

        if case.caused_by_parent_request_id_matches {
            assert_eq!(
                child.caused_by_parent_request_id.as_deref(),
                Some(case.parent_request_id.as_str()),
                "{}: caused_by_parent_request_id should carry through to UI",
                case.name
            );
        }
        if case.caused_by_parent_tool_call_id_matches {
            assert_eq!(
                child.caused_by_parent_tool_call_id.as_deref(),
                Some(case.parent_tool_call_id.as_str()),
                "{}: caused_by_parent_tool_call_id should carry through to UI",
                case.name
            );
        }

        // Round-trip through serde camelCase to confirm the panel receives the
        // same shape the bridge serializes. The runtime handler emits this
        // shape; the bridge re-serializes it for the React panel; the React
        // panel reads camelCase. A field rename here would break the whole
        // chain at once.
        let payload = serde_json::to_string(&view).expect("serialize subagent tree");
        assert!(payload.contains("\"rootRequestId\""), "{}", case.name);
        assert!(
            payload.contains("\"causedByParentRequestId\""),
            "{}",
            case.name
        );
        assert!(payload.contains("\"awaitMode\""), "{}", case.name);
        let round_tripped: SubagentTreeView =
            serde_json::from_str(&payload).expect("deserialize subagent tree");
        assert_eq!(round_tripped.root_request_id, view.root_request_id);
        assert_eq!(round_tripped.nodes.len(), view.nodes.len());
        assert_eq!(round_tripped.edges.len(), view.edges.len());
    }

    assert!(
        cross_seen,
        "Lean R5 contract should include a cross-deployment case"
    );
    assert!(
        local_seen,
        "Lean R5 contract should include a single-deployment fallback case"
    );
}
