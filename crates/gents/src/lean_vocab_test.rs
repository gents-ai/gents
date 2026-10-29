#[path = "lean_vocab_test/support.rs"]
mod support;

pub(crate) use support::canonical_presentation::native_adapter::native_presentation;
pub(crate) use support::*;

pub(crate) async fn native_queue_image(
    node: &std::sync::Arc<defra_node::EmbeddedNode>,
    node_did: &str,
    session_id: &str,
    selection: &[crate::lifecycle::queue::FoldedInput],
) -> QueueImage {
    use gents_protocol::request_lifecycle::RequestLifecycleState;
    #[derive(serde::Deserialize)]
    struct Row {
        request_id: String,
        lifecycle_state: RequestLifecycleState,
    }
    let access = crate::config_client::ConfigAccess::local(node.clone());
    let response = access
        .execute(&format!(
            r#"{{ AgentRequest(filter: {{ session_id: {{ _eq: "{}" }} }}) {{ request_id lifecycle_state }} }}"#,
            crate::graphql::escape_graphql_string(session_id)
        ))
        .await
        .unwrap();
    let rows: Vec<Row> = serde_json::from_value(response["data"]["AgentRequest"].clone()).unwrap();
    let state = |id: &str| {
        rows.iter()
            .find(|row| row.request_id == id)
            .map(|row| row.lifecycle_state)
    };
    let active = rows
        .iter()
        .filter(|row| {
            matches!(
                row.lifecycle_state,
                RequestLifecycleState::Claimed | RequestLifecycleState::Processing
            )
        })
        .map(|row| row.request_id.clone())
        .collect::<Vec<_>>();
    let folding = selection
        .iter()
        .map(|folded| folded.request_id.clone())
        .filter(|id| state(id) == Some(RequestLifecycleState::Pending))
        .collect::<Vec<_>>();
    let pending = crate::lifecycle::pending_user_queue(&access, node_did, session_id, node_did)
        .await
        .unwrap()
        .entries
        .into_iter()
        .map(|entry| entry.request_id)
        .filter(|id| !folding.contains(id))
        .collect::<Vec<_>>();
    let terminal = rows
        .iter()
        .filter(|row| row.lifecycle_state.is_terminal())
        .map(|row| row.request_id.clone())
        .collect::<std::collections::BTreeSet<_>>();
    assert!(active.len() <= 1, "one active request: {active:?}");
    assert_eq!(
        rows.len(),
        active.len() + folding.len() + pending.len() + terminal.len(),
        "every request is active, queued or terminal"
    );
    QueueImage {
        active: active.into_iter().next(),
        folding,
        pending,
        terminal,
    }
}

#[cfg(test)]
mod task_hook_vocab {
    use super::{LeanCommandResult, LeanTaskHook};
    pub(crate) fn lean_hook_phase(name: &str) -> crate::document_config::TaskHookPhase {
        use crate::document_config::TaskHookPhase;
        match name {
            "before" => TaskHookPhase::Before,
            "after_success" => TaskHookPhase::AfterSuccess,
            "after_failure" => TaskHookPhase::AfterFailure,
            "finally" => TaskHookPhase::Finally,
            other => panic!("generated contract emitted an unknown hook phase {other:?}"),
        }
    }

    impl LeanTaskHook {
        pub(crate) fn to_task_hook(&self) -> crate::document_config::TaskHook {
            crate::document_config::TaskHook {
                hook_id: self.hook_id.clone(),
                phase: lean_hook_phase(&self.phase),
                command: self.command.clone(),
                timeout_secs: self.timeout_secs,
                skip_exit_codes: self.skip_exit_codes.clone(),
            }
        }
    }

    impl LeanCommandResult {
        pub(crate) fn to_native(&self) -> crate::task_hooks::HookCommandResult {
            use crate::task_hooks::HookCommandResult;
            match self {
                Self::Exited { code } => HookCommandResult::Exited { code: Some(*code) },
                Self::LaunchFailed => HookCommandResult::LaunchFailed,
                Self::TimedOut => HookCommandResult::TimedOut,
                Self::Interrupted => HookCommandResult::Interrupted,
            }
        }
    }
}
#[cfg(test)]
pub(crate) use task_hook_vocab::lean_hook_phase;

#[cfg(test)]
#[path = "lean_vocab_test/tests.rs"]
mod tests;

#[cfg(test)]
#[path = "lean_vocab_test/request_execution_lease_policy.rs"]
mod request_execution_lease_policy;

#[cfg(test)]
#[path = "lean_vocab_test/task_hooks_policy.rs"]
mod task_hooks_policy;

#[cfg(test)]
#[path = "lean_vocab_test/task_hook_executor.rs"]
mod task_hook_executor;

#[cfg(test)]
#[path = "lean_vocab_test/task_hook_recovery.rs"]
mod task_hook_recovery;
