use gents_protocol::error_chain::ErrorChain;

use super::super::DesiredStateManifest;

pub(super) fn validate_automation(manifest: &DesiredStateManifest, errors: &mut Vec<String>) {
    for result in manifest
        .tasks
        .iter()
        .map(|task| {
            task.validate()
                .map_err(|error| ErrorChain(&error).to_string())
        })
        .chain(manifest.schedules.iter().map(|schedule| {
            schedule
                .cadence
                .validate()
                .map_err(|error| format!("{}", ErrorChain(&error)))
        }))
        .chain(manifest.event_sources.iter().map(|source| {
            source
                .validate()
                .map_err(|error| ErrorChain(&error).to_string())
        }))
        .chain(manifest.callback_bindings.iter().map(|binding| {
            binding
                .projected_fields()
                .map(|_| ())
                .map_err(|error| format!("{}", ErrorChain(&error)))
        }))
        .chain(manifest.repository_placements.iter().map(|placement| {
            placement
                .validate()
                .map_err(|error| ErrorChain(&error).to_string())
        }))
    {
        if let Err(error) = result {
            errors.push(error);
        }
    }
}
