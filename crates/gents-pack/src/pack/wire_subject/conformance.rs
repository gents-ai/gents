use gents_lean_vocab::families::contracts_pack_installation_wire_plan::WireSubject as Case;
use serde::{Deserialize, Serialize};

use super::{wire_plan_record, WireRecord, WireSubject};
use crate::pack::PackPlugin;

fn subject(case: &Case) -> WireSubject {
    let (namespace, name) = case
        .plugin
        .split_once('/')
        .expect("a case names a plugin coordinate");
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
        let planned = wire_plan_record(&input.requested, &approved, record);
        if let Some(planned) = &planned {
            assert_eq!(WireSubject::of_record(planned), Some(named));
        }
        planned.is_some()
    });
}

#[test]
fn a_wire_signs_only_the_ids_its_approval_yields_as_the_lean_model_says() {
    gents_lean_vocab::families::contracts_pack_installation_wire_approved_ids_cases::replay(
        |input| super::wire_approved_ids(input.approved.as_deref(), &input.served),
    );
}

#[test]
fn a_wire_signs_only_the_job_its_approval_yields_as_the_lean_model_says() {
    gents_lean_vocab::families::contracts_pack_installation_wire_reviewed_job_cases::replay(
        |input| {
            super::wire_reviewed_job(
                input.approved.as_ref().map(Option::as_ref),
                input.served.as_ref(),
            )
        },
    );
}

macro_rules! approvals {
    ($family:ident, $input:expr) => {
        $input
            .iter()
            .map(
                |approval: &gents_lean_vocab::families::$family::ApprovalInput| {
                    crate::pack::approvals::PackApproval {
                        digest: approval.digest.clone(),
                        coordinate: approval.coordinate.clone(),
                        version: "1.0.0".into(),
                        kind: crate::pack::PackKind::Documents,
                        approved_at: "2026-11-20T00:00:00Z".into(),
                        subjects: Vec::new(),
                        yields: approval.yields.as_ref().map(|ids| {
                            crate::pack::approvals::PackYield {
                                tools: parts_of(ids),
                                agent_targets: parts_of(ids),
                                ..Default::default()
                            }
                        }),
                    }
                },
            )
            .collect::<Vec<_>>()
    };
}

fn parts_of<P>(ids: &[Vec<P>]) -> Vec<Vec<super::IdPart>>
where
    P: AsPart,
{
    ids.iter()
        .map(|id| id.iter().map(AsPart::part).collect())
        .collect()
}

trait AsPart {
    fn part(&self) -> super::IdPart;
}

macro_rules! as_part {
    ($family:ident) => {
        impl AsPart for gents_lean_vocab::families::$family::IdPartInput {
            fn part(&self) -> super::IdPart {
                match self.slot {
                    true => super::IdPart::Slot(self.value.clone()),
                    false => super::IdPart::Text(self.value.clone()),
                }
            }
        }
    };
}

as_part!(contracts_pack_installation_wire_filled_ids_cases);
as_part!(contracts_pack_installation_wire_own_pack_ids_cases);
as_part!(contracts_pack_installation_wire_target_pack_ids_cases);

#[test]
fn a_wire_fills_slots_only_with_the_wired_agent_as_the_lean_model_says() {
    gents_lean_vocab::families::contracts_pack_installation_wire_filled_ids_cases::replay(
        |input| {
            let templates = input.approved.as_deref().map(parts_of);
            let yielded = super::wire_filled_ids(templates.as_deref(), input.agent.as_deref());
            super::wire_approved_ids(yielded.as_deref(), &input.served)
        },
    );
}

#[test]
fn a_wire_reads_its_own_pack_through_the_records_approval_as_the_lean_model_says() {
    gents_lean_vocab::families::contracts_pack_installation_wire_own_pack_ids_cases::replay(
        |input| {
            let approvals = approvals!(
                contracts_pack_installation_wire_own_pack_ids_cases,
                input.approvals
            );
            crate::pack::approvals::own_pack_ids(
                &approvals,
                &input.record_digest,
                "did:key:agent",
                gents_store::collection::Collection::Tools,
                &input.served,
            )
        },
    );
}

#[test]
fn a_wire_reads_a_target_pack_through_its_installed_approval_as_the_lean_model_says() {
    gents_lean_vocab::families::contracts_pack_installation_wire_target_pack_ids_cases::replay(
        |input| {
            let approvals = approvals!(
                contracts_pack_installation_wire_target_pack_ids_cases,
                input.approvals
            );
            crate::pack::approvals::target_pack_ids(
                &approvals,
                &input.coordinate,
                input.installed.as_deref(),
                &input.served,
            )
        },
    );
}
