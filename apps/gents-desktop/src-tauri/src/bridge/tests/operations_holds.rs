//! Holds strip bridge commands: list held tool calls, write the approval
//! decision. The row itself stays `awaitingApproval` after a resolve — the
//! owning runtime's verdict watcher (not running here) drives the
//! Lean-fenced approve/deny edge; the bridge only writes the document.

use crate::bridge::tauri_commands::operations::{
    list_tool_call_holds_for_core, resolve_tool_call_hold_for_core,
};
use crate::bridge::tests::support::boot_core;
use gents_desktop_bridge::types::{DesktopListHoldsRequest, DesktopResolveHoldRequest};

const AGENT_DID: &str = "did:test:holds-agent";

async fn seed_held_tool_call(core: &gents_desktop_core::client::ClientCore) {
    let deadline = (chrono::Utc::now() + chrono::Duration::seconds(120)).to_rfc3339();
    let mutation = format!(
        r#"mutation {{
            create_AgentToolCall(input: {{
                tool_call_key: "sess_holds:call_held",
                request_id: "req_holds",
                session_id: "sess_holds",
                agent_did: "{AGENT_DID}",
                message_sequence: 1,
                tool_name: "bash_unrestricted",
                tool_call_id: "call_held",
                args: "{{}}",
                result: "",
                status: "called",
                lifecycle_state: "awaitingApproval",
                started_at: null,
                deadline_at: "{deadline}"
            }}) {{ _docID }}
        }}"#
    );
    let response = core.node().execute(&mutation).await;
    assert!(
        !response.has_errors(),
        "seed held tool call failed: {:?}",
        response.errors
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn list_holds_returns_seeded_awaiting_approval_row() {
    let (core, _tmp) = boot_core().await;
    seed_held_tool_call(core.as_ref()).await;

    let held = list_tool_call_holds_for_core(
        core.clone(),
        DesktopListHoldsRequest {
            agent_did: AGENT_DID.to_string(),
        },
    )
    .await
    .expect("list holds");
    assert_eq!(held.len(), 1);
    assert_eq!(held[0].tool_call_id, "call_held");
    assert_eq!(held[0].tool_name.as_deref(), Some("bash_unrestricted"));
    assert_eq!(held[0].request_id.as_deref(), Some("req_holds"));

    // Other agents see nothing.
    let other = list_tool_call_holds_for_core(
        core.clone(),
        DesktopListHoldsRequest {
            agent_did: "did:test:someone-else".to_string(),
        },
    )
    .await
    .expect("list holds for other agent");
    assert!(other.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resolve_hold_writes_decision_signed_by_desktop_principal() {
    let (core, _tmp) = boot_core().await;
    seed_held_tool_call(core.as_ref()).await;

    let result = resolve_tool_call_hold_for_core(
        core.clone(),
        DesktopResolveHoldRequest {
            agent_did: AGENT_DID.to_string(),
            tool_call_id: "call_held".to_string(),
            approve: false,
            reason: Some("not on my watch".to_string()),
        },
    )
    .await
    .expect("resolve hold");
    assert_eq!(result.decision, "denied");
    assert_eq!(result.tool_call_id, "call_held");
    assert!(result.approval_id.starts_with("approval-call_held-"));

    let response = core
        .node()
        .execute(
            r#"{ AgentToolApproval(filter: { tool_call_id: { _eq: "call_held" } }) {
                decision reason approver_did agent_did
            } }"#,
        )
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolApproval"))
        .and_then(|rows| rows.as_array())
        .cloned()
        .unwrap_or_default();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].get("decision").and_then(|value| value.as_str()),
        Some("denied")
    );
    assert_eq!(
        rows[0].get("reason").and_then(|value| value.as_str()),
        Some("not on my watch")
    );
    assert_eq!(
        rows[0].get("approver_did").and_then(|value| value.as_str()),
        Some(core.principal().did())
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resolve_hold_rejects_unknown_tool_call() {
    let (core, _tmp) = boot_core().await;

    let error = resolve_tool_call_hold_for_core(
        core.clone(),
        DesktopResolveHoldRequest {
            agent_did: AGENT_DID.to_string(),
            tool_call_id: "no-such-call".to_string(),
            approve: true,
            reason: None,
        },
    )
    .await
    .expect_err("unknown hold must fail");
    assert!(error.contains("not awaiting approval"), "{error}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn resolve_hold_approve_writes_approved_decision() {
    let (core, _tmp) = boot_core().await;
    seed_held_tool_call(core.as_ref()).await;

    let result = resolve_tool_call_hold_for_core(
        core.clone(),
        DesktopResolveHoldRequest {
            agent_did: AGENT_DID.to_string(),
            tool_call_id: "call_held".to_string(),
            approve: true,
            reason: None,
        },
    )
    .await
    .expect("approve hold");
    assert_eq!(result.decision, "approved");

    let response = core
        .node()
        .execute(
            r#"{ AgentToolApproval(filter: { tool_call_id: { _eq: "call_held" } }) {
                decision reason
            } }"#,
        )
        .await;
    assert!(!response.has_errors(), "{:?}", response.errors);
    let rows = response
        .data
        .as_ref()
        .and_then(|data| data.get("AgentToolApproval"))
        .and_then(|rows| rows.as_array())
        .cloned()
        .unwrap_or_default();
    assert_eq!(rows.len(), 1);
    assert_eq!(
        rows[0].get("decision").and_then(|value| value.as_str()),
        Some("approved")
    );
    assert_eq!(rows[0].get("reason").and_then(|value| value.as_str()), None);
}
