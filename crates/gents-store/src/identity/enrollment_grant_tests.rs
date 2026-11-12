use super::*;

fn names(tuples: &[EnrollmentGrantTuple]) -> Vec<&'static str> {
    tuples.iter().map(|tuple| tuple.name()).collect()
}

use gents_lean_vocab::families::enrollment_grant_traces_cases as family;

fn observe(
    subjects: &[String],
    held: &[(String, EnrollmentGrantTuple)],
    receipts: &[EnrollmentGrantReceipt],
) -> family::StepObservation {
    family::StepObservation {
        holdings: subjects
            .iter()
            .map(|subject| family::Holding {
                subject: subject.clone(),
                tuples: EnrollmentGrantTuple::derived()
                    .into_iter()
                    .filter(|tuple| held.contains(&(subject.clone(), *tuple)))
                    .map(|tuple| tuple.name().to_owned())
                    .collect(),
            })
            .collect(),
        receipts: receipts
            .iter()
            .map(|receipt| family::ReceiptObservation {
                request: receipt.request_id.clone(),
                member: receipt.member_did.clone(),
                tuples: names(&receipt.tuples)
                    .into_iter()
                    .map(str::to_owned)
                    .collect(),
                withdrawn: receipt.withdrawn,
            })
            .collect(),
    }
}

/// Replays each Lean trace through the production planners. The fixture
/// only applies their decisions to a native-shaped tuple set: grants insert,
/// a revocation removes exactly what `plan_withdrawal` returns.
#[test]
fn grant_and_revocation_planners_replay_lean_traces() {
    family::replay(|input| {
        let derived = EnrollmentGrantTuple::derived();
        for tuple in &derived {
            assert_eq!(EnrollmentGrantTuple::parse(tuple.name()), Some(*tuple));
        }
        let mut held: Vec<(String, EnrollmentGrantTuple)> = Vec::new();
        let mut receipts: Vec<EnrollmentGrantReceipt> = Vec::new();
        let mut steps = Vec::new();
        for step in &input.steps {
            match step.action.as_str() {
                "grant" => {
                    let request = step.request.as_str();
                    let member = step.member.as_str();
                    if !receipts.iter().any(|receipt| receipt.request_id == request) {
                        let member_held: Vec<_> = held
                            .iter()
                            .filter(|(subject, _)| subject == member)
                            .map(|(_, tuple)| *tuple)
                            .collect();
                        let recorded = plan_grant(member, &member_held, &receipts);
                        for tuple in &recorded {
                            if !held.contains(&(member.to_owned(), *tuple)) {
                                held.push((member.to_owned(), *tuple));
                            }
                        }
                        receipts.push(EnrollmentGrantReceipt {
                            doc_id: format!("receipt-{request}"),
                            request_id: request.to_owned(),
                            member_did: member.to_owned(),
                            tuples: recorded,
                            withdrawn: false,
                        });
                    }
                }
                "independent" => {
                    let member = step.member.clone();
                    let tuple = EnrollmentGrantTuple::parse(&step.tuple).unwrap();
                    if !held.contains(&(member.clone(), tuple)) {
                        held.push((member, tuple));
                    }
                }
                "revoke" => {
                    let request = step.request.as_str();
                    let live = receipts
                        .iter()
                        .find(|receipt| receipt.request_id == request && !receipt.withdrawn)
                        .cloned();
                    if let Some(revoked) = live {
                        let withdrawn = plan_withdrawal(&revoked, &receipts);
                        held.retain(|(subject, tuple)| {
                            !(*subject == revoked.member_did && withdrawn.contains(tuple))
                        });
                        for receipt in &mut receipts {
                            if receipt.request_id == request {
                                receipt.withdrawn = true;
                            }
                        }
                    }
                }
                other => panic!("{}: unknown action {other}", input.name),
            }
            steps.push(observe(&input.subjects, &held, &receipts));
        }
        family::TraceOutput {
            derived: names(&derived).into_iter().map(str::to_owned).collect(),
            steps,
        }
    });
}

#[test]
fn withdrawal_plan_retires_only_the_live_receipt_for_its_request() {
    let receipt = |request: &str, withdrawn: bool| EnrollmentGrantReceipt {
        doc_id: format!("doc-{request}"),
        request_id: request.to_owned(),
        member_did: "did:key:z6MkktTX7oEjtsXwBJXEBPnrDCj6J917M1ERALJQriozAes4".to_owned(),
        tuples: EnrollmentGrantTuple::derived(),
        withdrawn,
    };
    assert!(EnrollmentGrantWithdrawal::plan("a", &[receipt("a", true)])
        .unwrap()
        .is_none());
    let planned = EnrollmentGrantWithdrawal::plan("a", &[receipt("a", false), receipt("b", false)])
        .unwrap()
        .unwrap();
    assert_eq!(planned.receipt_doc_id, "doc-a");
    assert!(
        planned.tuples.is_empty(),
        "b still accounts for every tuple"
    );
}

#[test]
fn approval_step_matches_lean() {
    gents_lean_vocab::families::identity_enrollment_grant_after_grant::replay(|i| {
        crate::identity::ApprovalStep::after_grant(i.grant_committed)
    });
}
