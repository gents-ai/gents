use super::*;

#[derive(Deserialize)]
struct Contracts {
    plugin_approval_cases: Vec<Case>,
}

#[derive(Debug, Deserialize)]
struct Case {
    directory: bool,
    write: bool,
    initial: State,
    steps: Vec<Step>,
}

#[derive(Debug, Deserialize)]
struct State {
    present: bool,
    decision: Option<bool>,
    path_grant: Option<bool>,
    folder_grant: Option<bool>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
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

fn access(write: bool) -> BindAccess {
    if write {
        BindAccess::ReadWrite
    } else {
        BindAccess::Read
    }
}

fn assert_state(home: &Path, target: &Path, folder: &Path, expected: &State, context: &str) {
    assert_eq!(
        request_file(home, "q1").exists(),
        expected.present,
        "{context}"
    );
    let decision = std::fs::read(decision_file(home, "q1"))
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
    let contracts: Contracts = gents_lean_contract::load_contract_snapshot().unwrap();
    assert_eq!(contracts.plugin_approval_cases.len(), 1188);
    for (index, case) in contracts.plugin_approval_cases.into_iter().enumerate() {
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
            &request_file(home.path(), "q1"),
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
                    let answer = match answer.as_str() {
                        "once" => Answer::Once,
                        "file" => Answer::AlwaysPath,
                        "always" => Answer::AlwaysFolder,
                        "deny" => Answer::Deny,
                        other => panic!("unknown modeled answer {other}"),
                    };
                    assert_eq!(
                        decide_with_clock(home.path(), &id, answer, || now).is_ok(),
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
                    });
                    expected
                }
            };
            assert_state(home.path(), &target, &folder, &expected, &context);
        }
    }
}
