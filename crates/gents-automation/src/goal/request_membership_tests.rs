use super::*;

#[tokio::test]
async fn generated_membership_observations_keep_model_roots_and_linear_receipt_work() {
    let cases = &gents_lean_vocab::lean_contract_snapshot().goal_request_head_cases;
    let temp = tempfile::tempdir().unwrap();
    let identities = [
        KeyIdentity::load_or_create(temp.path().join("owner.key"), None).unwrap(),
        KeyIdentity::load_or_create(temp.path().join("foreign.key"), None).unwrap(),
    ];
    let mut profiles = Vec::new();
    for typed in cases {
        let case = head_case(typed);
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
        let current = format!("physical-{}", typed.membership_current);
        let (observed, checks) =
            gents_store::identity::request_receipt::count_request_receipt_verifications(|| {
                authenticated_goal_request_members(owner, &session, &current, &rows)
            });
        if let Some(root) = typed.membership_entry {
            let observed = observed.unwrap_or_else(|error| panic!("{}: {error:#}", case.name));
            assert_eq!(
                observed.entry.doc_id.as_deref(),
                Some(format!("physical-{root}").as_str()),
                "{}: executed model entry",
                case.name,
            );
            let mut expected_members = typed
                .membership_members
                .iter()
                .map(|doc| format!("physical-{doc}"))
                .collect::<Vec<_>>();
            expected_members.sort_unstable();
            assert_eq!(observed.member_doc_ids, expected_members, "{}", case.name);
            let expected_parents = typed
                .membership_parents
                .iter()
                .map(|edge| (format!("physical-{}", edge[0]), format!("physical-{}", edge[1])))
                .collect::<std::collections::BTreeMap<_, _>>();
            let observed_parents = rows
                .iter()
                .filter(|row| {
                    row.doc_id != observed.entry.doc_id
                        && row
                            .doc_id
                            .as_ref()
                            .is_some_and(|doc| observed.member_doc_ids.contains(doc))
                })
                .map(|row| {
                    (
                        row.doc_id.clone().unwrap(),
                        row.caused_by_parent_request_doc_id.clone().unwrap(),
                    )
                })
                .collect::<std::collections::BTreeMap<_, _>>();
            assert_eq!(observed_parents, expected_parents, "{}", case.name);
        } else {
            assert!(
                observed.is_err(),
                "{}: model rejects this observation",
                case.name
            );
        }
        if let Some(budget) = typed.membership_receipt_checks {
            profiles.push((rows.len(), checks, usize::try_from(budget).unwrap()));
        }
    }
    assert!(profiles.iter().any(|(rows, _, _)| *rows >= 128));
    assert!(
        profiles.iter().all(|(_, checks, budget)| checks <= budget),
        "(rows, actual receipt checks, executed one-path budget): {profiles:?}"
    );
}
