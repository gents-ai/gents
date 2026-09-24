//! Native refinement of the generated cross-principal spawn contract.
//!
//! Unlike the generic canonical-execution `nativeCommand` fixture, this drives
//! provider publication, immutable spawn admission, replication, and the real
//! target-side subagent reconciler.

use crate::lean_vocab_test::lean_r5_cross_principal_cases;
use crate::support::native_remote_spawn::{wait_for_bridge, wait_for_child};
use crate::support::r5_cross_principal_runtime::{
    boot_cross_principal_accepted_turn, R5AcceptedSpec,
};

pub(super) async fn generated_remote_spawn_contract_drives_native_seam() {
    let case = lean_r5_cross_principal_cases()
        .iter()
        .find(|case| case.cross_principal_routing_fired)
        .expect("Lean exports a cross-principal spawn case");
    assert_eq!(case.action, "spawn_subagent");
    assert!(case.child_owned_by_target_principal);
    assert!(case.caused_by_parent_request_id_matches);
    assert!(case.caused_by_parent_tool_call_id_matches);

    let session_id = format!("{}-native-seam-session", case.parent_request_id);
    let runtime = boot_cross_principal_accepted_turn(R5AcceptedSpec {
        name: "native-remote-spawn-seam",
        parent_request_id: &case.parent_request_id,
        parent_session_id: &session_id,
        parent_tool_call_id: &case.parent_tool_call_id,
        target_behavior_id: &case.target_behavior_id,
        prompt: "publish one real remote spawn_subagent call",
        parent_subagent_depth: 0,
        hold_child_provider: false,
    })
    .await;

    let parent_bridge = wait_for_bridge(
        runtime.parent_db.node.as_ref(),
        &session_id,
        &case.parent_tool_call_id,
    )
    .await;
    let child_request_id = parent_bridge
        .child_request_id
        .as_deref()
        .expect("accepted spawn reserves immutable child identity");
    let target_bridge = wait_for_bridge(
        runtime.child_db.node.as_ref(),
        &session_id,
        &case.parent_tool_call_id,
    )
    .await;
    let child = wait_for_child(runtime.child_db.node.as_ref(), child_request_id).await;

    // Lifecycle may advance independently after replication. Compare only the
    // immutable admission coordinates; this is not a provider-publication
    // replay claim.
    assert_eq!(parent_bridge.doc_id, target_bridge.doc_id);
    assert_eq!(parent_bridge.request_id, target_bridge.request_id);
    assert_eq!(parent_bridge.request_doc_id, target_bridge.request_doc_id);
    assert_eq!(parent_bridge.tool_call_id, target_bridge.tool_call_id);
    assert_eq!(parent_bridge.tool_name, target_bridge.tool_name);
    assert_eq!(
        parent_bridge.child_request_id,
        target_bridge.child_request_id
    );
    assert_eq!(
        parent_bridge.spawn_target_did,
        target_bridge.spawn_target_did
    );
    assert_eq!(
        parent_bridge.spawn_behavior_id,
        target_bridge.spawn_behavior_id
    );
    assert_eq!(parent_bridge.await_mode, target_bridge.await_mode);
    assert_eq!(parent_bridge.cancel_policy, target_bridge.cancel_policy);
    assert_eq!(
        parent_bridge.delegated_workspace,
        target_bridge.delegated_workspace
    );
    assert_eq!(parent_bridge.delegated_input, target_bridge.delegated_input);
    assert_eq!(parent_bridge.tool_name, "spawn_subagent");
    assert_eq!(parent_bridge.request_id, case.parent_request_id);
    assert_eq!(parent_bridge.tool_call_id, case.parent_tool_call_id);
    assert_eq!(
        parent_bridge.spawn_target_did.as_deref(),
        Some(runtime.child_agent_did.as_str())
    );
    assert_eq!(
        parent_bridge.spawn_behavior_id.as_deref(),
        Some(case.target_behavior_id.as_str())
    );
    assert_eq!(
        parent_bridge.await_mode.as_deref(),
        Some(case.await_mode.as_str())
    );
    assert_eq!(
        parent_bridge.cancel_policy.as_deref(),
        Some(case.cancel_policy.as_str())
    );
    // No workspace is attached to this generated route. Its exact absence is
    // itself immutable and must survive replication rather than being inferred
    // or replaced by target-local configuration.
    assert!(parent_bridge.delegated_workspace.is_none());
    let delegated_input = parent_bridge
        .delegated_input
        .as_ref()
        .expect("remote spawn carries immutable delegated input");
    // R5 currently models a root parent. Depth 0 is the native refinement
    // input; the child-depth increment below is a native owner invariant.
    assert_eq!(
        delegated_input
            .get("parent_subagent_depth")
            .and_then(serde_json::Value::as_u64),
        Some(0)
    );
    let arguments = delegated_input
        .get("arguments")
        .and_then(serde_json::Value::as_str)
        .and_then(|value| serde_json::from_str::<serde_json::Value>(value).ok())
        .expect("delegated input retains exact provider arguments");
    assert_eq!(
        arguments.get("name").and_then(serde_json::Value::as_str),
        Some(case.target_behavior_id.as_str())
    );
    assert_eq!(
        arguments
            .get("await_mode")
            .and_then(serde_json::Value::as_str),
        Some(case.await_mode.as_str())
    );

    assert_eq!(child.request_id, child_request_id);
    assert_eq!(
        child.content,
        arguments
            .get("prompt")
            .and_then(serde_json::Value::as_str)
            .expect("spawn arguments retain child prompt")
    );
    assert_eq!(
        child.agent_did.as_deref(),
        Some(runtime.child_agent_did.as_str())
    );
    assert_eq!(
        child.requester_did.as_deref(),
        Some(runtime.parent_db.node_identity.did())
    );
    assert_eq!(
        child.behavior_id.as_deref(),
        Some(case.target_behavior_id.as_str())
    );
    assert_eq!(child.subagent_depth, Some(1));
    assert_eq!(
        child.caused_by_parent_request_id.as_deref(),
        Some(case.parent_request_id.as_str())
    );
    assert_eq!(
        child.caused_by_parent_request_doc_id.as_deref(),
        Some(parent_bridge.request_doc_id.as_str())
    );
    assert_eq!(
        child.caused_by_parent_tool_call_id.as_deref(),
        Some(case.parent_tool_call_id.as_str())
    );
    assert_eq!(
        child.caused_by_parent_tool_call_doc_id.as_deref(),
        Some(parent_bridge.doc_id.as_str())
    );
    assert_eq!(
        child.caused_by_trigger_kind.as_deref(),
        Some(case.caused_by_trigger_kind.as_str())
    );
    runtime.shutdown().await;
}
