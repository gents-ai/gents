use super::*;
use gents_loop::tool_policy::workspace_permissions as policy;

#[tokio::test]
#[ignore = "requires GENTS_APPROVED_BROWSER_WORKSPACE and GENTS_APPROVED_BROWSER_CACHE"]
async fn one_time_workspace_approval_runs_the_reported_six_browser_cases() {
    let project = PathBuf::from(std::env::var_os("GENTS_APPROVED_BROWSER_WORKSPACE").unwrap())
        .canonicalize()
        .unwrap();
    let cache = PathBuf::from(std::env::var_os("GENTS_APPROVED_BROWSER_CACHE").unwrap())
        .canonicalize()
        .unwrap();
    let package: serde_json::Value =
        serde_json::from_slice(&std::fs::read(project.join("package.json")).unwrap()).unwrap();
    assert_eq!(package["name"], "gents-vscode");
    assert!(cache.is_dir());
    let directory = tempfile::tempdir().unwrap();
    let home = directory.path().join("runtime");
    let key = workspace::WorkspaceKey::canonical("node", "requester", "agent", &project).unwrap();
    workspace::save_mode(&home, &key, 0, policy::Mode::Ask, None).unwrap();
    mark_listening(&home).unwrap();
    let context = workspace::for_request(
        Some(&home),
        "node",
        Some("requester"),
        "agent",
        Some(&project),
    )
    .unwrap();
    workspace::scope(
        context,
        scope_interactive(true, async {
            let tools = crate::tools::native::ToolContext::new(project.clone(), false).unwrap();
            let policy = crate::tools::native::CommandExecutionPolicy::write_capable()
                .with_mode(crate::tools::native::CommandExecutionMode::WorkspaceWrite);
            let args = vec![
                "-lc".into(),
                concat!(
                "npm run build && PLAYWRIGHT_BROWSERS_PATH=\"$1\" ",
                "npx playwright test tests/conversation.browser.ts --project=dark ",
                "--grep 'steers from the pending row|keeps pending-row geometry|",
                "applied opening attribution|initial goal message attribution' --max-failures=1",
            )
                .into(),
                "approved-browser-check".into(),
                cache.to_str().unwrap().into(),
            ];
            let run = || {
                crate::tools::native::run_command_for_test(
                    &tools,
                    "bash",
                    "/bin/sh",
                    &args,
                    None,
                    Duration::from_secs(120),
                    &policy,
                    true,
                    128 * 1024,
                )
            };
            let mut first = Box::pin(run());
            assert!(futures::poll!(first.as_mut()).is_pending());
            let questions = pending(&home).unwrap();
            assert_eq!(questions.len(), 1);
            let original = questions[0].id.clone();
            let workspace::BindingAction::Command {
                argv,
                cwd,
                writable_root,
                policy: bound,
                ..
            } = &questions[0].binding().unwrap().action
            else {
                panic!("command approval")
            };
            assert_eq!(
                argv,
                &std::iter::once("/bin/sh".to_owned())
                    .chain(args.iter().cloned())
                    .collect::<Vec<_>>()
            );
            assert_eq!(cwd, &project);
            assert_eq!(writable_root.as_ref(), Some(&project));
            assert_eq!(
                bound.mode,
                crate::tools::native::CommandExecutionMode::WorkspaceWrite
            );
            decide(&home, &original, Answer::Once).unwrap();
            let outcome = first.await;
            if let Some(report) = std::env::var_os("GENTS_APPROVED_BROWSER_REPORT") {
                let output = match &outcome {
                    Ok(output) => output.clone(),
                    Err(error) => format!("{error:#}"),
                };
                std::fs::write(report, output).unwrap();
            }
            let output =
                outcome.unwrap_or_else(|error| panic!("approved browser command: {error:#}"));
            let value: serde_json::Value = serde_json::from_str(&output).unwrap();
            assert_eq!(value["exit_code"], 0, "{output}");
            assert_eq!(value["execution_mode"], "workspace_write", "{output}");
            assert_eq!(value["sandbox"], "macos_seatbelt", "{output}");
            assert!(!value["timed_out"].as_bool().unwrap(), "{output}");
            assert!(
                value["stdout"].as_str().unwrap().contains("6 passed"),
                "{output}"
            );
            assert!(pending_requests(&home).unwrap().is_empty());
            assert!(workspace::get(&home, &key)
                .unwrap()
                .unwrap()
                .command_grants
                .is_empty());
            mark_listening(&home).unwrap();
            let mut again = Box::pin(run());
            assert!(futures::poll!(again.as_mut()).is_pending());
            let questions = pending(&home).unwrap();
            assert_eq!(questions.len(), 1);
            assert_ne!(questions[0].id, original);
            decide(&home, &questions[0].id, Answer::Deny).unwrap();
            assert!(again.await.is_err());
            assert!(pending_requests(&home).unwrap().is_empty());
        }),
    )
    .await;
}
