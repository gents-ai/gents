use std::sync::Arc;

use gents_inference::resolved::ResolvedConfiguration;
use gents_loop::loop_stream::{ProviderConfigurationGuard, ProviderConfigurationGuardError};

pub(crate) fn configuration_guard(
    configuration: &ResolvedConfiguration,
) -> Option<ProviderConfigurationGuard> {
    match configuration {
        ResolvedConfiguration::Static => None,
        ResolvedConfiguration::Documents(selection) => {
            let selection = selection.clone();
            Some(Arc::new(move || {
                let selection = selection.clone();
                Box::pin(async move {
                    selection.check().await.map_err(|error| {
                        ProviderConfigurationGuardError::Unavailable(Box::new(error))
                    })
                })
            }))
        }
    }
}
