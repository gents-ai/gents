use super::*;
use gents_loop::tool_policy::workspace_permissions as policy;

fn key(root: &Path, id: u64) -> workspace::WorkspaceKey {
    let path = root.join(format!("workspace-{id}"));
    std::fs::create_dir_all(&path).unwrap();
    workspace::WorkspaceKey::canonical("node", "requester", "agent", &path).unwrap()
}

fn kind_of(approval: &str) -> policy::ApprovalKind {
    serde_json::from_value(serde_json::Value::String(approval.to_owned())).unwrap()
}

fn expected_grant() -> policy::Grant<u64, u64> {
    policy::Grant {
        scope: policy::Scope {
            workspace: 1,
            revision: 2,
        },
        action: 3,
    }
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
                policy: crate::native::CommandExecutionPolicy::write_capable(),
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

fn entries(target: &Path, folder: &Path, grants: &Grants) -> Vec<allowed::AllowedDir> {
    [(target, grants.path_grant), (folder, grants.folder_grant)]
        .into_iter()
        .filter_map(|(path, grant)| {
            grant.map(|write| allowed::AllowedDir {
                path: path.to_owned(),
                access: access(write),
            })
        })
        .collect()
}

fn observe_settings(
    home: &Path,
    root: &Path,
    available: &[policy::Scope<u64>],
    target: &Path,
    folder: &Path,
    grants: &Grants,
) {
    let expected = expected_grant();
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
                entries(target, folder, grants)
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

fn scopes(available: &[(u64, u64)]) -> Vec<policy::Scope<u64>> {
    available
        .iter()
        .map(|(workspace, revision)| policy::Scope {
            workspace: *workspace,
            revision: *revision,
        })
        .collect()
}

fn workspace_request(
    fixture: &Paths,
    kind: policy::ApprovalKind,
    write: bool,
) -> (workspace::Binding, Request) {
    let expected_binding = binding(
        &fixture.folder,
        &expected_grant(),
        &fixture.resolved,
        access(write),
        kind,
    );
    let mut request = Request::for_binding(expected_binding.clone(), None);
    request.id = "q1".into();
    request.created_at_ms = 10;
    request.expires_at_ms = 20;
    (expected_binding, request)
}

#[test]
fn generated_scoped_approval_steps_drive_native_workspace_queue() {
    use gents_lean_vocab::families::plugin_approval_scoped_step as modeled;
    for case in gents_lean_vocab::families::plugin_approval_scoped_step::cases() {
        let input = &case.input;
        let fixture = paths(input.directory);
        let home = fixture.home.path();
        let kind = kind_of(&input.approval);
        let available = scopes(
            &input
                .available
                .iter()
                .map(|scope| (scope.workspace, scope.revision))
                .collect::<Vec<_>>(),
        );
        let (expected_binding, request) = workspace_request(&fixture, kind, input.write);
        observe_settings(
            home,
            &fixture.folder,
            &available,
            &fixture.target,
            &fixture.folder,
            &Grants {
                path_grant: input.initial.path_grant,
                folder_grant: input.initial.folder_grant,
            },
        );
        if input.initial.present {
            write_atomic(
                &Queue::Workspace.request_file(home, "q1"),
                &serde_json::to_vec(&request).unwrap(),
            )
            .unwrap();
        }
        settle(home, input.initial.decision);
        let accepted = act(
            home,
            Queue::Workspace,
            &input.kind,
            &input.id,
            input.now,
            &input.answer,
        );
        assert!(
            allowed::list(home).unwrap().is_empty(),
            "workspace answers must not widen global grants"
        );
        let dirs = workspace::resolve(home, &expected_binding.scope.workspace, None)
            .unwrap()
            .map(|settings| settings.dirs)
            .unwrap_or_default();
        let observed = observe_queue(
            home,
            Queue::Workspace,
            grants_of(&dirs, &fixture.target, &fixture.folder),
        );
        case.check(modeled::StepOutput {
            accepted,
            expected: modeled::StateWire {
                present: observed.present,
                decision: observed.decision,
                path_grant: observed.grants.path_grant,
                folder_grant: observed.grants.folder_grant,
            },
        });
    }
}

#[test]
fn generated_approval_deliveries_recheck_the_workspace_grant() {
    use gents_lean_vocab::families::plugin_approval_delivery as modeled;
    for case in gents_lean_vocab::families::plugin_approval_delivery::cases() {
        let input = &case.input;
        let fixture = paths(input.directory);
        let home = fixture.home.path();
        let kind = kind_of(&input.approval);
        let (_, request) = workspace_request(&fixture, kind, input.write);
        let none = Grants {
            path_grant: None,
            folder_grant: None,
        };
        observe_settings(
            home,
            &fixture.folder,
            &[expected_grant().scope],
            &fixture.target,
            &fixture.folder,
            &none,
        );
        write_atomic(
            &Queue::Workspace.request_file(home, "q1"),
            &serde_json::to_vec(&request).unwrap(),
        )
        .unwrap();
        settle(home, input.decision);
        observe_settings(
            home,
            &fixture.folder,
            &scopes(
                &input
                    .available
                    .iter()
                    .map(|scope| (scope.workspace, scope.revision))
                    .collect::<Vec<_>>(),
            ),
            &fixture.target,
            &fixture.folder,
            &none,
        );
        if let Ok(bytes) = std::fs::read(Queue::Workspace.decision_file(home, "q1")) {
            let mut delivered: Decision = serde_json::from_slice(&bytes).unwrap();
            delivered.binding = Some(binding(
                &fixture.folder,
                &policy::Grant {
                    scope: policy::Scope {
                        workspace: input.received.scope.workspace,
                        revision: input.received.scope.revision,
                    },
                    action: input.received.action,
                },
                &fixture.resolved,
                access(input.write),
                kind,
            ));
            write_atomic(
                &Queue::Workspace.decision_file(home, "q1"),
                &serde_json::to_vec(&delivered).unwrap(),
            )
            .unwrap();
        }
        let (delivered, refused) = match read_decision(home, &request) {
            Ok(delivered) => (delivered, false),
            Err(_) => (None, true),
        };
        case.check(modeled::DeliveryOutput { delivered, refused });
    }
}
