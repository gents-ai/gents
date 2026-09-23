//! Compile the actual production helpers while the larger runtime migration is
//! red. These checks do not substitute a Rust simulation for the native owner,
//! and do not claim database/gate/timer conformance.
#[path = "../../gents/src/session/canonical_rows.rs"]
mod canonical_rows;
#[path = "../../gents-loop/src/execution_policy.rs"]
mod execution_policy;
#[path = "../../gents/src/lean_vocab_test/request_execution_lease.rs"]
mod runtime_contract;
#[path = "../../gents/src/lifecycle/terminal_binding.rs"]
mod terminal_binding;

#[test]
fn generated_active_lease_cases_drive_native_admission_and_renewal() {
    use runtime_contract::{
        LeanRequestExecutionAction as Action, LeanRequestExecutionBoundary as Boundary,
        LeanRequestExecutionLeaseStatus as Status,
    };
    // Decode this family's existing strict DTO, not a hand-maintained fixture.
    // Full-snapshot decoder completeness is checked separately by runtime_snapshot.
    let snapshot: serde_json::Value =
        gents_lean_contract::load_contract_snapshot().expect("generate Lean contract");
    let cases: Vec<runtime_contract::LeanRequestExecutionLeaseCase> = serde_json::from_value(
        snapshot
            .get("request_execution_lease_cases")
            .expect("lease group")
            .clone(),
    )
    .expect("strict lease cases");
    let mut exercised = std::collections::BTreeSet::new();
    for case in &cases {
        // Boundary authorization belongs to the native transaction owner, not
        // these pure helpers. Only exercise their explicitly modeled domain.
        if !matches!(case.pre.lease.status, Status::Active) {
            continue;
        }
        let actual_generation = case.pre.lease.generation.unwrap().to_string();
        let observed = execution_policy::LeaseObservation {
            request: case.pre.request,
            generation: &actual_generation,
            deadline_ms: case
                .pre
                .lease
                .explicit_deadline
                .unwrap()
                .try_into()
                .unwrap(),
        };
        let now = case.pre.now.try_into().unwrap();
        let accepted = match &case.action {
            Action::Begin {
                boundary: Boundary::MutationWriteGate,
                generation,
            } => execution_policy::authorize_begin(observed, &generation.to_string(), now),
            Action::AppendOutput {
                boundary: Boundary::MutationWriteGate,
                generation,
            } => execution_policy::authorize_output_append(observed, &generation.to_string(), now),
            Action::AuthorizeProducerDecision {
                boundary: Boundary::MutationWriteGate,
                generation,
                ..
            } => execution_policy::authorize_producer_decision(
                observed,
                &generation.to_string(),
                now,
            ),
            Action::Renew {
                boundary: Boundary::MutationWriteGate,
                generation,
                expected_deadline,
            } => {
                let actual = execution_policy::authorize_renewal(
                    observed,
                    &generation.to_string(),
                    (*expected_deadline).try_into().unwrap(),
                    case.pre.lease.duration.unwrap().try_into().unwrap(),
                    now,
                );
                let expected = case
                    .expected
                    .as_ref()
                    .map(|world| i64::try_from(world.lease.explicit_deadline.unwrap()).unwrap());
                assert_eq!(actual, expected, "{}: renewal deadline", case.name);
                actual.is_some()
            }
            _ => continue,
        };
        assert_eq!(accepted, case.expected.is_some(), "{}", case.name);
        exercised.insert(case.action.kind());
    }
    assert_eq!(
        exercised,
        [
            "begin",
            "append_output",
            "authorize_producer_decision",
            "renew"
        ]
        .into_iter()
        .collect()
    );
}
