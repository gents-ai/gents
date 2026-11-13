use super::*;

#[path = "approval/scoped_contract_cases.rs"]
mod scoped;

struct Grants {
    path_grant: Option<bool>,
    folder_grant: Option<bool>,
}

struct Observed {
    present: bool,
    decision: Option<bool>,
    grants: Grants,
}

struct Paths {
    home: tempfile::TempDir,
    _outside: tempfile::TempDir,
    folder: PathBuf,
    target: PathBuf,
    resolved: allowed::Resolved,
}

fn paths(directory: bool) -> Paths {
    let home = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let folder = outside.path().canonicalize().unwrap();
    let target = folder.join("document.txt");
    std::fs::write(&target, "operator document").unwrap();
    let resolved = allowed::Resolved {
        target: if directory {
            folder.clone()
        } else {
            target.clone()
        },
        is_dir: directory,
    };
    Paths {
        home,
        _outside: outside,
        folder,
        target,
        resolved,
    }
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

fn grants_of(entries: &[allowed::AllowedDir], target: &Path, folder: &Path) -> Grants {
    let grant = |path: &Path| {
        entries
            .iter()
            .find(|entry| entry.path == path)
            .map(|entry| entry.access == BindAccess::ReadWrite)
    };
    let grants = Grants {
        path_grant: grant(target),
        folder_grant: grant(folder),
    };
    assert_eq!(
        entries.len(),
        usize::from(grants.path_grant.is_some()) + usize::from(grants.folder_grant.is_some()),
        "grants outside the requested path and folder: {entries:?}"
    );
    grants
}

fn observe_queue(home: &Path, queue: Queue, grants: Grants) -> Observed {
    Observed {
        present: queue.request_file(home, "q1").exists(),
        decision: std::fs::read(queue.decision_file(home, "q1"))
            .ok()
            .map(|bytes| serde_json::from_slice::<Decision>(&bytes).unwrap().allow),
        grants,
    }
}

fn settle(home: &Path, decision: Option<bool>) {
    if let Some(allow) = decision {
        decide_with_clock(
            home,
            "q1",
            if allow { Answer::Once } else { Answer::Deny },
            || 10,
        )
        .unwrap();
    }
}

fn act(
    home: &Path,
    queue: Queue,
    kind: &str,
    id: &Option<String>,
    now: Option<u64>,
    choice: &Option<String>,
) -> bool {
    match kind {
        "decide" => decide_with_clock(
            home,
            id.as_deref().unwrap(),
            answer(choice.as_deref().unwrap()),
            || now.unwrap(),
        )
        .is_ok(),
        "cancel" => {
            drop(Cleanup {
                home,
                id: "q1",
                queue,
            });
            !queue.request_file(home, "q1").exists()
        }
        other => panic!("unknown modeled step kind {other}"),
    }
}

#[test]
fn generated_approval_steps_drive_native_queue_and_grants() {
    use gents_lean_vocab::families::plugin_approval_unscoped_step as modeled;
    for case in gents_lean_vocab::families::plugin_approval_unscoped_step::cases() {
        let input = &case.input;
        let fixture = paths(input.directory);
        let home = fixture.home.path();
        if input.initial.present {
            let mut request =
                Request::new("team/files", &fixture.resolved, access(input.write), None);
            request.id = "q1".into();
            request.created_at_ms = 10;
            request.expires_at_ms = 20;
            write_atomic(
                &Queue::Unscoped.request_file(home, "q1"),
                &serde_json::to_vec(&request).unwrap(),
            )
            .unwrap();
        }
        for (path, grant) in [
            (&fixture.target, input.initial.path_grant),
            (&fixture.folder, input.initial.folder_grant),
        ] {
            if let Some(write) = grant {
                allowed::add(home, path, access(write)).unwrap();
            }
        }
        settle(home, input.initial.decision);
        let accepted = act(
            home,
            Queue::Unscoped,
            &input.kind,
            &input.id,
            input.now,
            &input.answer,
        );
        let observed = observe_queue(
            home,
            Queue::Unscoped,
            grants_of(
                &allowed::list(home).unwrap(),
                &fixture.target,
                &fixture.folder,
            ),
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
