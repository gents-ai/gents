use std::sync::Arc;

use defra_node::EmbeddedNode;
use rig::completion::{CompletionError, Usage};
use tokio::sync::OwnedSemaphorePermit;

use super::controller::{BackendAdmissionController, InferenceCallRecord};
use super::persistence::{persist_existing_call_terminal, spawn_persistence};
use super::stream_guard::StreamGuardLifecycle;

pub(crate) struct AdmissionPermit {
    node: Arc<EmbeddedNode>,
    controller: Arc<BackendAdmissionController>,
    _permit: OwnedSemaphorePermit,
    call: InferenceCallRecord,
    _doc_id: String,
    terminal: Option<PermitTerminal>,
    finished: bool,
}

#[derive(Clone, Debug)]
struct PermitTerminal {
    call_state: &'static str,
    failure_reason: Option<String>,
    usage: Option<Usage>,
}

impl AdmissionPermit {
    pub(super) fn new(
        node: Arc<EmbeddedNode>,
        controller: Arc<BackendAdmissionController>,
        permit: OwnedSemaphorePermit,
        call: InferenceCallRecord,
        doc_id: String,
    ) -> Self {
        Self {
            node,
            controller,
            _permit: permit,
            call,
            _doc_id: doc_id,
            terminal: None,
            finished: false,
        }
    }

    pub(crate) async fn finish_success(&mut self, usage: Option<Usage>) {
        self.terminal = Some(PermitTerminal {
            call_state: "completed",
            failure_reason: None,
            usage,
        });
        self.finish().await;
    }

    pub(crate) async fn finish_failure(&mut self, reason: &str) {
        self.terminal = Some(PermitTerminal {
            call_state: "failed",
            failure_reason: Some(reason.to_string()),
            usage: None,
        });
        self.finish().await;
    }

    /// Mark this permit as cancelled due to user-initiated interrupt.
    /// On `finish()` or `Drop`, the controller persists the InferenceCall
    /// with `call_state = "cancelled"` and `failure_reason = "Cancelled"`.
    /// Idempotent with the existing `finished` guard — callers should not
    /// invoke `finish_*` after `mark_interrupted` and instead rely on the
    /// Drop path (or explicit `finish_success`/`finish_failure`) to persist.
    pub(crate) fn mark_interrupted(&mut self) {
        if self.finished {
            return;
        }
        self.terminal = Some(PermitTerminal {
            call_state: "cancelled",
            failure_reason: Some("Cancelled".to_string()),
            usage: None,
        });
        // Intentionally do NOT set `self.finished = true` here. The Drop
        // path is the authority for the actual persist, which keeps the
        // drop-path fallback working if the caller forgets to finish.
    }

    async fn finish(&mut self) {
        if self.finished {
            return;
        }
        self.finished = true;
        let terminal = self.terminal.clone().unwrap_or(PermitTerminal {
            call_state: "completed",
            failure_reason: None,
            usage: None,
        });
        if let Err(error) = persist_existing_call_terminal(
            self.node.clone(),
            &self.call,
            terminal.call_state,
            terminal.failure_reason.as_deref(),
            terminal.usage,
        )
        .await
        {
            tracing::warn!(call_id = %self.call.call_id, error = %error, "failed to persist terminal inference call state");
        }
    }
}

impl StreamGuardLifecycle for AdmissionPermit {
    fn mark_stream_success(&mut self, usage: Option<Usage>) {
        if self.terminal.is_none() {
            self.terminal = Some(PermitTerminal {
                call_state: "completed",
                failure_reason: None,
                usage,
            });
        }
    }

    fn mark_stream_error(&mut self, error: &CompletionError) {
        self.terminal = Some(PermitTerminal {
            call_state: "failed",
            failure_reason: Some(error.to_string()),
            usage: None,
        });
    }
}

impl Drop for AdmissionPermit {
    fn drop(&mut self) {
        self.controller.release_running();
        if self.finished {
            return;
        }
        self.finished = true;
        let terminal = self.terminal.clone().unwrap_or(PermitTerminal {
            call_state: "failed",
            failure_reason: Some("StreamDroppedBeforeTerminalResponse".to_string()),
            usage: None,
        });
        let node = self.node.clone();
        let call_id = self.call.call_id.clone();
        let call = self.call.clone();
        spawn_persistence(async move {
            if let Err(error) = persist_existing_call_terminal(
                node,
                &call,
                terminal.call_state,
                terminal.failure_reason.as_deref(),
                terminal.usage,
            )
            .await
            {
                tracing::warn!(call_id = %call_id, error = %error, "failed to persist dropped inference call state");
            }
        });
    }
}
