use gents_loop::parallel_tools::{
    executable_wave_len, next_wave_len, speculative_footprint, Footprint,
};
use serde::Deserialize;

#[derive(Deserialize)]
struct Call {
    id: usize,
    key: String,
    footprint: Footprint,
}

#[derive(Deserialize)]
struct Wave {
    name: String,
    calls: Vec<Call>,
    wave: Vec<usize>,
    remaining: Vec<usize>,
}

#[derive(Deserialize)]
struct Cases {
    waves: Vec<Wave>,
    hook_footprints: Vec<HookFootprint>,
    limited: Limited,
    repeated: Limited,
}

#[derive(Deserialize)]
struct HookFootprint {
    allowed: bool,
    input: Footprint,
    expected: Footprint,
}

#[derive(Deserialize)]
struct Limited {
    limit: usize,
    calls: Vec<Call>,
    wave: Vec<usize>,
}

#[test]
fn generated_compatible_waves_match_the_scheduler() {
    let json = gents_lean_contract::run_prepared_generator(
        &gents_lean_contract::proofs_dir().unwrap(),
        "Proofs.Conformance.ParallelTools:olean",
        "Proofs/Conformance/ParallelTools.lean",
    )
    .unwrap();
    let cases: Cases = serde_json::from_str(&json).unwrap();
    for case in cases.hook_footprints {
        assert_eq!(
            speculative_footprint(case.allowed, case.input),
            case.expected
        );
    }
    for case in cases.waves {
        let footprints: Vec<_> = case
            .calls
            .iter()
            .map(|call| call.footprint.clone())
            .collect();
        let count = next_wave_len(&footprints);
        let ids: Vec<_> = case.calls.iter().map(|call| call.id).collect();
        assert_eq!(&ids[..count], case.wave, "{}", case.name);
        assert_eq!(&ids[count..], case.remaining, "{}", case.name);
    }
    for case in [cases.limited, cases.repeated] {
        let planned: Vec<_> = case
            .calls
            .iter()
            .map(|call| ((call.key.clone(), String::new()), call.footprint.clone()))
            .collect();
        let count = executable_wave_len(&planned, case.limit);
        let ids: Vec<_> = case.calls.iter().map(|call| call.id).take(count).collect();
        assert_eq!(ids, case.wave);
    }
}
