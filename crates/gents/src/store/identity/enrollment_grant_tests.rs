use super::*;

fn names(tuples: &[EnrollmentGrantTuple]) -> Vec<&'static str> {
    tuples.iter().map(|tuple| tuple.name()).collect()
}

#[test]
fn derived_tuples_match_lean() {
    let contract =
        &crate::lean_vocab_test::lean_contract_snapshot().identity_enrollment_grant_cases;
    let expected: Vec<&str> = contract["derived"]
        .as_array()
        .unwrap()
        .iter()
        .map(|name| name.as_str().unwrap())
        .collect();
    assert_eq!(names(&EnrollmentGrantTuple::derived()), expected);
    for name in expected {
        assert_eq!(EnrollmentGrantTuple::parse(name).unwrap().name(), name);
    }
}

/// Replays each Lean trace through the production planners. The fixture
/// only applies their decisions to a native-shaped tuple set: grants insert,
/// a revocation removes exactly what `plan_withdrawal` returns.
#[test]
fn grant_and_revocation_planners_replay_lean_traces() {
    let contract =
        &crate::lean_vocab_test::lean_contract_snapshot().identity_enrollment_grant_cases;
    let cases = contract["cases"].as_array().unwrap();
    assert!(!cases.is_empty());
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let mut held: Vec<(String, EnrollmentGrantTuple)> = Vec::new();
        let mut receipts: Vec<EnrollmentGrantReceipt> = Vec::new();
        for (index, step) in case["steps"].as_array().unwrap().iter().enumerate() {
            let action = &step["step"];
            match action["action"].as_str().unwrap() {
                "grant" => {
                    let request = action["request"].as_str().unwrap();
                    let member = action["member"].as_str().unwrap();
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
                    let member = action["member"].as_str().unwrap().to_owned();
                    let tuple =
                        EnrollmentGrantTuple::parse(action["tuple"].as_str().unwrap()).unwrap();
                    if !held.contains(&(member.clone(), tuple)) {
                        held.push((member, tuple));
                    }
                }
                "revoke" => {
                    let request = action["request"].as_str().unwrap();
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
                other => panic!("{name}: unknown action {other}"),
            }
            let expected = &step["expected"];
            for (subject, tuples) in expected["held"].as_object().unwrap() {
                let observed: Vec<&str> = EnrollmentGrantTuple::derived()
                    .into_iter()
                    .filter(|tuple| held.contains(&(subject.clone(), *tuple)))
                    .map(EnrollmentGrantTuple::name)
                    .collect();
                let expected: Vec<&str> = tuples
                    .as_array()
                    .unwrap()
                    .iter()
                    .map(|name| name.as_str().unwrap())
                    .collect();
                assert_eq!(observed, expected, "{name} step {index}: {subject} holds");
            }
            let observed_receipts: Vec<_> = receipts
                .iter()
                .map(|receipt| {
                    json!({
                        "request": receipt.request_id,
                        "member": receipt.member_did,
                        "tuples": names(&receipt.tuples),
                        "withdrawn": receipt.withdrawn,
                    })
                })
                .collect();
            assert_eq!(
                Value::Array(observed_receipts),
                expected["receipts"],
                "{name} step {index}: receipts"
            );
        }
    }
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
