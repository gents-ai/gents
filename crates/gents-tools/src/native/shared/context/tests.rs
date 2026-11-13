use super::{ToolContext, ToolError};
use crate::native::{CommandExecutionMode, CommandNetworkMode, CommandPolicyDenial, DenialReason};
use gents_loop::tool_call_lifecycle::FailureClass;

#[test]
fn policy_denial_reaches_dispatch_as_typed_failure() {
    let denial = CommandPolicyDenial::new(
        DenialReason::DisabledNetworkUnenforceable,
        CommandExecutionMode::ReadOnly,
        CommandNetworkMode::Disabled,
    );
    let expected = denial.tool_error_payload();

    match ToolError::policy_denial(denial).into_dispatch_error() {
        gents_loop::tool::ToolError::ReportedFailure { class, text } => {
            assert_eq!(class, FailureClass::PolicyDenied);
            assert_eq!(text, expected);
        }
        other => panic!("expected typed policy denial, got {other:?}"),
    }
}

#[test]
fn configured_root_is_the_unbound_default_base() {
    let root = tempfile::tempdir().unwrap();
    let nested = root.path().join("launcher-cwd");
    std::fs::create_dir_all(&nested).unwrap();

    let context = ToolContext::new(root.path().to_path_buf(), false).unwrap();
    let explicitly_bound =
        ToolContext::new_with_base(root.path().to_path_buf(), Some(nested.clone()), false).unwrap();

    assert_eq!(context.root(), std::fs::canonicalize(root.path()).unwrap());
    assert_eq!(context.base(), std::fs::canonicalize(root.path()).unwrap());
    assert_eq!(
        explicitly_bound.base(),
        std::fs::canonicalize(nested).unwrap()
    );
}

#[test]
fn relative_paths_resolve_from_base_inside_root() {
    let root =
        std::env::temp_dir().join(format!("gents-tool-context-root-{}", uuid::Uuid::new_v4()));
    let base = root.join("workspace");
    std::fs::create_dir_all(&base).unwrap();
    std::fs::write(base.join("README.md"), "ok").unwrap();

    let context = ToolContext::new_with_base(root.clone(), Some(base.clone()), false).unwrap();
    let resolved = context.resolve_existing_file("README.md").unwrap();

    assert_eq!(
        resolved,
        std::fs::canonicalize(base.join("README.md")).unwrap()
    );
    assert_eq!(context.display_path(&resolved), "README.md");

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn base_outside_root_falls_back_to_root() {
    let root =
        std::env::temp_dir().join(format!("gents-tool-context-root-{}", uuid::Uuid::new_v4()));
    let outside = std::env::temp_dir().join(format!(
        "gents-tool-context-outside-{}",
        uuid::Uuid::new_v4()
    ));
    std::fs::create_dir_all(&root).unwrap();
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(root.join("README.md"), "ok").unwrap();

    let context = ToolContext::new_with_base(root.clone(), Some(outside.clone()), false).unwrap();
    let resolved = context.resolve_existing_file("README.md").unwrap();

    assert_eq!(
        resolved,
        std::fs::canonicalize(root.join("README.md")).unwrap()
    );

    let _ = std::fs::remove_dir_all(root);
    let _ = std::fs::remove_dir_all(outside);
}

#[cfg(unix)]
#[test]
fn allow_create_rejects_a_dangling_symlink_leaf() {
    use std::os::unix::fs::symlink;

    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("not-created-yet");
    symlink(&target, root.path().join("notes.txt")).unwrap();
    let context = ToolContext::new(root.path().to_path_buf(), false).unwrap();

    let error = context
        .resolve_path_allow_create("notes.txt")
        .expect_err("a dangling symlink must not be treated as a creatable leaf");
    assert!(
        error.to_string().contains("canonicalizing path ancestor"),
        "{error:#}"
    );
}

#[tokio::test]
async fn request_workspace_overlays_resolve_as_the_tool_root_model() {
    for case in gents_lean_vocab::families::tool_root_resolution::cases() {
        let folder = tempfile::tempdir().unwrap();
        let top = std::fs::canonicalize(folder.path()).unwrap();
        let at = |segments: &[String]| {
            segments
                .iter()
                .fold(top.clone(), |path, segment| path.join(segment))
        };
        let input = &case.input;
        let directories = [
            Some(&input.static_root),
            input.static_base.as_ref(),
            input.cwd.as_ref(),
            input.root.as_ref(),
        ];
        for directory in directories.into_iter().flatten() {
            std::fs::create_dir_all(at(directory)).unwrap();
            if !input.absolute {
                std::fs::write(at(directory).join(input.target.join("/")), "ok").unwrap();
            }
        }
        if input.absolute {
            let file = at(&input.target);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, "ok").unwrap();
        }
        let context = ToolContext::new_with_base(
            at(&input.static_root),
            input.static_base.as_ref().map(|base| at(base)),
            false,
        )
        .unwrap();
        let target = if input.absolute {
            at(&input.target).to_str().unwrap().to_owned()
        } else {
            input.target.join("/")
        };
        let outcome = crate::runtime_context::scope_request_tool_execution_with_workspace_overlay(
            None,
            None,
            tokio_util::sync::CancellationToken::new(),
            crate::runtime_context::ToolWorkspaceScope {
                workspace_cwd: input.cwd.as_ref().map(|cwd| at(cwd)),
                workspace_root: input.root.as_ref().map(|root| at(root)),
                workspace_authority: Some(crate::native::WorkspaceAuthority::ReadWrite),
                ..Default::default()
            },
            None,
            None,
            None,
            Default::default(),
            false,
            async {
                context
                    .resolve_existing_file(&target)
                    .map(|resolved| {
                        let display = context.display_path(&resolved);
                        (resolved, display)
                    })
                    .map_err(|error| error.to_string())
            },
        )
        .await;
        let (resolved, display) = match outcome {
            Ok((resolved, display)) => {
                let segments = resolved
                    .strip_prefix(&top)
                    .unwrap()
                    .iter()
                    .map(|segment| segment.to_string_lossy().into_owned())
                    .collect::<Vec<_>>();
                let shown_whole = display == resolved.display().to_string();
                (Some(segments), (!shown_whole).then_some(display))
            }
            Err(error) if error.contains("outside the allowed tool root") => (None, None),
            Err(error) => panic!("{input:?}: {error}"),
        };
        case.check(gents_lean_vocab::families::tool_root_resolution::Output { resolved, display });
    }
}
