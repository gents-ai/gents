use chrono::{DateTime, Utc};
use gents_protocol::output::PayloadPresentation;

use super::FailureClass;
use gents_session::session::tool_call_state::{CancelCause, ToolCallState};

mod background_receipt;
mod dispatch;
mod output_append;
mod presentation;
mod spawned;
mod terminal_effects;
mod terminal_transaction;
mod terminalize;

pub(crate) use background_receipt::{publish_background_receipt_in_txn, BackgroundReceiptBinding};
pub(crate) use dispatch::{start_running_in_txn, DispatchStart};
#[cfg(any(test, feature = "test-fixtures"))]
pub use output_append::append_tool_output;
pub(crate) use output_append::ToolOutputBinding;
#[cfg(any(test, feature = "test-fixtures"))]
pub use output_append::{append_tool_output_at, ToolOutputAppendRejection};

#[cfg(test)]
mod plugin_writes_failed_tests;
#[cfg(test)]
mod spawned_background_tests;

#[derive(Debug, thiserror::Error)]
pub enum ToolDispatchRejection {
    #[error("dispatch lost exact request ownership or cancellation")]
    LostRequestOwnership,
}

/// Data writes a plugin tool returned beside its output, deferred to the
/// terminal of the tool call that returned them.
#[derive(Clone)]
pub(crate) struct PluginToolWrites {
    pub(crate) actor: identity::Did,
    pub(crate) grant: gents_pack::plugin::store::PluginData,
    pub(crate) writes: Vec<gents_protocol::job::DataWrite>,
}

#[cfg(any(test, feature = "test-fixtures"))]
static FAIL_NEXT_TERMINAL: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());

#[cfg(any(test, feature = "test-fixtures"))]
pub fn fail_next_terminal_for_test(tool_call_doc_id: &str) {
    FAIL_NEXT_TERMINAL
        .lock()
        .unwrap()
        .push(tool_call_doc_id.to_owned());
}

#[cfg(any(test, feature = "test-fixtures"))]
pub fn terminal_failure_pending_for_test(tool_call_doc_id: &str) -> bool {
    FAIL_NEXT_TERMINAL
        .lock()
        .unwrap()
        .iter()
        .any(|id| id == tool_call_doc_id)
}

#[cfg(any(test, feature = "test-fixtures"))]
fn take_terminal_failure_for_test(tool_call_doc_id: &str) -> bool {
    let mut pending = FAIL_NEXT_TERMINAL.lock().unwrap();
    if let Some(index) = pending.iter().position(|id| id == tool_call_doc_id) {
        pending.remove(index);
        true
    } else {
        false
    }
}

/// A deferred plugin write was refused or failed inside the terminal's
/// transaction, which rolled back whole.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct PluginWritesFailed(String);

/// Marks a refusal. A conflict or storage failure passes through unmarked so
/// the transaction owner replays it and, once exhausted, it stays a transient
/// error rather than a plugin refusal.
fn plugin_writes_failed(error: anyhow::Error) -> anyhow::Error {
    if gents_store::config_client::is_transaction_step_unavailable(&error) {
        return error;
    }
    let text = format!("{error:#}");
    error.context(PluginWritesFailed(text))
}

pub(super) struct RawPresentation<'a> {
    pub(super) raw: &'a str,
    pub(super) rendered: &'a str,
    pub(super) presentation: PayloadPresentation,
}

struct TerminalizeOptions<'a> {
    text: &'a str,
    pending_raw: Option<&'a str>,
    presentation: Option<PayloadPresentation>,
    adopt_competing_terminal: bool,
    publish_native_result: bool,
    operation: &'static str,
    fixture_now: Option<DateTime<Utc>>,
}

#[derive(Clone, Copy)]
pub(super) struct TerminalFields<'a> {
    pub state: ToolCallState,
    pub failure: Option<FailureClass>,
    pub cancel: Option<CancelCause>,
    pub completion_reason: Option<&'a str>,
}

/// A job row's progress, written at `expected_revision` in its terminal
/// transaction once the terminal compare wins.
#[derive(Clone)]
pub(super) struct TerminalJobProgress {
    pub expected_revision: u64,
    pub guest: gents_protocol::job::JobProgress,
    pub host: crate::background::job::records::HostProgressFacts,
}
