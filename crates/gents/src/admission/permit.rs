use std::sync::{Arc, Mutex};

use defra_node::EmbeddedNode;
use rig::completion::{CompletionError, Usage};
use tokio::sync::OwnedSemaphorePermit;
use tokio_util::sync::CancellationToken;

use super::controller::{BackendAdmissionController, InferenceCallRecord};
use super::persistence::{persist_existing_call_terminal, spawn_persistence};
use super::stream_guard::StreamGuardLifecycle;

pub(crate) struct AdmissionPermit {
    node: Arc<EmbeddedNode>,
    controller: Arc<BackendAdmissionController>,
    permit: Option<OwnedSemaphorePermit>,
    call: InferenceCallRecord,
    _doc_id: String,
    terminal: Option<PermitTerminal>,
    finished: bool,
    cancel_observer: Option<CancellationToken>,
    terminal_failure_observer: Option<Arc<Mutex<Option<String>>>>,
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
        cancel_observer: Option<CancellationToken>,
        terminal_failure_observer: Option<Arc<Mutex<Option<String>>>>,
    ) -> Self {
        Self {
            node,
            controller,
            permit: Some(permit),
            call,
            _doc_id: doc_id,
            terminal: None,
            finished: false,
            cancel_observer,
            terminal_failure_observer,
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
        // Return the semaphore permit before the in-flight release: the
        // release can synchronously install a replacement controller, and a
        // drained controller must hold no outstanding permits (#1001; Lean
        // `InferenceCall.ControllerBookkeeping.drained_no_outstanding_permits`).
        // Field drop runs only after this body — including the observer lock
        // below — so the permit must be taken explicitly here.
        drop(self.permit.take());
        self.controller.release_in_flight();
        if self.finished {
            return;
        }
        self.finished = true;
        let terminal_failure_reason =
            self.terminal_failure_observer
                .as_ref()
                .and_then(|observer| match observer.lock() {
                    Ok(reason) => reason.clone(),
                    Err(poisoned) => poisoned.into_inner().clone(),
                });
        let terminal = self.terminal.clone().unwrap_or_else(|| {
            if self
                .cancel_observer
                .as_ref()
                .is_some_and(|t| t.is_cancelled())
            {
                PermitTerminal {
                    call_state: "cancelled",
                    failure_reason: Some("Cancelled".to_string()),
                    usage: None,
                }
            } else if let Some(reason) = terminal_failure_reason {
                PermitTerminal {
                    call_state: "failed",
                    failure_reason: Some(reason),
                    usage: None,
                }
            } else {
                PermitTerminal {
                    call_state: "failed",
                    failure_reason: Some("StreamDroppedBeforeTerminalResponse".to_string()),
                    usage: None,
                }
            }
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
