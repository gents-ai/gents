use gents_loop::parallel_tools::{next_wave_len, Footprint};
use serde::Deserialize;

#[derive(Deserialize)]
struct Call {
    id: usize,
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
}
