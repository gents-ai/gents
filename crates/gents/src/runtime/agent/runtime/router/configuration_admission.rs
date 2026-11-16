//! Lean `RuntimeState.configurationAdmission` and `RuntimeState.retriesDeferred`:
//! the router reads the routed generation's selected configuration as its
//! Agent before a claim, and defers a request whose selection that generation
//! no longer holds until a newer generation is active.

use gents_inference::resolved::visibility::ConfigurationVisibilityError;
use gents_inference::resolved::ResolvedConfiguration;
use gents_protocol::row::AgentRequest;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub(in crate::runtime::agent) enum ConfigurationAdmission {
    Claim,
    AwaitNewerGeneration,
}

pub(in crate::runtime::agent) fn configuration_admission(
    visible: bool,
    versions_current: bool,
) -> ConfigurationAdmission {
    if visible && versions_current {
        ConfigurationAdmission::Claim
    } else {
        ConfigurationAdmission::AwaitNewerGeneration
    }
}

/// Admission requires current composite versions as well as native Agent
/// visibility. Provider-input assembly keeps its admitted configuration and
/// checks visibility without rebinding running work.
pub(in crate::runtime::agent) async fn routed_configuration_admission(
    configuration: Option<&ResolvedConfiguration>,
) -> Result<ConfigurationAdmission, ConfigurationVisibilityError> {
    let (visible, versions_current) = match configuration {
        Some(ResolvedConfiguration::Documents(selection)) => {
            match selection.check_current().await {
                Ok(current) => (true, current),
                Err(ConfigurationVisibilityError::Unavailable) => (false, false),
                Err(error) => return Err(error),
            }
        }
        Some(ResolvedConfiguration::Static) | None => (true, true),
    };
    Ok(configuration_admission(visible, versions_current))
}

/// Requests left pending with the generation that routed them. The request
/// watcher's cooldown would hold them for its full interval; the router
/// re-routes them as soon as a newer generation is active.
#[derive(Default)]
pub(in crate::runtime::agent) struct DeferredRequests(Vec<(u64, AgentRequest)>);

impl DeferredRequests {
    pub(in crate::runtime::agent) fn defer(&mut self, generation: u64, request: AgentRequest) {
        self.forget(&request.request_id);
        self.0.push((generation, request));
    }

    pub(in crate::runtime::agent) fn forget(&mut self, request_id: &str) {
        self.0
            .retain(|(_, deferred)| deferred.request_id != request_id);
    }

    pub(in crate::runtime::agent) fn release(&mut self, active: u64) -> Option<AgentRequest> {
        let index = self
            .0
            .iter()
            .position(|(deferred_at, _)| retries_deferred(*deferred_at, active))?;
        Some(self.0.remove(index).1)
    }
}

fn retries_deferred(deferred_at: u64, active: u64) -> bool {
    deferred_at < active
}

#[cfg(test)]
mod conformance {
    use super::*;

    #[test]
    fn configuration_admission_matches_lean() {
        gents_lean_vocab::families::runtime_state_configuration_admission::replay(|i| {
            configuration_admission(i.visible, i.versions_current)
        });
    }
}
