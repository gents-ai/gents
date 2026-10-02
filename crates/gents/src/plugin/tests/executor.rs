//! The shared executor and the model-tool adapter over it.

use std::sync::Arc;

use super::{build_plugin_pack, ECHO_WAT};
use crate::document_config::PluginToolRef;
use crate::llm::tool::ToolDyn;
use crate::plugin::executor::PluginExecutor;
use crate::plugin::store::{self, InstalledPlugin};
use crate::plugin::tool::PluginTool;

/// Installs the echo plugin as `team/plugin` under a fresh home.
pub(crate) fn installed_echo() -> (tempfile::TempDir, InstalledPlugin) {
    installed_plugin(ECHO_WAT, None)
}

/// Installs `wat` as `team/plugin` under a fresh home, declaring `bind_dir`
/// on `path` with `access` when given.
pub(crate) fn installed_plugin(
    wat: &str,
    access: Option<crate::pack::BindAccess>,
) -> (tempfile::TempDir, InstalledPlugin) {
    let (mut declaration, bytes) = build_plugin_pack("echo_pack", wat, None);
    declaration.bind_dir = access.map(|access| crate::pack::PluginDirBinding {
        input_field: "path".into(),
        original_field: None,
        description: "a directory".into(),
        access,
    });
    let home = tempfile::tempdir().unwrap();
    let hex = format!("{:x}", <sha2::Sha256 as sha2::Digest>::digest(&bytes));
    store::store_bytes(home.path(), &hex, &bytes).unwrap();
    let record = InstalledPlugin {
        namespace: "team".into(),
        name: declaration.name.clone(),
        version: "0.1.0".into(),
        digest: format!("sha256:{hex}"),
        language: "rust".into(),
        declaration,
        granted: None,
        instructions: None,
        owner_pack_coordinate: None,
        owner_pack_digest: None,
        model_binding: None,
    };
    store::write_record(home.path(), &record).unwrap();
    (home, record)
}

fn tool_ref(digest: Option<&str>) -> PluginToolRef {
    PluginToolRef {
        plugin: "team/plugin".into(),
        digest: digest.map(str::to_owned),
    }
}

#[tokio::test]
async fn a_model_tool_runs_the_installed_plugin_with_its_declared_schema() {
    let (home, record) = installed_echo();
    let executor = Arc::new(PluginExecutor::new(Some(home.path().to_owned())));
    let tool =
        PluginTool::resolve(executor.clone(), &tool_ref(Some(&record.digest)), None).unwrap();

    let definition = tool.definition(String::new()).await;
    assert_eq!(definition.name, "plugin");
    assert_eq!(definition.description, record.declaration.description);
    assert_eq!(definition.parameters, record.declaration.input_schema);

    for n in 0..2 {
        let output = tool.call(format!(r#"{{"n":{n}}}"#)).await.unwrap();
        assert_eq!(output, format!(r#"{{"n":{n}}}"#));
    }
    assert_eq!(
        executor.admitted_len(),
        1,
        "a second call reuses the admission"
    );
}

#[test]
fn a_pin_that_no_longer_matches_the_installed_plugin_is_refused() {
    let (home, _) = installed_echo();
    let executor = Arc::new(PluginExecutor::new(Some(home.path().to_owned())));
    let stale = format!("sha256:{}", "0".repeat(64));
    let error = PluginTool::resolve(executor, &tool_ref(Some(&stale)), None)
        .err()
        .expect("a stale pin must not resolve");
    assert!(format!("{error:#}").contains("not the pinned"), "{error:#}");
}

#[test]
fn a_plugin_that_is_not_installed_is_refused() {
    let (home, _) = installed_echo();
    let executor = PluginExecutor::new(Some(home.path().to_owned()));
    assert!(executor.resolve("team/missing", None).is_err());
    assert!(executor.resolve("../escape", None).is_err());
    assert!(PluginExecutor::default()
        .resolve("team/plugin", None)
        .is_err());
}

#[tokio::test]
async fn a_changed_grant_is_admitted_again() {
    let (home, mut record) = installed_echo();
    let executor = PluginExecutor::new(Some(home.path().to_owned()));
    executor.call(&record, serde_json::json!(1)).await.unwrap();
    record.granted = Some(crate::plugin::Manifold::sealed());
    let call = executor.call(&record, serde_json::json!(2)).await.unwrap();
    assert_eq!(call.outcome.output, serde_json::json!(2));
    assert_eq!(
        executor.admitted_len(),
        1,
        "the new grant replaces the old admission"
    );
}

#[tokio::test]
async fn changed_resource_declaration_invalidates_admission_without_new_artifact() {
    let (home, mut record) = installed_plugin(&super::large_json_output_wat(1536 * 1024), None);
    record.granted = Some(crate::plugin::Manifold::sealed());
    record.declaration.limits = Some(crate::pack::PluginLimits {
        max_output_mib: Some(2),
        ..Default::default()
    });
    let executor = PluginExecutor::new(Some(home.path().to_owned()));
    let input = serde_json::json!({});
    assert_eq!(
        executor
            .call(&record, input.clone())
            .await
            .unwrap()
            .outcome
            .verdict,
        crate::plugin::PluginVerdict::Success
    );
    record.declaration.limits = None;
    assert_eq!(
        executor.call(&record, input).await.unwrap().outcome.verdict,
        crate::plugin::PluginVerdict::BadOutput
    );
}

#[test]
fn resource_increases_need_fresh_consent_and_reinstall_retains_it() {
    let (home, mut record) = installed_echo();
    record.declaration.limits = Some(crate::pack::PluginLimits {
        memory_mib: Some(1536),
        ..Default::default()
    });
    assert!(store::grant_on_install(home.path(), "team", &record.declaration, false).is_err());
    record.granted =
        store::grant_on_install(home.path(), "team", &record.declaration, true).unwrap();
    store::write_record(home.path(), &record).unwrap();
    assert!(
        store::grant_on_install(home.path(), "team", &record.declaration, false)
            .unwrap()
            .is_some()
    );
    record.declaration.limits.as_mut().unwrap().memory_mib = Some(2048);
    assert!(store::grant_on_install(home.path(), "team", &record.declaration, false).is_err());
}

mod bound {
    use super::*;
    use crate::pack::BindAccess;
    use crate::plugin::executor::BindContext;
    use crate::plugin::tests::{create_file_wat, open_and_copy_wat};
    use crate::plugin::{allowed, approval};
    use crate::tool_call_lifecycle::runtime::{
        scope_request_tool_execution_with_workspace_overlay, ToolWorkspaceScope,
    };

    fn tool_for(home: &tempfile::TempDir, record: &InstalledPlugin) -> PluginTool {
        let executor = Arc::new(PluginExecutor::new(Some(home.path().to_owned())));
        PluginTool::resolve(executor, &tool_ref(Some(&record.digest)), None).unwrap()
    }

    fn args(path: &std::path::Path) -> String {
        serde_json::json!({ "path": path }).to_string()
    }

    async fn refusal(tool: &PluginTool, path: &std::path::Path) -> String {
        format!("{:#}", tool.call(args(path)).await.unwrap_err())
    }

    /// Runs `tool` on `path` as a chat turn would, inside `workdir`.
    async fn in_session(
        tool: &PluginTool,
        path: &std::path::Path,
        workdir: &std::path::Path,
        interactive: bool,
    ) -> Result<String, String> {
        scope_request_tool_execution_with_workspace_overlay(
            None,
            tokio_util::sync::CancellationToken::new(),
            ToolWorkspaceScope::cwd_only(Some(workdir.to_owned())),
            None,
            Some("session-1".into()),
            None,
            Default::default(),
            false,
            approval::scope_interactive(interactive, tool.call(args(path))),
        )
        .await
        .map_err(|error| format!("{error:#}"))
    }

    /// Answers the next question the way the operator would.
    fn answer(
        home: &std::path::Path,
        allow: bool,
        always: bool,
    ) -> tokio::task::JoinHandle<String> {
        let answer = match (allow, always) {
            (false, _) => approval::Answer::Deny,
            (true, false) => approval::Answer::Once,
            (true, true) => approval::Answer::AlwaysFolder,
        };
        let home = home.to_owned();
        tokio::spawn(async move {
            loop {
                if let Some(request) = approval::pending(&home).unwrap().into_iter().next() {
                    approval::decide(&home, &request.id, answer).unwrap();
                    return request.prompt();
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            }
        })
    }

    struct Fixture {
        home: tempfile::TempDir,
        record: InstalledPlugin,
        root: std::path::PathBuf,
        _dir: tempfile::TempDir,
    }

    /// A reader of `in.json` in a world of `work/` (the session folder),
    /// `docs/` and `outside/`, each holding `in.json` and `sibling.json`.
    fn fixture(wat: &str, access: BindAccess) -> Fixture {
        let (home, record) = installed_plugin(wat, Some(access));
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        for folder in ["work", "docs", "outside"] {
            std::fs::create_dir_all(root.join(folder)).unwrap();
            std::fs::write(root.join(folder).join("in.json"), r#"{"read":true}"#).unwrap();
            std::fs::write(root.join(folder).join("sibling.json"), "{}").unwrap();
        }
        Fixture {
            home,
            record,
            root,
            _dir: dir,
        }
    }

    #[tokio::test]
    async fn a_model_tool_reads_a_file_in_an_allowed_folder() {
        let fx = fixture(&open_and_copy_wat("in.json", 0), BindAccess::Read);
        allowed::add(fx.home.path(), &fx.root.join("docs"), BindAccess::Read).unwrap();
        let output = tool_for(&fx.home, &fx.record)
            .call(args(&fx.root.join("docs/in.json")))
            .await
            .unwrap();
        assert_eq!(output, r#"{"read":true}"#);
    }

    #[tokio::test]
    async fn the_sandbox_sees_the_one_named_file_and_not_its_sibling() {
        let fx = fixture(&open_and_copy_wat("sibling.json", 0), BindAccess::Read);
        allowed::add(fx.home.path(), &fx.root.join("docs"), BindAccess::Read).unwrap();
        let tool = tool_for(&fx.home, &fx.record);
        let error = refusal(&tool, &fx.root.join("docs/in.json")).await;
        assert!(
            error.contains("did not return a result"),
            "the sibling is invisible: {error}"
        );
        let output = tool
            .call(args(&fx.root.join("docs/sibling.json")))
            .await
            .unwrap();
        assert_eq!(output, "{}");
    }

    #[tokio::test]
    async fn the_working_folder_is_allowed_with_no_configuration_and_read_only() {
        let fx = fixture(&open_and_copy_wat("in.json", 0), BindAccess::Read);
        let tool = tool_for(&fx.home, &fx.record);
        let work = fx.root.join("work");
        let output = in_session(&tool, &work.join("in.json"), &work, false)
            .await
            .unwrap();
        assert_eq!(output, r#"{"read":true}"#);

        let writer = fixture(&create_file_wat("out.json"), BindAccess::ReadWrite);
        let tool = tool_for(&writer.home, &writer.record);
        let work = writer.root.join("work");
        let error = in_session(&tool, &work, &work, false).await.unwrap_err();
        assert!(error.contains("gents plugin dirs add"), "{error}");
        assert!(error.contains("--access read_write"), "{error}");
        assert!(!work.join("out.json").exists());
    }

    #[tokio::test]
    async fn a_read_write_plugin_writes_only_where_the_folder_is_read_write() {
        let fx = fixture(&create_file_wat("out.json"), BindAccess::ReadWrite);
        let tool = tool_for(&fx.home, &fx.record);
        let reader = fx.root.join("docs");
        let writer = fx.root.join("work");
        allowed::add(fx.home.path(), &reader, BindAccess::Read).unwrap();
        allowed::add(fx.home.path(), &writer, BindAccess::ReadWrite).unwrap();

        let error = refusal(&tool, &reader).await;
        assert!(error.contains("--access read_write"), "{error}");
        assert!(!reader.join("out.json").exists());

        tool.call(args(&writer)).await.unwrap();
        assert_eq!(
            std::fs::read_to_string(writer.join("out.json")).unwrap(),
            "{}"
        );
    }

    #[tokio::test]
    async fn a_model_tool_without_the_path_field_runs_sealed() {
        let (home, record) = installed_plugin(ECHO_WAT, Some(BindAccess::Read));
        let output = tool_for(&home, &record)
            .call(r#"{"name":"a.txt"}"#.into())
            .await
            .unwrap();
        assert_eq!(output, r#"{"name":"a.txt"}"#);
    }

    #[tokio::test]
    async fn paths_that_escape_or_hide_are_refused_with_the_command_that_allows_them() {
        let fx = fixture(&open_and_copy_wat("in.json", 0), BindAccess::Read);
        let tool = tool_for(&fx.home, &fx.record);
        let work = fx.root.join("work");
        let dotdot = work.join("..").join("outside").join("in.json");
        let error = in_session(&tool, &dotdot, &work, false).await.unwrap_err();
        assert!(error.contains("gents plugin dirs add"), "{error}");
        assert!(error.contains("outside"), "{error}");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(fx.root.join("outside"), work.join("escape")).unwrap();
            let through = work.join("escape").join("in.json");
            let error = in_session(&tool, &through, &work, false).await.unwrap_err();
            assert!(error.contains("gents plugin dirs add"), "{error}");
        }
        assert!(
            refusal(&tool, std::path::Path::new("relative/x"))
                .await
                .contains("give the full path"),
            "no working folder, so a relative path has nothing to start from"
        );
        assert!(refusal(&tool, &fx.root.join("work/missing"))
            .await
            .contains("does not exist"));

        assert!(allowed::add(fx.home.path(), fx.home.path(), BindAccess::Read).is_err());
        let error = refusal(&tool, &fx.home.path().join("plugins")).await;
        assert!(error.contains("gents home"), "{error}");
    }

    #[tokio::test]
    async fn an_interactive_call_outside_every_folder_asks_and_allow_once_remembers_nothing() {
        let fx = fixture(&open_and_copy_wat("in.json", 0), BindAccess::Read);
        let tool = tool_for(&fx.home, &fx.record);
        let work = fx.root.join("work");
        let target = fx.root.join("outside/in.json");
        let asked = answer(fx.home.path(), true, false);
        let output = in_session(&tool, &target, &work, true).await.unwrap();
        assert_eq!(output, r#"{"read":true}"#);
        let prompt = asked.await.unwrap();
        assert_eq!(
            prompt,
            format!("Allow team/plugin to read {}?", target.display())
        );
        assert!(allowed::list(fx.home.path()).unwrap().is_empty());
    }

    #[tokio::test]
    async fn always_allow_stores_the_folder_so_the_next_call_does_not_ask() {
        let fx = fixture(&open_and_copy_wat("in.json", 0), BindAccess::Read);
        let tool = tool_for(&fx.home, &fx.record);
        let work = fx.root.join("work");
        let asked = answer(fx.home.path(), true, true);
        in_session(&tool, &fx.root.join("outside/in.json"), &work, true)
            .await
            .unwrap();
        asked.await.unwrap();
        assert_eq!(
            allowed::list(fx.home.path()).unwrap(),
            vec![allowed::AllowedDir {
                path: fx.root.join("outside"),
                access: BindAccess::Read
            }]
        );
        // No answerer is running: a second question would hang until it timed out.
        let output = in_session(&tool, &fx.root.join("outside/in.json"), &work, true).await;
        assert_eq!(output.unwrap(), r#"{"read":true}"#);
    }

    #[tokio::test]
    async fn a_denied_call_fails_and_stores_nothing() {
        let fx = fixture(&open_and_copy_wat("in.json", 0), BindAccess::Read);
        let tool = tool_for(&fx.home, &fx.record);
        let work = fx.root.join("work");
        let asked = answer(fx.home.path(), false, true);
        let error = in_session(&tool, &fx.root.join("outside/in.json"), &work, true)
            .await
            .unwrap_err();
        asked.await.unwrap();
        assert!(error.contains("declined"), "{error}");
        assert!(allowed::list(fx.home.path()).unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_headless_call_never_asks() {
        let fx = fixture(&open_and_copy_wat("in.json", 0), BindAccess::Read);
        let executor = PluginExecutor::new(Some(fx.home.path().to_owned()));
        let input = serde_json::json!({ "path": fx.root.join("outside/in.json") });
        let work = fx.root.join("work");
        let error = executor
            .bind_input(&fx.record, &input, &BindContext::headless(Some(&work)))
            .await
            .unwrap_err();
        assert!(error.contains("gents plugin dirs add"), "{error}");
        assert!(approval::pending(fx.home.path()).unwrap().is_empty());
    }

    #[tokio::test]
    async fn a_dispatched_call_on_a_spawned_task_files_a_question_the_decision_unblocks() {
        let fx = fixture(&open_and_copy_wat("in.json", 0), BindAccess::Read);
        let tool = Arc::new(tool_for(&fx.home, &fx.record));
        let work = fx.root.join("work");
        let target = fx.root.join("outside/in.json");
        let home = fx.home.path().to_owned();
        let queue = home.join(crate::home::PLUGIN_APPROVALS_DIR_NAME);
        let call = scope_request_tool_execution_with_workspace_overlay(
            None,
            tokio_util::sync::CancellationToken::new(),
            ToolWorkspaceScope::cwd_only(Some(work.clone())),
            None,
            Some("session-1".into()),
            None,
            Default::default(),
            false,
            approval::scope_interactive(true, async {
                // The way a background tool call leaves the chat's task.
                tokio::spawn(approval::carry_interactive(
                    scope_request_tool_execution_with_workspace_overlay(
                        None,
                        tokio_util::sync::CancellationToken::new(),
                        ToolWorkspaceScope::cwd_only(Some(work)),
                        None,
                        Some("session-1".into()),
                        None,
                        Default::default(),
                        false,
                        async move {
                            crate::tool_call_lifecycle::runtime::call_tool_managed(
                                tool.as_ref(),
                                args(&target),
                            )
                            .await
                        },
                    ),
                ))
                .await
                .unwrap()
            }),
        );
        let operator = async {
            let request = loop {
                if let Some(request) = approval::pending(&home).unwrap().into_iter().next() {
                    break request;
                }
                tokio::time::sleep(std::time::Duration::from_millis(5)).await;
            };
            assert!(queue.join(format!("{}.json", request.id)).is_file());
            approval::decide(&home, &request.id, approval::Answer::Once).unwrap();
            assert!(queue.join(format!("{}.decision", request.id)).is_file());
        };
        let (outcome, ()) = tokio::join!(call, operator);
        assert!(
            matches!(&outcome, crate::tool_call_lifecycle::ToolOutcome::Completed(text) if text == r#"{"read":true}"#),
            "{outcome:?}"
        );
    }

    #[tokio::test]
    async fn the_tool_root_is_the_working_folder_only_when_the_session_has_none_of_its_own() {
        let fx = fixture(&open_and_copy_wat("in.json", 0), BindAccess::Read);
        let executor = PluginExecutor::new(Some(fx.home.path().to_owned()));
        let input = serde_json::json!({ "path": fx.root.join("work/in.json") });
        let work = fx.root.join("work");
        let call = executor
            .call_data_bound(&fx.record, input.clone(), Some(&work))
            .await
            .unwrap();
        assert_eq!(call.outcome.output, serde_json::json!({ "read": true }));
        let error = executor
            .call_data_bound(&fx.record, input, None)
            .await
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("gents plugin dirs add"),
            "{error:#}"
        );
    }

    #[tokio::test]
    async fn a_declared_original_field_carries_the_real_path_of_a_single_file() {
        let (home, mut record) = installed_plugin(ECHO_WAT, Some(BindAccess::Read));
        record.declaration.bind_dir.as_mut().unwrap().original_field = Some("path_original".into());
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::write(root.join("a.txt"), "a").unwrap();
        let executor = PluginExecutor::new(Some(home.path().to_owned()));
        let input = serde_json::json!({
            "path": root.join("a.txt"),
            "path_original": "/spoofed",
        });
        let call = executor
            .call_data_bound(&record, input, Some(&root))
            .await
            .unwrap();
        let seen = &call.outcome.output;
        assert_eq!(seen["path_original"], root.join("a.txt").to_str().unwrap());
        let linked = seen["path"].as_str().unwrap();
        assert!(
            linked.ends_with("/a.txt") && !linked.starts_with(root.to_str().unwrap()),
            "{linked}"
        );

        let folder = serde_json::json!({ "path": root });
        let call = executor
            .call_data_bound(&record, folder, Some(&root))
            .await
            .unwrap();
        assert_eq!(
            call.outcome.output["path"],
            call.outcome.output["path_original"]
        );
    }
}
