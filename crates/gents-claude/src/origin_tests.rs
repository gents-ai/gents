use super::*;
use crate::system::identity_for;

#[test]
fn origin_wire_modes_match_lean() {
    use gents_lean_vocab::families::claude_prompt_assembly_origin as family;
    family::replay::<Cause, Kind>(|i| {
        let origin: Origin = gents_lean_vocab::family::recode(&i.origin);
        let mode = origin.wire_mode();
        family::OriginObservation {
            turn_origin: origin.turn_origin().map(str::to_owned),
            turn_position: origin.turn_position(i.prompts, i.position_lost).map(
                |(prompt_index, turn_index)| family::OriginPosition {
                    prompt_index,
                    turn_index,
                },
            ),
            identity: identity_for(false, mode.session).to_owned(),
            entrypoint: mode.entrypoint.to_owned(),
            background: mode.background,
            workload: mode.workload.map(str::to_owned),
            subagent: mode.subagent,
            request_class: mode.request_class.as_str().to_owned(),
            main_thread_source: mode.main_thread_source,
            agentic: mode.agentic,
        }
    });
}

#[test]
fn the_origin_round_trips_through_its_wire_names() {
    let origin = Origin {
        cause: Cause::PluginJob,
        root_client: true,
        scheduled: false,
        kind: Kind::OneShot,
        appends_system_prompt: true,
    };
    let text = serde_json::to_string(&origin).unwrap();
    assert_eq!(
        text,
        r#"{"cause":"plugin_job","root_client":true,"scheduled":false,"kind":"one_shot","appends_system_prompt":true}"#
    );
    assert_eq!(serde_json::from_str::<Origin>(&text).unwrap(), origin);
}
