use gents_loop::tool_policy::{CommandExecutionMode, WorkspaceAuthority};

use crate::native::{apply_workspace_authority, CommandExecutionPolicy};
use gents_session::workspace::artifact_test_fixture;
use gents_session::workspace::overlay::overlay_tests::artifact_alternate_owner;

#[cfg(target_os = "macos")]
#[tokio::test]
async fn generated_artifact_admission_cases_drive_live_binding_and_launch_policy() {
    use crate::native::CommandConstraints;
    use crate::runtime_context::{
        scope_request_tool_execution_with_workspace_overlay, ToolWorkspaceScope,
    };
    use gents_loop::tool_policy::CommandNetworkMode;

    async fn execute(node: &defra_node::Node, mutation: &str) {
        let result = node.execute(mutation).await;
        assert!(!result.has_errors(), "{mutation}: {:?}", result.errors);
    }

    let cases = &gents_lean_vocab::lean_contract_snapshot().artifact_admission_cases;
    assert_eq!(cases.len(), 13);
    for case in cases {
        let name = case["name"].as_str().unwrap();
        let binding = &case["binding"];
        let mode = CommandExecutionMode::parse(case["mode"].as_str().unwrap()).unwrap();
        let fx = artifact_test_fixture(&[]).await;
        if name == "unsupported_platform" {
            assert_eq!(case["expected_admitted"], false);
            assert!(case["expected_bound_mode"].is_null());
            let error =
                gents_session::workspace::overlay::resolve_request_workspace_overlay_on_host(
                    &fx.node,
                    fx.owner.request(),
                    fx.owner.execution_generation().unwrap(),
                    true,
                    Some(fx._dir.path()),
                    false,
                )
                .await
                .unwrap_err();
            assert!(error.to_string().contains("enforceable artifact sandbox"));
            continue;
        }
        let alternate = artifact_alternate_owner(&fx, name, binding).await;
        let (request_owner, claim_outcome) = alternate.unwrap();
        if !matches!(
            claim_outcome,
            gents_session::request::lifecycle::ClaimOutcome::Claimed
        ) {
            assert_eq!(
                name, "wrong_owner",
                "unexpected preclaim denial: {claim_outcome:?}"
            );
            assert_eq!(
                case["expected_admitted"], false,
                "{name}: {claim_outcome:?}"
            );
            assert!(
                case["expected_bound_mode"].is_null(),
                "{name}: {claim_outcome:?}"
            );
        }
        if name == "unsealed" {
            execute(&fx.node, r#"mutation { update_IsolatedWorkspace(filter: { workspace_id: { _eq: "artifact-workspace" } }, input: { lifecycle_state: "ready" }) { _docID } }"#).await;
        }
        let generation = if name == "stale_incarnation" {
            "stale-execution"
        } else {
            request_owner.execution_generation().unwrap_or("")
        };
        let resolved = gents_session::workspace::overlay::resolve_request_workspace_overlay(
            &fx.node,
            request_owner.request(),
            generation,
            mode == CommandExecutionMode::ArtifactWrite,
            Some(fx._dir.path()),
        )
        .await;
        if matches!(
            name,
            "missing_binding"
                | "integrate_not_artifact"
                | "readwrite_not_artifact"
                | "unsealed"
                | "wrong_seal"
                | "wrong_owner"
                | "stale_incarnation"
        ) {
            assert!(
                resolved.is_err(),
                "{name}: invalid artifact context must stop at resolver, before tools"
            );
        }
        let overlay = resolved.ok().flatten();
        let authority = overlay.as_ref().map(|o| o.authority).or_else(|| {
            binding["authority"]
                .as_str()
                .map(|value| WorkspaceAuthority::parse(value).unwrap())
        });
        let grant = overlay.as_ref().and_then(|o| o.workspace_artifact.clone());
        if name == "foreign_root" {
            let target = grant
                .as_ref()
                .expect("valid resolver before root replacement")
                .root()
                .join("target");
            std::fs::remove_dir(&target).unwrap();
            std::os::unix::fs::symlink(fx.grant.source_root(), &target).unwrap();
        }
        let requested = CommandExecutionPolicy::write_capable().with_mode(mode);
        let effective = if mode == CommandExecutionMode::ArtifactWrite {
            requested
        } else {
            apply_workspace_authority(&requested, authority.expect("ordinary bound case"))
        };
        let root = fx.grant.source_root().to_owned();
        let constraints = CommandConstraints {
            allowed_argv_prefixes: vec![vec!["/bin/echo".into()]],
            forbidden_argv_prefixes: Vec::new(),
            network_mode: CommandNetworkMode::Disabled,
            execution_mode: effective.mode,
            sandbox: effective.mode,
            deny_all_argv: false,
            deny_git_metadata_writes: true,
        };
        let result = scope_request_tool_execution_with_workspace_overlay(
            None,
            tokio_util::sync::CancellationToken::new(),
            ToolWorkspaceScope {
                runtime_home: None,
                workspace_cwd: Some(root.clone()),
                workspace_root: Some(root.clone()),
                workspace_authority: authority,
                workspace_artifact: grant,
            },
            None,
            None,
            None,
            Default::default(),
            false,
            crate::native::prepare_managed_command(&root, "/bin/echo", &[], &constraints),
        )
        .await;
        let observed_mode = result.as_ref().ok().map(|_| effective.mode.as_str());
        assert_eq!(
            observed_mode,
            case["expected_bound_mode"].as_str(),
            "{name}: {result:?}"
        );
        assert_eq!(
            observed_mode == Some("artifact_write"),
            case["expected_admitted"].as_bool().unwrap(),
            "{name}"
        );
        assert_eq!(
            std::fs::read_to_string(root.join("README.md")).unwrap(),
            "sealed source\n"
        );
    }
}
