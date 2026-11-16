use super::*;

fn physical_doc(doc_id: &str) -> u64 {
    doc_id
        .strip_prefix("physical-")
        .and_then(|doc| doc.parse().ok())
        .unwrap_or_else(|| panic!("{doc_id} is not a fixture document"))
}

#[tokio::test]
async fn generated_membership_observations_keep_model_roots_and_linear_receipt_work() {
    let cases = gents_lean_vocab::families::goal_request_head_contracts_membership::cases::<
        InputDisposition,
    >();
    let temp = tempfile::tempdir().unwrap();
    let identities = [
        KeyIdentity::load_or_create(temp.path().join("owner.key"), None).unwrap(),
        KeyIdentity::load_or_create(temp.path().join("foreign.key"), None).unwrap(),
    ];
    let mut profiles = Vec::new();
    for typed in &cases {
        let case = membership_case(&typed.input);
        let mut fixture = Fixture {
            case: &case,
            identities: &identities,
            built: RapidHashMap::new(),
        };
        let mut rows = Vec::new();
        for row in case.rows.iter().rev() {
            fixture.build(row.clone()).await;
        }
        for row in &case.rows {
            rows.push(fixture.build(row.clone()).await);
        }
        let owner = identities[(case.scope.owner - 1) as usize].did();
        let session = format!("session-{}", case.scope.session);
        let current = format!("physical-{}", typed.input.membership_current);
        let (observed, checks) =
            gents_store::identity::request_receipt::count_request_receipt_verifications(|| {
                authenticated_goal_request_members(owner, &session, &current, &rows)
            });
        let output = match observed {
            Ok(observed) => {
                assert!(
                    observed
                        .member_doc_ids
                        .windows(2)
                        .all(|pair| pair[0] <= pair[1]),
                    "{}: members are reported in document-id order",
                    case.name
                );
                let mut members: Vec<u64> = observed
                    .member_doc_ids
                    .iter()
                    .map(|doc| physical_doc(doc))
                    .collect();
                members.sort_unstable();
                let mut parents: Vec<members::Edge> = rows
                    .iter()
                    .filter(|row| {
                        row.doc_id != observed.entry.doc_id
                            && row
                                .doc_id
                                .as_ref()
                                .is_some_and(|doc| observed.member_doc_ids.contains(doc))
                    })
                    .map(|row| members::Edge {
                        doc: physical_doc(row.doc_id.as_deref().unwrap()),
                        parent: physical_doc(
                            row.caused_by_parent_request_doc_id.as_deref().unwrap(),
                        ),
                    })
                    .collect();
                parents.sort_by_key(|edge| edge.doc);
                members::MembershipOutput {
                    membership_entry: Some(physical_doc(
                        observed.entry.doc_id.as_deref().unwrap_or_else(|| {
                            panic!("{}: an admitted entry has a document", case.name)
                        }),
                    )),
                    membership_members: members,
                    membership_parents: parents,
                }
            }
            Err(_) => members::MembershipOutput {
                membership_entry: None,
                membership_members: Vec::new(),
                membership_parents: Vec::new(),
            },
        };
        typed.check(output);
        if let Some(budget) = case.receipt_checks {
            profiles.push((rows.len(), checks, budget));
        }
    }
    assert!(profiles.iter().any(|(rows, _, _)| *rows >= 128));
    assert!(
        profiles.iter().all(|(_, checks, budget)| checks <= budget),
        "(rows, actual receipt checks, executed one-path budget): {profiles:?}"
    );
}
