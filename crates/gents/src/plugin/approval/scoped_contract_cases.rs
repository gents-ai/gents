use super::*;
use gents_loop::tool_policy::workspace_permissions as policy;

#[derive(Debug, Deserialize)]
pub(super) struct Case {
    kind: policy::ApprovalKind,
    available: Vec<policy::Scope<u64>>,
    expected: policy::Grant<u64, u64>,
    deliveries: Vec<Delivery>,
}

#[derive(Debug, Deserialize)]
struct Delivery {
    available: Vec<policy::Scope<u64>>,
    received: policy::Grant<u64, u64>,
    expected: Option<bool>,
}

fn key(root: &Path, id: u64) -> workspace::WorkspaceKey {
    let path = root.join(format!("workspace-{id}"));
    std::fs::create_dir_all(&path).unwrap();
    workspace::WorkspaceKey::canonical("node", "requester", "agent", &path).unwrap()
}

fn binding(
    root: &Path,
    value: &policy::Grant<u64, u64>,
    resolved: &allowed::Resolved,
    access: BindAccess,
    kind: policy::ApprovalKind,
) -> workspace::Binding {
    policy::Grant {
        scope: policy::Scope {
            workspace: key(root, value.scope.workspace),
            revision: value.scope.revision,
        },
        action: match kind {
            policy::ApprovalKind::Filesystem => workspace::BindingAction::Plugin {
                plugin: "team/files".into(),
                artifact_digest: "artifact".into(),
                input_digest: format!("input-{}", value.action),
                path: resolved.target.clone(),
                is_dir: resolved.is_dir,
                access,
            },
            policy::ApprovalKind::Command => workspace::BindingAction::Command {
                argv: vec!["fixture-command".into(), format!("action-{}", value.action)],
                cwd: root.to_owned(),
                root: root.to_owned(),
                writable_root: None,
                policy: crate::tools::native::CommandExecutionPolicy::write_capable(),
            },
            policy::ApprovalKind::FileMutation => workspace::BindingAction::FileMutation {
                tool: "write_file".into(),
                input_digest: format!("input-{}", value.action),
                path: resolved.target.clone(),
                root: root.to_owned(),
                existed: true,
            },
        },
    }
}

fn entries(target: &Path, folder: &Path, state: &State) -> Vec<allowed::AllowedDir> {
    [(target, state.path_grant), (folder, state.folder_grant)]
        .into_iter()
        .filter_map(|(path, grant)| {
            grant.map(|write| allowed::AllowedDir {
                path: path.to_owned(),
                access: access(write),
            })
        })
        .collect()
}

fn observe(
    home: &Path,
    root: &Path,
    available: &[policy::Scope<u64>],
    expected: &policy::Grant<u64, u64>,
    target: &Path,
    folder: &Path,
    state: &State,
) {
    let settings: Vec<_> = available
        .iter()
        .map(|scope| workspace::Settings {
            workspace: key(root, scope.workspace),
            selection: policy::Preference {
                revision: scope.revision,
                mode: policy::Mode::Ask,
                review_task: None,
            },
            command_grants: Vec::new(),
            dirs: if scope.workspace == expected.scope.workspace {
                entries(target, folder, state)
            } else {
                Vec::new()
            },
        })
        .collect();
    std::fs::write(
        home.join(workspace::FILE),
        serde_json::to_vec(&serde_json::json!({ "entries": settings })).unwrap(),
    )
    .unwrap();
}

fn assert_scoped_state(
    home: &Path,
    target: &Path,
    folder: &Path,
    expected: &State,
    binding: &workspace::Binding,
    context: &str,
) {
    assert_eq!(
        Queue::Workspace.request_file(home, "q1").exists(),
        expected.present,
        "{context}"
    );
    let decision = std::fs::read(Queue::Workspace.decision_file(home, "q1"))
        .ok()
        .map(|bytes| serde_json::from_slice::<Decision>(&bytes).unwrap().allow);
    assert_eq!(decision, expected.decision, "{context}");
    assert!(
        allowed::list(home).unwrap().is_empty(),
        "workspace answers must not widen global grants: {context}"
    );
    let dirs = workspace::get(home, &binding.scope.workspace)
        .unwrap()
        .map(|settings| settings.dirs)
        .unwrap_or_default();
    assert_eq!(
        dirs.len(),
        usize::from(expected.path_grant.is_some()) + usize::from(expected.folder_grant.is_some()),
        "{context}"
    );
    for (path, grant) in [
        (target, expected.path_grant),
        (folder, expected.folder_grant),
    ] {
        assert_eq!(
            dirs.iter()
                .find(|entry| entry.path == path)
                .map(|entry| entry.access),
            grant.map(access),
            "{context}"
        );
    }
}

pub(super) fn run(index: usize, case: &super::Case, scoped: &Case) {
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
    let expected_binding = binding(
        &folder,
        &scoped.expected,
        &resolved,
        access(case.write),
        scoped.kind,
    );
    let mut request = Request::for_binding(expected_binding.clone(), None);
    request.id = "q1".into();
    request.created_at_ms = 10;
    request.expires_at_ms = 20;
    observe(
        home.path(),
        &folder,
        &scoped.available,
        &scoped.expected,
        &target,
        &folder,
        &case.initial,
    );
    write_atomic(
        &Queue::Workspace.request_file(home.path(), "q1"),
        &serde_json::to_vec(&request).unwrap(),
    )
    .unwrap();
    let mut state = &case.initial;
    for (step_index, step) in case.steps.iter().enumerate() {
        let context = format!("scoped case {index}, step {step_index}: {step:?}");
        state = match step {
            Step::Decide {
                id,
                now,
                answer: choice,
                accepted,
                expected,
            } => {
                assert_eq!(
                    decide_with_clock(home.path(), id, answer(choice), || *now).is_ok(),
                    *accepted,
                    "{context}"
                );
                expected
            }
            Step::Cancel { accepted, expected } => {
                assert!(*accepted, "{context}");
                drop(Cleanup {
                    home: home.path(),
                    id: "q1",
                    queue: Queue::Workspace,
                });
                expected
            }
        };
        assert_scoped_state(
            home.path(),
            &target,
            &folder,
            state,
            &expected_binding,
            &context,
        );
    }
    let decision = std::fs::read(Queue::Workspace.decision_file(home.path(), "q1")).ok();
    for (delivery_index, delivery) in scoped.deliveries.iter().enumerate() {
        observe(
            home.path(),
            &folder,
            &delivery.available,
            &scoped.expected,
            &target,
            &folder,
            state,
        );
        if let Some(bytes) = &decision {
            let mut delivered: Decision = serde_json::from_slice(bytes).unwrap();
            delivered.binding = Some(binding(
                &folder,
                &delivery.received,
                &resolved,
                access(case.write),
                scoped.kind,
            ));
            write_atomic(
                &Queue::Workspace.decision_file(home.path(), "q1"),
                &serde_json::to_vec(&delivered).unwrap(),
            )
            .unwrap();
        }
        let actual = read_decision(home.path(), &request);
        let context = format!("scoped case {index}, delivery {delivery_index}");
        if delivery.expected.is_none() && decision.is_some() {
            assert!(actual.is_err(), "{context}: {actual:?}");
        } else {
            assert_eq!(actual.unwrap(), delivery.expected, "{context}");
        }
    }
}
