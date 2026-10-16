use super::super::DesiredStateManifest;

pub(super) fn validate_automation(manifest: &DesiredStateManifest, errors: &mut Vec<String>) {
    for result in manifest
        .tasks
        .iter()
        .map(|task| task.validate())
        .chain(
            manifest
                .schedules
                .iter()
                .map(|schedule| schedule.cadence.validate()),
        )
        .chain(
            manifest
                .event_sources
                .iter()
                .map(|source| source.validate()),
        )
        .chain(
            manifest
                .callback_bindings
                .iter()
                .map(|binding| binding.projected_fields().map(|_| ())),
        )
        .chain(
            manifest
                .repository_placements
                .iter()
                .map(|placement| placement.validate()),
        )
    {
        if let Err(error) = result {
            errors.push(format!("{error:#}"));
        }
    }
}
