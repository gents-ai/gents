use std::sync::{Arc, Mutex};

use defra_node::EmbeddedNode;
use futures::future::BoxFuture;
use gents_loop::rig_compat::CachedInputTokensObservation;
use tokio_util::sync::CancellationToken;

use super::client::AdmissionError;
use super::controller::{InferenceCallRecord, PoolPermit};
use super::persistence::{persist_existing_call_terminal, ProviderCallUsage};
use super::stream_guard::StreamGuardLifecycle;
use crate::drop_cleanup::DropCleanup;

pub(crate) struct AdmissionPermit {
    node: Arc<EmbeddedNode>,
    permit: Option<PoolPermit>,
    call: InferenceCallRecord,
    doc_id: String,
    terminal: Option<PermitTerminal>,
    finished: bool,
    cancel_observer: Option<CancellationToken>,
    terminal_failure_observer: Option<Arc<Mutex<Option<String>>>>,
    provider_activity: Option<Arc<gents_loop::provider_activity::ProviderActivity>>,
    runtime: DropCleanup,
}

#[derive(Clone, Debug)]
struct PermitTerminal {
    call_state: &'static str,
    failure_reason: Option<String>,
    usage: Option<ProviderCallUsage>,
    cached_input_tokens: CachedInputTokensObservation,
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
            doc_id,
            terminal: None,
            finished: false,
            cancel_observer,
            terminal_failure_observer,
            provider_activity: None,
            runtime: DropCleanup::current(),
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

    pub(crate) fn observe_provider_activity(
        &mut self,
        activity: Option<Arc<gents_loop::provider_activity::ProviderActivity>>,
    ) {
        self.provider_activity = activity;
    }

    #[cfg(test)]
    pub(crate) async fn finish_success(
        &mut self,
        usage: Option<ProviderCallUsage>,
    ) -> Result<(), AdmissionError> {
        self.finish_success_with_cache(usage, CachedInputTokensObservation::NotAvailable)
            .await
    }

    pub(crate) async fn finish_success_with_cache(
        &mut self,
        usage: Option<ProviderCallUsage>,
        cached_input_tokens: CachedInputTokensObservation,
    ) -> Result<(), AdmissionError> {
        self.terminal = Some(PermitTerminal {
            call_state: "completed",
            failure_reason: None,
            usage,
            cached_input_tokens,
        });
        self.finish().await
    }

    pub(crate) async fn finish_failure(&mut self, reason: &str) -> Result<(), AdmissionError> {
        self.terminal = Some(PermitTerminal {
            call_state: "failed",
            failure_reason: Some(recorded_failure_reason(reason)),
            usage: None,
            cached_input_tokens: CachedInputTokensObservation::NotAvailable,
        });
        self.finish().await
    }

    pub(crate) fn mark_interrupted(&mut self) {
        if self.finished {
            return;
        }
        self.terminal = Some(PermitTerminal {
            call_state: "cancelled",
            failure_reason: Some("Cancelled".to_string()),
            usage: None,
            cached_input_tokens: CachedInputTokensObservation::NotAvailable,
        });
    }

    async fn finish(&mut self) -> Result<(), AdmissionError> {
        if self.finished {
            return Ok(());
        }
        let terminal = self.terminal.clone().unwrap_or(PermitTerminal {
            call_state: "completed",
            failure_reason: None,
            usage: None,
            cached_input_tokens: CachedInputTokensObservation::NotAvailable,
        });
        if let Err(error) = persist_existing_call_terminal(
            self.node.clone(),
            &self.call,
            &self.doc_id,
            terminal.call_state,
            terminal.failure_reason.as_deref(),
            terminal.usage,
            terminal.cached_input_tokens,
        )
        .await
        {
            // Charged usage must land durably before the request continues;
            // rehydrate reads only InferenceCall rows.
            if terminal.usage.is_some() {
                return Err(AdmissionError(format!(
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
    fn mark_stream_success(
        &mut self,
        usage: Option<ProviderCallUsage>,
        cached_input_tokens: CachedInputTokensObservation,
    ) {
        if self.terminal.is_none() {
            self.terminal = Some(PermitTerminal {
                call_state: "completed",
                failure_reason: None,
                usage,
                cached_input_tokens,
            });
        }
    }

    fn mark_stream_error(&mut self, error: &str) {
        if self.terminal.is_none() {
            self.terminal = Some(PermitTerminal {
                call_state: "failed",
                failure_reason: Some(recorded_failure_reason(error)),
                usage: None,
                cached_input_tokens: CachedInputTokensObservation::NotAvailable,
            });
        }
    }

    fn finish_stream(self) -> BoxFuture<'static, Result<(), AdmissionError>> {
        Box::pin(async move {
            let mut permit = self;
            permit.finish().await
        })
    }
}

impl Drop for AdmissionPermit {
    fn drop(&mut self) {
        // Return capacity before field drop, which can block on the observer lock.
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
                    cached_input_tokens: CachedInputTokensObservation::NotAvailable,
                }
            } else if let Some(reason) = terminal_failure_reason {
                PermitTerminal {
                    call_state: "failed",
                    failure_reason: Some(recorded_failure_reason(&reason)),
                    usage: None,
                    cached_input_tokens: CachedInputTokensObservation::NotAvailable,
                }
            } else {
                PermitTerminal {
                    call_state: "failed",
                    failure_reason: Some("StreamDroppedBeforeTerminalResponse".to_string()),
                    usage: None,
                    cached_input_tokens: CachedInputTokensObservation::NotAvailable,
                }
            }
        });
        let node = self.node.clone();
        let call_id = self.call.call_id.clone();
        let call = self.call.clone();
        let doc_id = self.doc_id.clone();
        let charged_usage = terminal.usage;
        // Drop must never block a Tokio runtime thread.
        self.runtime.spawn(async move {
            if let Err(error) = persist_existing_call_terminal(
                node,
                &call,
                &doc_id,
                terminal.call_state,
                terminal.failure_reason.as_deref(),
                terminal.usage,
                terminal.cached_input_tokens,
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

/// InferenceCall rows replicate like the request's own reason, so their text
/// goes through the same redaction and bound.
fn recorded_failure_reason(reason: &str) -> String {
    crate::failure_text::bounded_text(&gents_loop::provider_limit::persisted_failure_reason(
        reason,
        chrono::Utc::now(),
    ))
}
