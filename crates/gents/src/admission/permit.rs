use std::sync::{Arc, Mutex};

use defra_node::EmbeddedNode;
use futures::future::BoxFuture;
use rig::completion::{CompletionError, Usage};
use tokio_util::sync::CancellationToken;

use super::controller::{InferenceCallRecord, PoolPermit};
use super::persistence::{persist_existing_call_terminal, spawn_persistence};
use super::stream_guard::StreamGuardLifecycle;

pub(crate) struct AdmissionPermit {
    node: Arc<EmbeddedNode>,
    permit: Option<PoolPermit>,
    call: InferenceCallRecord,
    _doc_id: String,
    terminal: Option<PermitTerminal>,
    finished: bool,
    cancel_observer: Option<CancellationToken>,
    terminal_failure_observer: Option<Arc<Mutex<Option<String>>>>,
    provider_activity: Option<Arc<gents_loop::provider_activity::ProviderActivity>>,
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
        permit: PoolPermit,
        call: InferenceCallRecord,
        doc_id: String,
        cancel_observer: Option<CancellationToken>,
        terminal_failure_observer: Option<Arc<Mutex<Option<String>>>>,
    ) -> Self {
        Self {
            node,
            permit: Some(permit),
            call,
            _doc_id: doc_id,
            terminal: None,
            finished: false,
            cancel_observer,
            terminal_failure_observer,
            provider_activity: None,
        }
    }

    #[cfg(test)]
    pub(super) fn controller_generation_for_test(&self) -> u64 {
        self.call.controller_generation
    }

    #[cfg(test)]
    pub(super) fn attribution_for_test(&self) -> &str {
        &self.call.backend_config_fingerprint
    }

    /// The armed attempt this call serves; its stall reason names a drop
    /// caused by the owned loop's provider idle window.
    pub(crate) fn observe_provider_activity(
        &mut self,
        activity: Option<Arc<gents_loop::provider_activity::ProviderActivity>>,
    ) {
        self.provider_activity = activity;
    }

    pub(crate) async fn finish_success(
        &mut self,
        usage: Option<Usage>,
    ) -> Result<(), CompletionError> {
        self.terminal = Some(PermitTerminal {
            call_state: "completed",
            failure_reason: None,
            usage,
        });
        self.finish().await
    }

    pub(crate) async fn finish_failure(&mut self, reason: &str) -> Result<(), CompletionError> {
        self.terminal = Some(PermitTerminal {
            call_state: "failed",
            failure_reason: Some(reason.to_string()),
            usage: None,
        });
        self.finish().await
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

    async fn finish(&mut self) -> Result<(), CompletionError> {
        if self.finished {
            return Ok(());
        }
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
            // Charged usage must land durably before the request continues:
            // live ledger charge already happened, and rehydrate reads only
            // InferenceCall rows. Silent warn would mint a fresh allowance on
            // crash redrive.
            if terminal.usage.is_some() {
                return Err(CompletionError::ProviderError(format!(
                    "persisting terminal InferenceCall usage failed for call {}: {error:#}",
                    self.call.call_id
                )));
            }
            tracing::warn!(call_id = %self.call.call_id, error = %error, "failed to persist terminal inference call state");
        }
        self.finished = true;
        Ok(())
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
        if self.terminal.is_none() {
            self.terminal = Some(PermitTerminal {
                call_state: "failed",
                failure_reason: Some(error.to_string()),
                usage: None,
            });
        }
    }

    fn finish_stream(self) -> BoxFuture<'static, Result<(), CompletionError>> {
        Box::pin(async move {
            let mut permit = self;
            permit.finish().await
        })
    }
}

impl Drop for AdmissionPermit {
    fn drop(&mut self) {
        // Field drop runs only after this body, which can block on the
        // observer lock below; return capacity first.
        drop(self.permit.take());
        if self.finished {
            return;
        }
        self.finished = true;
        let terminal_failure_reason = self
            .provider_activity
            .as_ref()
            .and_then(|activity| activity.stall_reason())
            .or_else(|| {
                self.terminal_failure_observer
                    .as_ref()
                    .and_then(|observer| match observer.lock() {
                        Ok(reason) => reason.clone(),
                        Err(poisoned) => poisoned.into_inner().clone(),
                    })
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
        let charged_usage = terminal.usage;
        // Explicit stream finalization awaits charged usage before exposing the
        // provider's terminal item. Drop remains the abort/cancellation repair
        // path and must never block a Tokio runtime thread.
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
                if charged_usage.is_some() {
                    tracing::error!(
                        call_id = %call_id,
                        error = %error,
                        "failed to persist terminal InferenceCall usage on stream drop"
                    );
                } else {
                    tracing::warn!(
                        call_id = %call_id,
                        error = %error,
                        "failed to persist dropped inference call state"
                    );
                }
            }
        });
    }
}
