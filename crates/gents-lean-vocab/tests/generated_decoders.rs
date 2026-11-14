use std::path::Path;

#[allow(dead_code)]
#[path = "../examples/decoders_from_snapshot/generator.rs"]
mod generator;

#[test]
fn generated_decoders_are_current() {
    let json = gents_lean_contract::load_contract_json().expect("Lean conformance snapshot");
    let snapshot = serde_json::from_str(&json).expect("snapshot JSON");
    let src = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/src"));
    let output = generator::generate(
        &snapshot,
        &generator::decoder_sources(src),
        None,
        &Default::default(),
    );
    let checked_in = std::fs::read_to_string(src.join(generator::GENERATED)).unwrap();
    assert!(
        output.generated == checked_in && output.removals.is_empty(),
        "the Lean snapshot and src/generated.rs disagree: {:#}\nregenerate with \
         make lean-regen",
        serde_json::json!({
            "patches": output.report["patches"],
            "mismatches": output.report["mismatches"],
            "hand_written_decoders": output.removals,
        })
    );
}
