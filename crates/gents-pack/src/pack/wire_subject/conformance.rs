use gents_lean_vocab::families::contracts_pack_installation_wire_plan::WireSubject as Case;
use serde::{Deserialize, Serialize};

use super::{wire_plan_record, WireRecord, WireSubject};
use crate::pack::PackPlugin;

/// The subject a case names: its pack digest and coordinate as written, and
/// a declaration that differs between the case's records.
fn subject(case: &Case) -> WireSubject {
    let (namespace, name) = case
        .coordinate
        .split_once('/')
        .expect("a case names a coordinate");
    WireSubject {
        owner_pack_digest: format!("sha256:{}", case.pack_digest),
        owner_pack_coordinate: case.coordinate.clone(),
        namespace: namespace.to_owned(),
        name: name.to_owned(),
        version: "1.0.0".into(),
        digest: "sha256:artifact".into(),
        declaration: PackPlugin {
            name: name.to_owned(),
            description: case.record.clone(),
            artifact: format!("plugins/{name}.wasm"),
            language: "rust".into(),
            input_schema: serde_json::json!({"type": "object"}),
            ..PackPlugin::default()
        },
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Source {
    SentPack,
    OwnHome,
    Served,
}

#[test]
fn a_wire_plans_from_a_record_as_the_lean_model_says() {
    gents_lean_vocab::families::contracts_pack_installation_wire_plan::replay::<Source>(|input| {
        let approved: Vec<WireSubject> = input.approved.iter().map(subject).collect();
        let named = subject(&input.subject);
        let record = match input.source {
            Source::SentPack => WireRecord::SentPack(named.clone()),
            Source::OwnHome => WireRecord::OwnHome(named.installed()),
            Source::Served => WireRecord::Served(named.installed()),
        };
        let planned = wire_plan_record(&approved, record);
        if let Some(planned) = &planned {
            assert_eq!(WireSubject::of_record(planned), Some(named));
        }
        planned.is_some()
    });
}
