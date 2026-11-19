use std::future::Future;
use std::sync::Arc;

use super::controller::PendingCallMetadata;
use super::permit::AdmissionPermit;
use super::AdmissionRegistry;
use gents_loop::provider_call::admission::AdmissionCallContext;
use gents_loop::provider_call::{ForCall, ProviderCall};

const CANCELLED_BY_INTERRUPT_MSG: &str = "inference cancelled by request interrupt";

/// Admission refused, cancelled, or could not record a provider call.
#[derive(Clone, Debug, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct AdmissionError(pub(crate) String);

/// A provider call that did not complete under admission.
#[derive(Debug)]
pub(crate) enum AdmittedCallError<E> {
    Admission(AdmissionError),
    Provider(E),
}

/// A provider model admitted through the backend it was built for.
/// `connection` attributes calls only; after a connection change, calls
/// through this model still share the backend's pool and finish on that
/// connection.
#[derive(Clone)]
pub struct AdmittedCompletionModel<M> {
    pub(crate) inner: M,
    admission: AdmissionRegistry,
    connection: Arc<str>,
    pub(crate) model_name: Option<String>,
    pub(crate) profile_id: Option<String>,
    pub(crate) separate_reasoning: bool,
    pub agent_signer: Option<gents_store::identity::ManagedAgentSigner>,
    /// The call this model was built for.
    call: ProviderCall,
}

impl<M> ForCall for AdmittedCompletionModel<M>
where
    M: ForCall,
{
    fn for_call(&self, call: &ProviderCall) -> Self {
        Self {
            inner: self.inner.for_call(call),
            admission: self.admission.clone(),
            connection: self.connection.clone(),
            model_name: self.model_name.clone(),
            profile_id: self.profile_id.clone(),
            separate_reasoning: self.separate_reasoning,
            agent_signer: self.agent_signer.clone(),
            call: call.clone(),
        }
    }
}

impl<M> AdmittedCompletionModel<M> {
    pub fn new(inner: M, admission: AdmissionRegistry, connection: impl Into<Arc<str>>) -> Self {
        Self {
            inner,
            admission,
            connection: connection.into(),
            model_name: None,
            profile_id: None,
            separate_reasoning: false,
            agent_signer: None,
            call: ProviderCall::default(),
        }
    }

    /// Run one provider call under an admission permit, racing the request's
    /// interrupt token. On success the caller owns the permit's terminal.
    pub(crate) async fn admit<T, E>(
        &self,
        observe_provider_activity: bool,
        call: impl Future<Output = Result<T, E>>,
    ) -> Result<(T, AdmissionPermit), AdmittedCallError<E>>
    where
        E: std::fmt::Display,
    {
        let mut permit = self
            .admission
            .acquire_call_with_signer(
                self.call.admission.as_ref(),
                &self.connection,
                self.model_name.as_deref(),
                self.profile_id.as_deref(),
                self.agent_signer.clone(),
            )
            .await
            .map_err(AdmittedCallError::Admission)?;
        if observe_provider_activity {
            permit.observe_provider_activity(
                crate::rendered_request::scope::current_attempt_activity(
                    self.call.capture.as_ref(),
                ),
            );
        }
        let result = match self
            .call
            .admission
            .as_ref()
            .and_then(|context| context.inference_token().cloned())
        {
            Some(token) => {
                tokio::select! {
                    biased;
                    _ = token.cancelled() => {
                        permit.mark_interrupted();
                        return Err(AdmittedCallError::Admission(AdmissionError(
                            CANCELLED_BY_INTERRUPT_MSG.into(),
                        )));
                    }
                    result = call => result,
                }
            }
            None => call.await,
        };
        match result {
            Ok(value) => Ok((value, permit)),
            Err(error) => {
                let _ = permit.finish_failure(&error.to_string()).await;
                Err(AdmittedCallError::Provider(error))
            }
        }
    }
}

/// The pending call admission records for the next call under `context`.
pub(super) fn next_call(
    context: &AdmissionCallContext,
    runtime_instance_id: &str,
    model_name: Option<&str>,
    profile_id: Option<&str>,
    agent_signer: Option<gents_store::identity::ManagedAgentSigner>,
) -> PendingCallMetadata {
    let (call_seq, join) = context.mint(uuid::Uuid::new_v4().to_string());
    PendingCallMetadata {
        agent_signer,
        call_id: join.call_id,
        runtime_instance_id: runtime_instance_id.to_string(),
        request_id: context.request_id().to_owned(),
        request_doc_id: context.request_doc_id().to_owned(),
        request_purpose: context.request_purpose(),
        session_id: context.session_id().to_owned(),
        requester_did: context.requester_did().map(str::to_owned),
        model_name: model_name.map(str::to_owned),
        profile_id: profile_id.map(str::to_owned),
        call_seq,
        backend_id: context.backend_id().to_owned(),
        agent_did: context.agent_did().to_owned(),
        node_did: context.node_did().to_owned(),
        call_kind: context.call_kind(),
        attempt: context.attempt(),
    }
}
