use super::*;
use gents_lean_vocab::{
    LeanPluginApprovalCase, LeanPluginApprovalCaseInitial, LeanPluginApprovalCaseScoped,
    LeanPluginApprovalCaseScopedDeliveriesItem, LeanPluginApprovalCaseStepsItem,
    LeanPluginApprovalCaseStepsItemExpected,
};

#[path = "approval/scoped_contract_cases.rs"]
mod scoped;

#[derive(Debug)]
struct Case {
    directory: bool,
    write: bool,
    initial: State,
    steps: Vec<Step>,
    scoped: Option<scoped::Case>,
}

#[derive(Debug)]
struct State {
    present: bool,
    decision: Option<bool>,
    path_grant: Option<bool>,
    folder_grant: Option<bool>,
}

#[derive(Debug)]
enum Step {
    Decide {
        id: String,
        now: u64,
        answer: String,
        accepted: bool,
        expected: State,
    },
    Cancel {
        accepted: bool,
        expected: State,
    },
}

fn answer(value: &str) -> Answer {
    match value {
        "once" => Answer::Once,
        "file" => Answer::AlwaysPath,
        "always" => Answer::AlwaysFolder,
        "deny" => Answer::Deny,
        other => panic!("unknown modeled answer {other}"),
    }
}

fn access(write: bool) -> BindAccess {
    if write {
        BindAccess::ReadWrite
    } else {
        BindAccess::Read
    }
}

fn initial_of(initial: &LeanPluginApprovalCaseInitial) -> State {
    let LeanPluginApprovalCaseInitial {
        decision,
        folder_grant,
        path_grant,
        present,
    } = initial;
    State {
        present: *present,
        decision: decision.as_ref().map(|value| value.as_bool().unwrap()),
        path_grant: *path_grant,
        folder_grant: *folder_grant,
    }
}

fn state_of(state: &LeanPluginApprovalCaseStepsItemExpected) -> State {
    let LeanPluginApprovalCaseStepsItemExpected {
        decision,
        folder_grant,
        path_grant,
        present,
    } = state;
    State {
        present: *present,
        decision: *decision,
        path_grant: *path_grant,
        folder_grant: *folder_grant,
    }
}

fn step_of(step: &LeanPluginApprovalCaseStepsItem) -> Step {
    let LeanPluginApprovalCaseStepsItem {
        accepted,
        answer,
        expected,
        id,
        kind,
        now,
    } = step;
    let accepted = *accepted;
    let expected = state_of(expected);
    match kind.as_str() {
        "decide" => Step::Decide {
            id: id.clone().unwrap(),
            now: now.unwrap(),
            answer: answer.clone().unwrap(),
            accepted,
            expected,
        },
        "cancel" => Step::Cancel { accepted, expected },
        other => panic!("unknown modeled step kind {other}"),
    }
}

fn scope_json(workspace: u64, revision: u64) -> serde_json::Value {
    serde_json::json!({ "workspace": workspace, "revision": revision })
}

fn grant_json(action: u64, workspace: u64, revision: u64) -> serde_json::Value {
    serde_json::json!({ "action": action, "scope": scope_json(workspace, revision) })
}

fn delivery_json(delivery: &LeanPluginApprovalCaseScopedDeliveriesItem) -> serde_json::Value {
    let LeanPluginApprovalCaseScopedDeliveriesItem {
        available,
        expected,
        received,
    } = delivery;
    let available: Vec<serde_json::Value> = available
        .iter()
        .map(|scope| scope_json(scope.workspace, scope.revision))
        .collect();
    serde_json::json!([
        available,
        grant_json(
            received.action,
            received.scope.workspace,
            received.scope.revision
        ),
        expected,
    ])
}

fn scoped_of(scoped: &LeanPluginApprovalCaseScoped) -> scoped::Case {
    let LeanPluginApprovalCaseScoped {
        available,
        deliveries,
        expected,
        kind,
    } = scoped;
    let available: Vec<serde_json::Value> = available
        .iter()
        .map(|scope| scope_json(scope.workspace, scope.revision))
        .collect();
    let deliveries: Vec<serde_json::Value> = deliveries.iter().map(delivery_json).collect();
    serde_json::from_value(serde_json::json!([
        kind,
        available,
        grant_json(
            expected.action,
            expected.scope.workspace,
            expected.scope.revision,
        ),
        deliveries,
    ]))
    .unwrap()
}

fn case_of(case: &LeanPluginApprovalCase) -> Case {
    Case {
        directory: case.directory,
        write: case.write,
        initial: initial_of(&case.initial),
        steps: case.steps.iter().map(step_of).collect(),
        scoped: case.scoped.as_ref().map(scoped_of),
    }
}

fn assert_state(home: &Path, target: &Path, folder: &Path, expected: &State, context: &str) {
    assert_eq!(
        Queue::Unscoped.request_file(home, "q1").exists(),
        expected.present,
        "{context}"
    );
    let decision = std::fs::read(Queue::Unscoped.decision_file(home, "q1"))
        .ok()
        .map(|bytes| serde_json::from_slice::<Decision>(&bytes).unwrap().allow);
    assert_eq!(decision, expected.decision, "{context}");
    let grants = allowed::list(home).unwrap();
    assert_eq!(
        grants.len(),
        usize::from(expected.path_grant.is_some()) + usize::from(expected.folder_grant.is_some()),
        "{context}"
    );
    for (path, expected) in [
        (target, expected.path_grant),
        (folder, expected.folder_grant),
    ] {
        assert_eq!(
            grants.iter().find(|g| g.path == path).map(|g| g.access),
            expected.map(access),
            "{context}"
        );
    }
}

#[test]
fn generated_approval_traces_drive_native_queue_and_grants() {
    let cases = &gents_lean_vocab::lean_contract_snapshot().plugin_approval_cases;
    assert_eq!(cases.len(), 2178);
    for (index, case) in cases.iter().map(case_of).enumerate() {
        if let Some(scoped) = &case.scoped {
            scoped::run(index, &case, scoped);
            continue;
        }
        let home = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let folder = outside.path().canonicalize().unwrap();
        let target = folder.join("document.txt");
        std::fs::write(&target, "operator document").unwrap();
        let resolved = allowed::Resolved {
            target: if case.directory {
                folder.clone()
            } else {
                target.clone()
            },
            is_dir: case.directory,
        };
        let mut request = Request::new("team/files", &resolved, access(case.write), None);
        request.id = "q1".into();
        request.created_at_ms = 10;
        request.expires_at_ms = 20;
        write_atomic(
            &Queue::Unscoped.request_file(home.path(), "q1"),
            &serde_json::to_vec(&request).unwrap(),
        )
        .unwrap();
        for (path, grant) in [
            (&target, case.initial.path_grant),
            (&folder, case.initial.folder_grant),
        ] {
            if let Some(write) = grant {
                allowed::add(home.path(), path, access(write)).unwrap();
            }
        }
        assert_state(
            home.path(),
            &target,
            &folder,
            &case.initial,
            &format!("{index} initial"),
        );
        for (step_index, step) in case.steps.into_iter().enumerate() {
            let context = format!("case {index}, step {step_index}: {step:?}");
            let expected = match step {
                Step::Decide {
                    id,
                    now,
                    answer,
                    accepted,
                    expected,
                } => {
                    assert_eq!(
                        decide_with_clock(home.path(), &id, self::answer(&answer), || now).is_ok(),
                        accepted,
                        "{context}"
                    );
                    expected
                }
                Step::Cancel { accepted, expected } => {
                    assert!(accepted, "{context}");
                    drop(Cleanup {
                        home: home.path(),
                        id: "q1",
                        queue: Queue::Unscoped,
                    });
                    expected
                }
            };
            assert_state(home.path(), &target, &folder, &expected, &context);
        }
    }
}
