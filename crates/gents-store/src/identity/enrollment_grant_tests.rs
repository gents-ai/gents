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
                owed: receipt.owed,
                restore: receipt.restore,
            })
            .collect(),
    }
}

fn owe(
    member: &str,
    retired: &[String],
    removed: &[EnrollmentGrantTuple],
    held: &mut Vec<(String, EnrollmentGrantTuple)>,
    receipts: &mut [EnrollmentGrantReceipt],
) {
    let retiring: Vec<_> = receipts
        .iter()
        .filter(|receipt| retired.contains(&receipt.doc_id))
        .cloned()
        .collect();
    let owing = plan_owing(member, &retiring.iter().collect::<Vec<_>>(), removed);
    held.retain(|(subject, tuple)| {
        !(subject == member
            && *tuple != EnrollmentGrantTuple::NodeAdministrator
            && removed.contains(tuple))
    });
    for receipt in receipts {
        if retired.contains(&receipt.doc_id) {
            receipt.withdrawn = true;
            receipt.owed |= owing.contains(&receipt.doc_id);
        }
    }
}

fn take_over(member: &str, receipts: &mut [EnrollmentGrantReceipt]) {
    for receipt in receipts {
        if receipt.member_did == member && receipt.owed {
            receipt.owed = false;
            receipt.restore = true;
        }
    }
}

fn restore(
    member: &str,
    held: &mut Vec<(String, EnrollmentGrantTuple)>,
    receipts: &mut [EnrollmentGrantReceipt],
) {
    if !receipts
        .iter()
        .any(|receipt| receipt.restore && receipt.member_did == member)
    {
        return;
    }
    let holds = held.contains(&(member.to_owned(), EnrollmentGrantTuple::NodeAdministrator));
    if restoration_step(administration_owed(member, receipts), holds) == RestorationStep::Restore {
        held.push((member.to_owned(), EnrollmentGrantTuple::NodeAdministrator));
    }
    for receipt in receipts {
        if receipt.member_did == member {
            receipt.restore = false;
        }
    }
}

fn settle(
    member: &str,
    held: &mut Vec<(String, EnrollmentGrantTuple)>,
    receipts: &mut [EnrollmentGrantReceipt],
) {
    let owed = administration_owed(member, receipts);
    let taken_over = receipts
        .iter()
        .any(|receipt| receipt.accounts(member, EnrollmentGrantTuple::NodeAdministrator));
    match settlement_step(owed, taken_over) {
        SettlementStep::Leave => return,
        SettlementStep::Close => {}
        SettlementStep::Issue => {
            held.retain(|(subject, tuple)| {
                !(subject == member && *tuple == EnrollmentGrantTuple::NodeAdministrator)
            });
            if after_removal(taken_over) == AfterRemoval::Reissue {
                held.push((member.to_owned(), EnrollmentGrantTuple::NodeAdministrator));
            }
        }
    }
    for receipt in receipts {
        if receipt.member_did == member {
            receipt.owed = false;
        }
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
                "grant" | "grant_interrupted" => {
                    let request = step.request.as_str();
                    let member = step.member.as_str();
                    let issued = if step.action == "grant" {
                        usize::MAX
                    } else {
                        usize::try_from(step.issued).unwrap()
                    };
                    let existing = receipts
                        .iter()
                        .find(|receipt| receipt.request_id == request)
                        .cloned();
                    let to_issue = match &existing {
                        Some(receipt) if receipt.withdrawn => Vec::new(),
                        Some(receipt) => receipt.tuples.clone(),
                        None => {
                            let member_held: Vec<_> = held
                                .iter()
                                .filter(|(subject, _)| subject == member)
                                .map(|(_, tuple)| *tuple)
                                .collect();
                            let recorded = plan_grant(member, &member_held, &receipts);
                            take_over(member, &mut receipts);
                            receipts.push(EnrollmentGrantReceipt {
                                doc_id: format!("receipt-{request}"),
                                request_id: request.to_owned(),
                                member_did: member.to_owned(),
                                tuples: recorded.clone(),
                                withdrawn: false,
                                owed: false,
                                restore: false,
                            });
                            recorded
                        }
                    };
                    let new: Vec<_> = to_issue
                        .into_iter()
                        .filter(|tuple| !held.contains(&(member.to_owned(), *tuple)))
                        .take(issued)
                        .collect();
                    for tuple in new {
                        held.push((member.to_owned(), tuple));
                    }
                }
                "abandon" => {
                    let request = step.request.as_str();
                    let live = receipts
                        .iter()
                        .find(|receipt| receipt.request_id == request && !receipt.withdrawn)
                        .cloned();
                    if let Some(abandoned) = live {
                        let removed = plan_withdrawal(&abandoned, &receipts);
                        held.retain(|(subject, tuple)| {
                            !(*subject == abandoned.member_did && removed.contains(tuple))
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
                "revoke_member" => {
                    let member = step.member.as_str();
                    let withdrawn = plan_membership_withdrawal(member, &receipts);
                    held.retain(|(subject, tuple)| {
                        !(subject == member && withdrawn.contains(tuple))
                    });
                    for receipt in &mut receipts {
                        if receipt.member_did == member {
                            receipt.withdrawn = true;
                        }
                    }
                }
                "owe_revoke" => {
                    let live = receipts
                        .iter()
                        .find(|receipt| receipt.request_id == step.request && !receipt.withdrawn)
                        .cloned();
                    if let Some(revoked) = live {
                        let removed = plan_withdrawal(&revoked, &receipts);
                        let member = revoked.member_did.clone();
                        owe(
                            &member,
                            &[revoked.doc_id],
                            &removed,
                            &mut held,
                            &mut receipts,
                        );
                    }
                }
                "owe_revoke_member" => {
                    let member = step.member.as_str();
                    let removed = plan_membership_withdrawal(member, &receipts);
                    let retired: Vec<_> = receipts
                        .iter()
                        .filter(|receipt| receipt.member_did == member)
                        .map(|receipt| receipt.doc_id.clone())
                        .collect();
                    owe(member, &retired, &removed, &mut held, &mut receipts);
                }
                "settle" => settle(&step.member, &mut held, &mut receipts),
                "grant_other" => {
                    let member = step.member.clone();
                    let tuple = EnrollmentGrantTuple::parse(&step.tuple).unwrap();
                    let owed = administration_owed(&member, &receipts);
                    let admitted = match tuple {
                        EnrollmentGrantTuple::Permission(_) => permission_grant_admitted(owed),
                        EnrollmentGrantTuple::NodeAdministrator => {
                            take_over(&member, &mut receipts);
                            true
                        }
                    };
                    if admitted && !held.contains(&(member.clone(), tuple)) {
                        held.push((member, tuple));
                    }
                }
                "expire" => {
                    let member = step.member.as_str();
                    let removed = plan_membership_withdrawal(member, &receipts);
                    let retired: Vec<_> = receipts
                        .iter()
                        .filter(|receipt| receipt.member_did == member)
                        .map(|receipt| receipt.doc_id.clone())
                        .collect();
                    owe(member, &retired, &removed, &mut held, &mut receipts);
                }
                "settle_by_node" => {}
                "overtaking_removal" => held.retain(|(subject, tuple)| {
                    !(*subject == step.member && *tuple == EnrollmentGrantTuple::NodeAdministrator)
                }),
                "restore" => restore(&step.member, &mut held, &mut receipts),
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
        owed: false,
        restore: false,
    };
    assert!(EnrollmentGrantWithdrawal::plan("a", &[receipt("a", true)])
        .unwrap()
        .is_none());
    let planned = EnrollmentGrantWithdrawal::plan("a", &[receipt("a", false), receipt("b", false)])
        .unwrap()
        .unwrap();
    assert_eq!(planned.receipt_doc_ids, ["doc-a"]);
    assert!(
        planned.tuples.is_empty(),
        "b still accounts for every tuple"
    );
}

#[test]
fn settlement_steps_match_lean() {
    gents_lean_vocab::families::identity_enrollment_grant_settlement_step::replay(|i| {
        settlement_step(i.owed, i.taken_over)
    });
    gents_lean_vocab::families::identity_enrollment_grant_restoration_step::replay(|i| {
        restoration_step(i.owed, i.held)
    });
    gents_lean_vocab::families::identity_enrollment_grant_after_removal::replay(|i| {
        after_removal(i.taken_over)
    });
}

#[test]
fn the_permission_rule_and_lapse_match_lean() {
    gents_lean_vocab::families::identity_enrollment_grant_permission_grant_admitted::replay(|i| {
        permission_grant_admitted(i.owed)
    });
    gents_lean_vocab::families::enrollment_grant_traces_lapse_cases::replay(|i| {
        membership_lapsed(i.provider, i.wall, &i.leases)
    });
}

#[test]
fn approval_step_matches_lean() {
    gents_lean_vocab::families::identity_enrollment_grant_after_grant::replay(|i| {
        crate::identity::ApprovalStep::after_grant(i.grant_committed)
    });
}

/// A member's tuples in memory, refusing the issue at `fail_at` and, when
/// `removals_fail`, every removal.
struct InMemoryTuples {
    held: std::sync::Mutex<Vec<EnrollmentGrantTuple>>,
    issues: std::sync::atomic::AtomicUsize,
    fail_at: Option<usize>,
    removals_fail: bool,
}

impl InMemoryTuples {
    fn holding(held: &[EnrollmentGrantTuple], fail_at: Option<usize>, removals_fail: bool) -> Self {
        Self {
            held: std::sync::Mutex::new(held.to_vec()),
            issues: std::sync::atomic::AtomicUsize::new(0),
            fail_at,
            removals_fail,
        }
    }

    fn held(&self) -> Vec<EnrollmentGrantTuple> {
        let held = self.held.lock().unwrap();
        EnrollmentGrantTuple::derived()
            .into_iter()
            .filter(|tuple| held.contains(tuple))
            .collect()
    }
}

impl TupleChanges for InMemoryTuples {
    async fn issue(&self, tuple: EnrollmentGrantTuple) -> Result<(), ConfigClientError> {
        let call = self
            .issues
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        if self.fail_at == Some(call) {
            return Err(ConfigClientError::RelationshipActorRequired);
        }
        let mut held = self.held.lock().unwrap();
        if !held.contains(&tuple) {
            held.push(tuple);
        }
        Ok(())
    }

    async fn remove(&self, tuple: EnrollmentGrantTuple) -> Result<(), ConfigClientError> {
        if self.removals_fail {
            return Err(ConfigClientError::RelationshipActorRequired);
        }
        self.held.lock().unwrap().retain(|held| *held != tuple);
        Ok(())
    }
}

/// A grant failing at each of its tuples removes everything it alone
/// accounts for, keeps another enrollment's tuples, and a retry then holds
/// every derived tuple (Lean `abandon`, `retried_approval_after_abandon_is_a_clean_approval`).
#[tokio::test]
async fn a_grant_failing_at_any_tuple_leaves_none_of_its_authority() {
    let member = "did:key:z6MkktTX7oEjtsXwBJXEBPnrDCj6J917M1ERALJQriozAes4";
    let derived = EnrollmentGrantTuple::derived();
    let other = EnrollmentGrantReceipt {
        doc_id: "doc-other".into(),
        request_id: "other".into(),
        member_did: member.into(),
        tuples: derived[..2].to_vec(),
        withdrawn: false,
        owed: false,
        restore: false,
    };
    let this = EnrollmentGrantReceipt {
        doc_id: "doc-this".into(),
        request_id: "this".into(),
        member_did: member.into(),
        tuples: derived.clone(),
        withdrawn: false,
        owed: false,
        restore: false,
    };
    let receipts = [other.clone(), this.clone()];
    for fail_at in 0..derived.len() {
        let tuples = InMemoryTuples::holding(&other.tuples, Some(fail_at), false);
        let failed = issue_or_abandon(&tuples, &this, &receipts).await;
        assert!(
            matches!(failed, Err(EnrollmentGrantError::Issue { tuple, .. }) if tuple == derived[fail_at].name()),
            "{fail_at}: {failed:?}"
        );
        assert_eq!(tuples.held(), other.tuples, "{fail_at}");
        let retried = InMemoryTuples::holding(&tuples.held(), None, false);
        issue_or_abandon(&retried, &this, &receipts).await.unwrap();
        assert_eq!(retried.held(), derived, "{fail_at}");
    }
}

/// When removing after a failed issue fails too, the error says so and the
/// receipt still accounts for what remains: a retried grant reissues every
/// tuple, and the revocation's withdrawal plan removes them.
#[tokio::test]
async fn a_grant_whose_abandon_fails_is_repaired_by_a_retry_or_a_revocation() {
    let member = "did:key:z6MkktTX7oEjtsXwBJXEBPnrDCj6J917M1ERALJQriozAes4";
    let derived = EnrollmentGrantTuple::derived();
    let this = EnrollmentGrantReceipt {
        doc_id: "doc-this".into(),
        request_id: "this".into(),
        member_did: member.into(),
        tuples: derived.clone(),
        withdrawn: false,
        owed: false,
        restore: false,
    };
    let receipts = [this.clone()];
    let tuples = InMemoryTuples::holding(&[], Some(1), true);
    let failed = issue_or_abandon(&tuples, &this, &receipts).await;
    assert!(
        matches!(
            failed,
            Err(EnrollmentGrantError::IssueNotAbandoned { tuple, removing, .. })
                if tuple == derived[1].name() && removing == derived[0].name()
        ),
        "{failed:?}"
    );
    assert_eq!(tuples.held(), derived[..1]);
    let retried = InMemoryTuples::holding(&tuples.held(), None, false);
    issue_or_abandon(&retried, &this, &receipts).await.unwrap();
    assert_eq!(retried.held(), derived);
    let revoked = InMemoryTuples::holding(&tuples.held(), None, false);
    for tuple in plan_withdrawal(&this, &receipts) {
        revoked.remove(tuple).await.unwrap();
    }
    assert!(revoked.held().is_empty());
}
