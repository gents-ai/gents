use std::fmt;
use std::sync::Arc;

use crate::provider_input::routing_affinity::RoutingAffinity;

pub mod admission;

/// What one provider call carries into the client that makes it. The owner
/// of an execution supplies its capture scope and admission; the loop adds
/// each dispatch's routing affinity. The model built for that call and its
/// transport stack hold it as a field.
#[derive(Clone, Default)]
pub struct ProviderCall {
    pub routing_affinity: Option<Arc<RoutingAffinity>>,
    pub capture: Option<Arc<crate::rendered_request::scope::RequestCaptureScope>>,
    pub admission: Option<admission::AdmissionCallContext>,
}

impl fmt::Debug for ProviderCall {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ProviderCall")
            .field("routing_affinity", &self.routing_affinity.is_some())
            .field("capture", &self.capture.is_some())
            .field("admission", &self.admission.is_some())
            .finish()
    }
}

/// A provider model rebuilt for one call, so that its client and transport
/// stack hold that call's [`ProviderCall`].
pub trait ForCall: Sized {
    fn for_call(&self, call: &ProviderCall) -> Self;
}
