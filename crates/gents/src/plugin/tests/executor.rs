use gents_protocol::error_chain::ErrorChain;
use std::sync::Arc;

use super::{build_plugin_pack, ECHO_WAT};
use crate::document_config::PluginToolRef;
use crate::llm::tool::ToolDyn;
use crate::plugin::executor::PluginExecutor;
use crate::plugin::store::{self, InstalledPlugin};
use crate::plugin::tool::PluginTool;

pub(crate) fn installed_echo() -> (tempfile::TempDir, InstalledPlugin) {
    installed_plugin(ECHO_WAT, None)
}

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
        write_fields: Vec::new(),
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
        data_unique_keys: Default::default(),
        data_collection_fields: Default::default(),
    };
    store::write_record(home.path(), &record).unwrap();
    (home, record)
}

pub(crate) fn asking_plugin_wat(
    service: &str,
    results: &str,
    canned: &serde_json::Value,
    forever: bool,
) -> String {
    let escape = |text: &str| {
        text.bytes()
            .map(|byte| format!("\\{byte:02x}"))
            .collect::<String>()
    };
    let canned = canned.to_string();
    let calls = format!("\"{service}\":true");
    let stop = if forever {
        "(i32.const 1)".to_owned()
    } else {
        format!(
            "(i32.eqz (call $contains (i32.const 50000) (i32.const {}) (local.get $n)))",
            results.len()
        )
    };
    format!(
        r#"(module
  (import "wasi_snapshot_preview1" "fd_read" (func $fd_read (param i32 i32 i32 i32) (result i32)))
  (import "wasi_snapshot_preview1" "fd_write" (func $fd_write (param i32 i32 i32 i32) (result i32)))
  (memory (export "memory") 2)
  (data (i32.const 8192) "{canned_bytes}")
  (data (i32.const 50000) "{results_bytes}")
  (data (i32.const 50100) "{calls_bytes}")
  (func $contains (param $needle i32) (param $nlen i32) (param $hlen i32) (result i32)
    (local $i i32) (local $j i32)
    (block $notfound
      (loop $outer
        (br_if $notfound (i32.gt_u (i32.add (local.get $i) (local.get $nlen)) (local.get $hlen)))
        (local.set $j (i32.const 0))
        (block $mismatch
          (loop $inner
            (if (i32.eq (local.get $j) (local.get $nlen)) (then (return (i32.const 1))))
            (br_if $mismatch (i32.ne
              (i32.load8_u (i32.add (local.get $i) (local.get $j)))
              (i32.load8_u (i32.add (local.get $needle) (local.get $j)))))
            (local.set $j (i32.add (local.get $j) (i32.const 1)))
            (br $inner)))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $outer)))
    (i32.const 0))
  (func (export "_start")
    (local $n i32)
    (i32.store (i32.const 60000) (i32.const 0))
    (i32.store (i32.const 60004) (i32.const 8000))
    (drop (call $fd_read (i32.const 0) (i32.const 60000) (i32.const 1) (i32.const 60008)))
    (local.set $n (i32.load (i32.const 60008)))
    (if (call $contains (i32.const 50100) (i32.const {calls_len}) (local.get $n))
      (then
        (if {stop}
          (then
            (i32.store (i32.const 60000) (i32.const 8192))
            (i32.store (i32.const 60004) (i32.const {canned_len}))
            (drop (call $fd_write (i32.const 1) (i32.const 60000) (i32.const 1) (i32.const 60012)))
            (return)))))
    (i32.store (i32.const 60000) (i32.const 0))
    (i32.store (i32.const 60004) (local.get $n))
    (drop (call $fd_write (i32.const 1) (i32.const 60000) (i32.const 1) (i32.const 60012)))))"#,
        canned_bytes = escape(&canned),
        results_bytes = escape(results),
        calls_bytes = escape(&calls),
        calls_len = calls.len(),
        canned_len = canned.len(),
    )
}

fn tool_ref(digest: Option<&str>) -> PluginToolRef {
    PluginToolRef {
        tool_calls: false,
        plugin: "team/plugin".into(),
        digest: digest.map(str::to_owned),
        input_fields: Vec::new(),
        job: None,
    }
}

#[tokio::test]
async fn installed_filesystem_grants_cannot_expose_operator_aliases() {
    use crate::plugin::{FsAccess, Manifold};

    for write in [false, true] {
        let wat = if write {
            super::create_file_wat("out.json")
        } else {
            super::open_and_copy_wat("in.json", 0)
        };
        let (home, mut record) = installed_plugin(&wat, None);
        let folder = tempfile::tempdir().unwrap();
        let root = folder.path().canonicalize().unwrap();
        std::fs::write(root.join("in.json"), r#"{"read":true}"#).unwrap();
        let granted = Manifold {
            fs: if write {
                FsAccess::ReadWrite(vec![root.clone()])
            } else {
                FsAccess::ReadOnly(vec![root.clone()])
            },
            ..Manifold::sealed()
        };
        record.declaration.manifold = Some(serde_json::to_value(&granted).unwrap());
        record.granted = Some(granted);
        store::write_record(home.path(), &record).unwrap();
        let executor = PluginExecutor::new(Some(home.path().to_owned()));
        let first = executor.call(&record, serde_json::json!({})).await.unwrap();
        assert_eq!(first.outcome.verdict, crate::plugin::PluginVerdict::Success);
        if write {
            assert_eq!(
                std::fs::read_to_string(root.join("out.json")).unwrap(),
                "{}"
            );
            std::fs::remove_file(root.join("out.json")).unwrap();
        } else {
            assert_eq!(first.outcome.output, serde_json::json!({"read":true}));
        }
        let permissions = home.path().join(crate::home::ALLOWED_DIRS_FILE_NAME);
        std::fs::write(&permissions, "operator state").unwrap();
        std::fs::hard_link(&permissions, root.join("operator-alias")).unwrap();
        let error = executor
            .call(&record, serde_json::json!({}))
            .await
            .unwrap_err();
        assert!(
            format!("{}", ErrorChain(error.as_ref())).contains("protected runtime permissions"),
            "{}",
            ErrorChain(error.as_ref())
        );
        assert_eq!(
            std::fs::read_to_string(&permissions).unwrap(),
            "operator state"
        );
        assert!(!root.join("out.json").exists());
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
    assert!(
        format!("{}", ErrorChain(error.as_ref())).contains("not the pinned"),
        "{}",
        ErrorChain(error.as_ref())
    );
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
    store::write_record(home.path(), &record).unwrap();
    let call = executor.call(&record, serde_json::json!(2)).await.unwrap();
    assert_eq!(call.outcome.output, serde_json::json!(2));
    assert_eq!(
        executor.admitted_len(),
        1,
        "the new grant replaces the old admission"
    );
}

#[tokio::test]
async fn a_removed_plugin_fails_closed_with_a_warm_cache() {
    let (home, record) = installed_echo();
    let executor = PluginExecutor::new(Some(home.path().to_owned()));
    executor.call(&record, serde_json::json!(1)).await.unwrap();
    assert_eq!(executor.admitted_len(), 1);
    store::remove_record(home.path(), &record.namespace, &record.name).unwrap();
    let error = executor
        .call(&record, serde_json::json!(2))
        .await
        .unwrap_err();
    assert!(
        format!("{}", ErrorChain(error.as_ref())).contains("is not installed"),
        "{}",
        ErrorChain(error.as_ref())
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
    store::write_record(home.path(), &record).unwrap();
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
    store::write_record(home.path(), &record).unwrap();
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
        format!("{}", ErrorChain(&tool.call(args(path)).await.unwrap_err()))
    }

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
        .map_err(|error| format!("{}", ErrorChain(&error)))
    }

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
    async fn workspace_modes_allow_routine_plugin_writes_without_widening_folder_grants() {
        use approval::workspace::{self, Mode, WorkspaceKey};

        for mode in [Mode::Ask, Mode::AutoReview, Mode::FullAccess] {
            let fx = fixture(&create_file_wat("out.json"), BindAccess::ReadWrite);
            let work = fx.root.join("work");
            let key = WorkspaceKey::canonical("node", "requester", "agent", &work).unwrap();
            let reviewer = workspace::ReviewTaskSelection::for_task(
                &workspace::default_review_task("node", "agent"),
            );
            workspace::save_mode(
                fx.home.path(),
                &key,
                0,
                mode,
                (mode == Mode::AutoReview).then_some(&reviewer),
            )
            .unwrap();
            let context = workspace::for_request(
                Some(fx.home.path()),
                "node",
                Some("requester"),
                "agent",
                Some(&work),
            )
            .unwrap();
            let tool = tool_for(&fx.home, &fx.record);
            workspace::scope(context, in_session(&tool, &work, &work, false))
                .await
                .unwrap();
            assert_eq!(
                std::fs::read_to_string(work.join("out.json")).unwrap(),
                "{}"
            );
            assert!(allowed::list(fx.home.path()).unwrap().is_empty());
            assert!(approval::pending_requests(fx.home.path())
                .unwrap()
                .is_empty());
        }
    }

    #[tokio::test]
    async fn a_bound_plugin_rejects_changed_input_and_revised_workspace_consent() {
        use approval::workspace::{self, Mode, WorkspaceKey};

        let fx = fixture(&create_file_wat("out.json"), BindAccess::ReadWrite);
        let work = fx.root.join("work");
        let docs = fx.root.join("docs");
        let key = WorkspaceKey::canonical("node", "requester", "agent", &work).unwrap();
        let settings =
            workspace::save_mode(fx.home.path(), &key, 0, Mode::FullAccess, None).unwrap();
        let context = workspace::for_request(
            Some(fx.home.path()),
            "node",
            Some("requester"),
            "agent",
            Some(&work),
        )
        .unwrap();
        let executor = PluginExecutor::new(Some(fx.home.path().to_owned()));
        let input = serde_json::json!({"path": docs});
        let bound = workspace::scope(
            context,
            executor.bind_input(&fx.record, &input, &BindContext::headless(Some(&work))),
        )
        .await
        .unwrap()
        .unwrap();
        let changed = serde_json::json!({"path": docs, "operation": "different"});
        let error = executor
            .call_bound(&fx.record, changed, bound.clone())
            .await
            .unwrap_err();
        assert!(
            format!("{}", ErrorChain(error.as_ref())).contains("invocation changed"),
            "{}",
            ErrorChain(error.as_ref())
        );
        assert!(!docs.join("out.json").exists());

        executor
            .call_bound(&fx.record, input.clone(), bound.clone())
            .await
            .unwrap();
        assert_eq!(
            std::fs::read_to_string(docs.join("out.json")).unwrap(),
            "{}"
        );
        std::fs::remove_file(docs.join("out.json")).unwrap();
        workspace::save_mode(
            fx.home.path(),
            &key,
            settings.selection.revision,
            Mode::Ask,
            None,
        )
        .unwrap();
        let error = executor
            .call_bound(&fx.record, input, bound)
            .await
            .unwrap_err();
        assert!(
            format!("{}", ErrorChain(error.as_ref())).contains("permissions changed"),
            "{}",
            ErrorChain(error.as_ref())
        );
        assert!(!docs.join("out.json").exists());
    }

    #[tokio::test]
    async fn plugin_mounts_reject_operator_aliases_at_admission_and_execution() {
        use approval::workspace::{self, Mode, WorkspaceKey};

        for added_after_binding in [false, true] {
            let fx = fixture(&create_file_wat("out.json"), BindAccess::ReadWrite);
            let work = fx.root.join("work");
            let docs = fx.root.join("docs");
            let key = WorkspaceKey::canonical("node", "requester", "agent", &work).unwrap();
            workspace::save_mode(fx.home.path(), &key, 0, Mode::FullAccess, None).unwrap();
            let permission_file = workspace::preferences_file(fx.home.path());
            let before = std::fs::read(&permission_file).unwrap();
            let context = workspace::for_request(
                Some(fx.home.path()),
                "node",
                Some("requester"),
                "agent",
                Some(&work),
            )
            .unwrap();
            let executor = PluginExecutor::new(Some(fx.home.path().to_owned()));
            let input = serde_json::json!({"path": docs});
            let bind = BindContext::headless(Some(&work));
            let bound = if added_after_binding {
                Some(
                    workspace::scope(
                        context.clone(),
                        executor.bind_input(&fx.record, &input, &bind),
                    )
                    .await
                    .unwrap()
                    .unwrap(),
                )
            } else {
                None
            };
            std::fs::create_dir(docs.join("nested")).unwrap();
            let alias = docs.join("nested/operator-alias");
            std::fs::hard_link(&permission_file, &alias).unwrap();
            let error = match bound {
                Some(bound) => format!(
                    "{}",
                    ErrorChain(
                        executor
                            .call_bound(&fx.record, input, bound)
                            .await
                            .unwrap_err()
                            .as_ref()
                    )
                ),
                None => workspace::scope(
                    context.clone(),
                    executor.bind_input(&fx.record, &input, &bind),
                )
                .await
                .unwrap_err(),
            };
            assert!(error.contains("protected runtime permissions"), "{error}");
            let file_input = serde_json::json!({"path": alias});
            let error =
                workspace::scope(context, executor.bind_input(&fx.record, &file_input, &bind))
                    .await
                    .unwrap_err();
            assert!(
                error.contains("permission files cannot be shared"),
                "{error}"
            );
            assert_eq!(std::fs::read(&permission_file).unwrap(), before);
            assert!(!docs.join("out.json").exists());
            assert!(approval::pending_requests(fx.home.path())
                .unwrap()
                .is_empty());
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_guest_cannot_follow_an_outside_link_from_a_shared_folder() {
        use approval::workspace::{self, Mode, WorkspaceKey};

        let fx = fixture(&open_and_copy_wat("operator-link", 0), BindAccess::Read);
        let work = fx.root.join("work");
        let key = WorkspaceKey::canonical("node", "requester", "agent", &work).unwrap();
        workspace::save_mode(fx.home.path(), &key, 0, Mode::Ask, None).unwrap();
        let permission_file = workspace::preferences_file(fx.home.path());
        let before = std::fs::read(&permission_file).unwrap();
        std::os::unix::fs::symlink(&permission_file, work.join("operator-link")).unwrap();
        let context = workspace::for_request(
            Some(fx.home.path()),
            "node",
            Some("requester"),
            "agent",
            Some(&work),
        )
        .unwrap();
        let tool = tool_for(&fx.home, &fx.record);
        let error = workspace::scope(context, in_session(&tool, &work, &work, false))
            .await
            .unwrap_err();
        assert!(error.contains("did not return a result"), "{error}");
        assert_eq!(std::fs::read(&permission_file).unwrap(), before);
    }

    #[tokio::test]
    async fn workspace_consent_uses_the_original_scope_and_reads_each_new_selection() {
        use approval::workspace::{self, Mode, WorkspaceKey};

        let fx = fixture(&open_and_copy_wat("in.json", 0), BindAccess::Read);
        let work = fx.root.join("work");
        let other = fx.root.join("docs");
        let target = fx.root.join("outside/in.json");
        let key = WorkspaceKey::canonical("node", "requester", "agent", &work).unwrap();
        let full = workspace::save_mode(fx.home.path(), &key, 0, Mode::FullAccess, None).unwrap();
        let context = workspace::for_request(
            Some(fx.home.path()),
            "node",
            Some("requester"),
            "agent",
            Some(&work),
        )
        .unwrap();
        let tool = tool_for(&fx.home, &fx.record);
        workspace::scope(context, async {
            let output = in_session(&tool, &target, &other, true).await.unwrap();
            assert_eq!(output, r#"{"read":true}"#);
            assert!(allowed::list(fx.home.path()).unwrap().is_empty());
            assert!(approval::pending_requests(fx.home.path())
                .unwrap()
                .is_empty());

            workspace::save_mode(
                fx.home.path(),
                &key,
                full.selection.revision,
                Mode::Ask,
                None,
            )
            .unwrap();
            let declined = answer(fx.home.path(), false, false);
            let error = in_session(&tool, &target, &other, true).await.unwrap_err();
            declined.await.unwrap();
            assert!(error.contains("operator declined"), "{error}");
        })
        .await;
        let ask = workspace::get(fx.home.path(), &key).unwrap().unwrap();
        workspace::save_mode(
            fx.home.path(),
            &key,
            ask.selection.revision,
            Mode::FullAccess,
            None,
        )
        .unwrap();
        for (agent, folder) in [("other-agent", &work), ("agent", &other)] {
            let other_context = workspace::for_request(
                Some(fx.home.path()),
                "node",
                Some("requester"),
                agent,
                Some(folder),
            )
            .unwrap();
            let error = workspace::scope(other_context, in_session(&tool, &target, folder, false))
                .await
                .unwrap_err();
            assert!(error.contains("outside the folders"), "{error}");
        }
    }

    #[tokio::test]
    async fn workspace_consent_without_a_reviewer_still_needs_a_person() {
        use approval::workspace::{self, Mode, WorkspaceKey};

        let fx = fixture(&open_and_copy_wat("in.json", 0), BindAccess::Read);
        let work = fx.root.join("work");
        let target = fx.root.join("outside/in.json");
        let key = WorkspaceKey::canonical("node", "requester", "agent", &work).unwrap();
        workspace::save_mode(
            fx.home.path(),
            &key,
            0,
            Mode::AutoReview,
            Some(&workspace::ReviewTaskSelection::for_task(
                &crate::document_config::Task {
                    task_id: "review-task".into(),
                    ..workspace::default_review_task("node", "agent")
                },
            )),
        )
        .unwrap();
        let context = workspace::for_request(
            Some(fx.home.path()),
            "node",
            Some("requester"),
            "agent",
            Some(&work),
        )
        .unwrap();
        let tool = tool_for(&fx.home, &fx.record);
        let declined = answer(fx.home.path(), false, false);
        let error = workspace::scope(context, in_session(&tool, &target, &work, true))
            .await
            .unwrap_err();
        declined.await.unwrap();
        assert!(error.contains("operator declined"), "{error}");
        assert!(allowed::list(fx.home.path()).unwrap().is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_workspace_mode_cannot_follow_a_redirected_working_folder() {
        use approval::workspace::{self, Mode, WorkspaceKey};

        let fx = fixture(&open_and_copy_wat("in.json", 0), BindAccess::Read);
        let work = fx.root.join("work");
        let key = WorkspaceKey::canonical("node", "requester", "agent", &work).unwrap();
        workspace::save_mode(fx.home.path(), &key, 0, Mode::Ask, None).unwrap();
        let context = workspace::for_request(
            Some(fx.home.path()),
            "node",
            Some("requester"),
            "agent",
            Some(&work),
        )
        .unwrap();
        std::fs::rename(&work, fx.root.join("original-work")).unwrap();
        std::os::unix::fs::symlink(fx.root.join("docs"), &work).unwrap();
        let tool = tool_for(&fx.home, &fx.record);
        let error = workspace::scope(
            context,
            in_session(&tool, &work.join("in.json"), &work, false),
        )
        .await
        .unwrap_err();
        assert!(error.contains("path changed"), "{error}");
        assert!(allowed::list(fx.home.path()).unwrap().is_empty());
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
    async fn one_plugin_writes_only_on_a_call_that_sets_its_write_field() {
        let mut fx = fixture(&create_file_wat("out.json"), BindAccess::ReadWrite);
        fx.record
            .declaration
            .bind_dir
            .as_mut()
            .unwrap()
            .write_fields = vec!["output".into(), "delete".into()];
        store::write_record(fx.home.path(), &fx.record).unwrap();
        let tool = tool_for(&fx.home, &fx.record);
        let reader = fx.root.join("docs");
        let writer = fx.root.join("work");
        allowed::add(fx.home.path(), &reader, BindAccess::Read).unwrap();
        allowed::add(fx.home.path(), &writer, BindAccess::ReadWrite).unwrap();
        let call = |path: &std::path::Path, output: bool| {
            let mut input = serde_json::json!({ "path": path });
            if output {
                input["output"] = serde_json::json!("out.json");
            }
            tool.call(input.to_string())
        };

        let read = format!("{}", ErrorChain(&call(&reader, false).await.unwrap_err()));
        assert!(
            read.contains("did not return a result") && !read.contains("dirs add"),
            "a reading call binds under a read-only folder and its sandbox refuses the write: {read}"
        );
        let escalation = format!("{}", ErrorChain(&call(&reader, true).await.unwrap_err()));
        for expected in [
            "allowed read-only",
            r#"sets "output""#,
            "--access read_write",
            r#"call again without "output""#,
        ] {
            assert!(escalation.contains(expected), "{escalation}");
        }
        assert!(!reader.join("out.json").exists());
        let both =
            format!(
            "{}",
            ErrorChain(&tool
                .call(
                    serde_json::json!({ "path": reader, "output": "out.json", "delete": true })
                        .to_string()
                )
                .await
                .unwrap_err())
        );
        assert!(
            both.contains(r#"call again without "output", "delete" to only read"#),
            "a call that sets two write fields is told to drop both: {both}"
        );

        call(&writer, false).await.unwrap_err();
        assert!(
            !writer.join("out.json").exists(),
            "a reading call runs read-only even where writing is allowed"
        );
        call(&writer, true).await.unwrap();
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
    async fn a_composed_child_asks_and_resumes_on_the_operator() {
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
        assert!(approval::pending(fx.home.path()).unwrap().is_empty());
        assert!(
            allowed::list(fx.home.path()).unwrap().is_empty(),
            "allow once remembers nothing"
        );
    }

    #[tokio::test]
    async fn a_composed_child_the_operator_denies_fails_and_stores_nothing() {
        let fx = fixture(&open_and_copy_wat("in.json", 0), BindAccess::Read);
        let tool = tool_for(&fx.home, &fx.record);
        let work = fx.root.join("work");
        let target = fx.root.join("outside/in.json");
        let asked = answer(fx.home.path(), false, true);
        let error = in_session(&tool, &target, &work, true).await.unwrap_err();
        let prompt = tokio::time::timeout(std::time::Duration::from_secs(10), asked)
            .await
            .expect("the operator was asked about the child's path")
            .unwrap();
        assert_eq!(
            prompt,
            format!("Allow team/plugin to read {}?", target.display())
        );
        assert!(error.contains("declined"), "{error}");
        assert!(approval::pending(fx.home.path()).unwrap().is_empty());
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
        let queue = approval::queue_dir(&home);
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
            .call_data_bound(&fx.record, input.clone(), Some(&work), false)
            .await
            .unwrap();
        assert_eq!(call.outcome.output, serde_json::json!({ "read": true }));
        let error = executor
            .call_data_bound(&fx.record, input, None, false)
            .await
            .unwrap_err();
        assert!(
            format!("{}", ErrorChain(error.as_ref())).contains("gents plugin dirs add"),
            "{}",
            ErrorChain(error.as_ref())
        );
    }

    #[tokio::test]
    async fn a_bound_call_fails_when_the_plugin_is_reinstalled_after_binding() {
        let fx = fixture(&open_and_copy_wat("in.json", 0), BindAccess::Read);
        let executor = PluginExecutor::new(Some(fx.home.path().to_owned()));
        let input = serde_json::json!({ "path": fx.root.join("work/in.json") });
        let work = fx.root.join("work");
        let context = crate::plugin::executor::BindContext::headless(Some(&work));
        let bound = executor
            .bind_input(&fx.record, &input, &context)
            .await
            .unwrap()
            .expect("a bound directory");
        let mut reinstalled = fx.record.clone();
        reinstalled
            .declaration
            .bind_dir
            .as_mut()
            .unwrap()
            .original_field = Some("path_original".into());
        store::write_record(fx.home.path(), &reinstalled).unwrap();
        let error = executor
            .call_bound(&fx.record, input, bound)
            .await
            .unwrap_err();
        assert!(
            format!("{}", ErrorChain(error.as_ref())).contains("reinstalled"),
            "{}",
            ErrorChain(error.as_ref())
        );
    }

    #[tokio::test]
    async fn a_declared_original_field_carries_the_real_path_of_a_single_file() {
        let (home, mut record) = installed_plugin(ECHO_WAT, Some(BindAccess::Read));
        record.declaration.bind_dir.as_mut().unwrap().original_field = Some("path_original".into());
        store::write_record(home.path(), &record).unwrap();
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        std::fs::write(root.join("a.txt"), "a").unwrap();
        let executor = PluginExecutor::new(Some(home.path().to_owned()));
        let input = serde_json::json!({
            "path": root.join("a.txt"),
            "path_original": "/spoofed",
        });
        let call = executor
            .call_data_bound(&record, input, Some(&root), false)
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
            .call_data_bound(&record, folder, Some(&root), false)
            .await
            .unwrap();
        assert_eq!(
            call.outcome.output["path"],
            call.outcome.output["path_original"]
        );
    }
}

#[tokio::test]
async fn generated_plugin_invocation_retry_revalidates_current_installation() {
    let cases = &crate::lean_vocab_test::lean_contract_snapshot().plugin_resource_cases;
    for case in cases["invocation"].as_array().unwrap() {
        let (home, original) = installed_plugin(ECHO_WAT, Some(crate::pack::BindAccess::Read));
        let mut executor = PluginExecutor::new(Some(home.path().to_owned()));
        executor
            .call(&original, serde_json::json!({"attempt":"first"}))
            .await
            .unwrap();
        if case["restart"] == true {
            executor = PluginExecutor::new(Some(home.path().to_owned()));
        }
        let mut current = original.clone();
        if case["current"].is_null() {
            store::remove_record(home.path(), &original.namespace, &original.name).unwrap();
        } else {
            let snapshot = &case["current"];
            match snapshot["digest"].as_str().unwrap() {
                "artifact" => {}
                "replacement" => current.digest = format!("sha256:{}", "0".repeat(64)),
                other => panic!("unknown modeled artifact {other}"),
            }
            match snapshot["grant"].as_str().unwrap() {
                "sealed" => {}
                "changed" => current.granted = Some(crate::plugin::Manifold::sealed()),
                other => panic!("unknown modeled grant {other}"),
            }
            match snapshot["declaration"].as_str().unwrap() {
                "original" => {}
                "changed" => current.declaration.description.push_str(" updated"),
                other => panic!("unknown modeled declaration {other}"),
            }
            match snapshot["binding"].as_str().unwrap() {
                "original" => {}
                "changed" => current.instructions = Some("updated instructions".into()),
                other => panic!("unknown modeled binding {other}"),
            }
            store::write_record(home.path(), &current).unwrap();
        }
        let reusable = !case["current"].is_null() && executor.cached_admission_matches(&current);
        assert_eq!(reusable, case["reusable"].as_bool().unwrap(), "{case}");
        let input = serde_json::json!({"attempt":"retry"});
        let result = if case["bound"] == true {
            let directory = tempfile::tempdir().unwrap();
            let bound = crate::plugin::BoundDir::new(directory.path(), None).unwrap();
            executor.call_bound(&original, input.clone(), bound).await
        } else {
            executor.call(&original, input.clone()).await
        };
        assert_eq!(
            result.is_ok(),
            case["admitted"].as_bool().unwrap(),
            "{case}: {result:?}"
        );
        if let Ok(call) = result {
            assert_eq!(call.outcome.output["attempt"], input["attempt"], "{case}");
        }
    }
}

#[tokio::test]
async fn execution_receipt_hashes_canonical_values_and_keeps_tool_output_native() {
    use gents_protocol::plugin::{PluginExecutionVerdict, PluginFilesystemGrant};
    use sha2::{Digest, Sha256};
    let (home, record) = installed_echo();
    let executor = Arc::new(PluginExecutor::new(Some(home.path().to_owned())));
    let input = serde_json::json!({"z":2,"a":1});
    let (call, receipt) = executor
        .call_data_bound_with_receipt(&record, input.clone(), None, false)
        .await;
    assert_eq!(call.unwrap().outcome.output, input);
    let digest = format!("sha256:{:x}", Sha256::digest(br#"{"a":1,"z":2}"#));
    assert_eq!(receipt.input_digest, digest);
    assert_eq!(receipt.output_digest.as_deref(), Some(digest.as_str()));
    assert_eq!(receipt.artifact_digest, record.digest);
    assert_eq!(receipt.verdict, PluginExecutionVerdict::Success);
    let authority = receipt.authority.unwrap();
    assert_eq!(authority.filesystem, PluginFilesystemGrant::None);
    assert_eq!(authority.host_http, None);
    assert!(!authority.host_model);
    assert!(!authority.host_tools);
    assert!(receipt.limits.unwrap().memory_bytes > 0);

    let tool = PluginTool::resolve(executor, &tool_ref(None), None).unwrap();
    let dispatched = tool.call_with_receipt(input.to_string()).await;
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(&dispatched.result.unwrap()).unwrap(),
        input
    );
    assert_eq!(
        dispatched.plugin_receipt.unwrap().output_digest,
        Some(digest)
    );
}

#[tokio::test]
async fn execution_receipts_distinguish_refusal_bad_output_and_guest_trap() {
    use gents_protocol::plugin::PluginExecutionVerdict;
    for (wat, verdict) in [
        (
            crate::plugin::tests::constant_output_wat(b"invalid JSON"),
            PluginExecutionVerdict::BadOutput,
        ),
        (
            r#"(module (memory (export "memory") 1) (func (export "_start") unreachable))"#.into(),
            PluginExecutionVerdict::Failed,
        ),
    ] {
        let (home, record) = installed_plugin(&wat, None);
        let executor = PluginExecutor::new(Some(home.path().to_owned()));
        let (_, receipt) = executor
            .call_data_bound_with_receipt(&record, serde_json::json!({}), None, false)
            .await;
        assert_eq!(receipt.verdict, verdict);
        assert!(receipt.authority.is_some());
        assert!(receipt.limits.is_some());
        assert_eq!(receipt.output_digest, None);
        store::remove_record(home.path(), &record.namespace, &record.name).unwrap();
        let (result, receipt) = executor
            .call_data_bound_with_receipt(&record, serde_json::json!({}), None, false)
            .await;
        assert!(result.is_err());
        assert_eq!(receipt.verdict, PluginExecutionVerdict::AdmissionRefused);
        assert_eq!(receipt.authority, None);
        assert_eq!(receipt.limits, None);
        assert_eq!(receipt.output_digest, None);
    }
}

#[tokio::test]
async fn receipt_hashes_host_prepared_input_and_actual_http_grant() {
    use afterburner_core::manifold::NetAccess;
    use sha2::{Digest, Sha256};
    for (hosts, timeout) in [
        (vec![], None),
        (vec!["api.example.com".to_owned()], None),
        (vec!["api.example.com".to_owned()], Some(60_000)),
        (vec!["api.example.com".to_owned()], Some(1_000)),
    ] {
        let (home, mut record) = installed_echo();
        let mut granted = crate::plugin::Manifold::sealed();
        granted.net = NetAccess::OutboundHttp(Some(hosts.clone()));
        granted.http_timeout_ms = timeout;
        record.declaration.manifold = Some(serde_json::to_value(&granted).unwrap());
        record.granted = Some(granted);
        store::write_record(home.path(), &record).unwrap();
        let input = serde_json::json!({"state":"caller","http_results":{"forged":true},"value":1});
        let call = PluginExecutor::new(Some(home.path().to_owned()))
            .call(&record, input.clone())
            .await
            .unwrap();
        let effective = if hosts.is_empty() {
            input
        } else {
            serde_json::json!({"http_calls":true,"value":1})
        };
        let canonical = crate::workspace::canonical_json_string(&effective).unwrap();
        assert_eq!(
            call.receipt.input_digest,
            format!("sha256:{:x}", Sha256::digest(canonical.as_bytes()))
        );
        assert_eq!(call.outcome.output, effective);
        let http = call.receipt.authority.unwrap().host_http;
        assert_eq!(http.is_some(), !hosts.is_empty());
        if let Some(http) = http {
            assert_eq!(http.timeout_ms, Some(timeout.unwrap_or(30_000).min(30_000)));
        }
    }
}

#[tokio::test]
async fn corrupted_store_bytes_are_refused_and_never_cached() {
    let (home, record) = installed_echo();
    let hex = record.digest.strip_prefix("sha256:").unwrap();
    let mut bytes = store::read_bytes(home.path(), hex).unwrap();
    bytes.push(0);
    let path = home
        .path()
        .join("plugins")
        .join("store")
        .join(format!("{hex}.afb"));
    assert!(path.is_file(), "{}", path.display());
    std::fs::write(&path, &bytes).unwrap();
    let executor = PluginExecutor::new(Some(home.path().to_owned()));
    let error = executor
        .call(&record, serde_json::json!({}))
        .await
        .unwrap_err();
    assert!(
        format!("{}", ErrorChain(error.as_ref())).contains("do not match their digest"),
        "{}",
        ErrorChain(error.as_ref())
    );
    assert_eq!(executor.admitted_len(), 0);
}

#[tokio::test]
async fn bound_receipt_names_the_declared_field_not_the_host_path() {
    use gents_protocol::plugin::{PluginFilesystemGrant, PluginFilesystemScope};
    let (home, record) = installed_plugin(ECHO_WAT, Some(crate::pack::BindAccess::Read));
    let directory = tempfile::tempdir().unwrap();
    let bound = crate::plugin::BoundDir::new(directory.path(), None).unwrap();
    let call = PluginExecutor::new(Some(home.path().to_owned()))
        .call_bound(&record, serde_json::json!({}), bound)
        .await
        .unwrap();
    let authority = call.receipt.authority.unwrap();
    assert_eq!(
        authority.filesystem,
        PluginFilesystemGrant::ReadOnly(PluginFilesystemScope::BoundInput {
            field: "path".into()
        })
    );
    let encoded = serde_json::to_string(&authority).unwrap();
    let host = directory.path().canonicalize().unwrap();
    assert!(!encoded.contains(host.to_str().unwrap()), "{encoded}");
}

mod mounted {
    use super::*;
    use crate::pack::{write_home_install, BindAccess, HomePackInstall, InstalledVia, PackKind};
    use crate::plugin::tests::{open_and_copy_wat, ECHO_WAT};
    use crate::plugin::BoundDir;
    use gents_protocol::job::AssetMountDeclaration;
    use gents_protocol::plugin::{PluginFilesystemGrant, PluginFilesystemScope};

    /// A plugin declaring the `workflows` mount class, in a home whose one
    /// materialized pack ships `workflows/a.json`.
    pub(super) fn mounted_plugin(
        wat: &str,
        access: Option<BindAccess>,
    ) -> (tempfile::TempDir, InstalledPlugin) {
        let (home, mut record) = installed_plugin(wat, access);
        record.declaration.asset_mounts = vec![AssetMountDeclaration {
            class: "workflows".into(),
        }];
        store::write_record(home.path(), &record).unwrap();
        let hex = "a".repeat(64);
        let assets = format!("packs/.materialized/acme/content/{hex}");
        let dir = home.path().join(&assets).join("workflows");
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("a.json"), r#"{"mounted":true}"#).unwrap();
        write_home_install(
            home.path(),
            &HomePackInstall {
                coordinate: "acme/content".into(),
                version: "1.0.0".into(),
                digest: format!("sha256:{hex}"),
                kind: PackKind::Documents,
                assets,
                dependency_assets: vec![],
                plugins: vec![],
                ux: Vec::new(),
                installed_at: "2026-01-01T00:00:00Z".into(),
                installed_via: Some(InstalledVia::Operator),
            },
        )
        .unwrap();
        (home, record)
    }

    pub(super) fn executor(home: &tempfile::TempDir) -> PluginExecutor {
        PluginExecutor::new(Some(home.path().to_owned()))
    }

    #[tokio::test]
    async fn mounted_path_is_readable_and_a_write_to_it_fails() {
        let (home, record) = mounted_plugin(&open_and_copy_wat("a.json", 0), None);
        let read = executor(&home)
            .call(&record, serde_json::json!({}))
            .await
            .unwrap();
        assert_eq!(read.outcome.output, serde_json::json!({"mounted": true}));

        let (home, record) = mounted_plugin(&open_and_copy_wat("new.json", 1), None);
        let write = executor(&home)
            .call(&record, serde_json::json!({}))
            .await
            .unwrap();
        assert_eq!(
            write.outcome.verdict,
            crate::plugin::PluginVerdict::BadOutput
        );
        let mounted = home.path().join(format!(
            "packs/.materialized/acme/content/{}/workflows",
            "a".repeat(64)
        ));
        assert!(!mounted.join("new.json").exists());
    }

    #[tokio::test]
    async fn read_write_bind_with_mounts_is_refused_with_the_next_call() {
        let (home, record) = mounted_plugin(ECHO_WAT, Some(BindAccess::ReadWrite));
        let directory = tempfile::tempdir().unwrap();
        let bound = BoundDir::new(directory.path(), None).unwrap();
        let error = executor(&home)
            .call_bound(&record, serde_json::json!({}), bound)
            .await
            .unwrap_err();
        let message = format!("{}", ErrorChain(error.as_ref()));
        assert!(
            message.contains("this call writes; run it without mounted workflows"),
            "{message}"
        );
    }

    /// [`ECHO_WAT`] with a 4 KiB read buffer: a mount list outgrows its 256 bytes.
    pub(super) const ECHO_BIG_WAT: &str = r#"
      (module
        (import "wasi_snapshot_preview1" "fd_read"
          (func $fd_read (param i32 i32 i32 i32) (result i32)))
        (import "wasi_snapshot_preview1" "fd_write"
          (func $fd_write (param i32 i32 i32 i32) (result i32)))
        (memory (export "memory") 1)
        (func (export "_start")
          i32.const 8192  i32.const 0    i32.store
          i32.const 8196  i32.const 4096 i32.store
          i32.const 0
          i32.const 8192
          i32.const 1
          i32.const 8200
          call $fd_read
          drop
          i32.const 8196  i32.const 8200 i32.load  i32.store
          i32.const 1
          i32.const 8192
          i32.const 1
          i32.const 8204
          call $fd_write
          drop))
    "#;

    fn mounts_of(output: &serde_json::Value) -> &Vec<serde_json::Value> {
        output["asset_mounts"].as_array().expect("a mount list")
    }

    #[tokio::test]
    async fn non_job_call_receives_asset_mounts() {
        let (home, record) = mounted_plugin(ECHO_BIG_WAT, None);
        let call = executor(&home)
            .call(&record, serde_json::json!({"keep": 1}))
            .await
            .unwrap();
        assert_eq!(call.outcome.output["keep"], 1);
        let mounts = mounts_of(&call.outcome.output);
        assert_eq!(mounts.len(), 1);
        assert_eq!(mounts[0]["pack"], "acme/content");
        assert_eq!(mounts[0]["class"], "workflows");
        assert_eq!(mounts[0]["digest"], format!("sha256:{}", "a".repeat(64)));
        assert!(mounts[0]["path"].as_str().unwrap().ends_with("/workflows"));
    }

    #[tokio::test]
    async fn guest_supplied_asset_mounts_is_overwritten() {
        let (home, record) = mounted_plugin(ECHO_BIG_WAT, None);
        let call = executor(&home)
            .call(&record, serde_json::json!({"asset_mounts": "forged"}))
            .await
            .unwrap();
        let mounts = mounts_of(&call.outcome.output);
        assert_eq!(mounts.len(), 1);
        assert_eq!(mounts[0]["pack"], "acme/content");
    }

    #[tokio::test]
    async fn mounted_call_receipt_scope_is_mounted_without_host_paths() {
        let (home, record) = mounted_plugin(ECHO_WAT, Some(BindAccess::Read));
        let plain = executor(&home)
            .call(&record, serde_json::json!({}))
            .await
            .unwrap();
        let scope = |bound_input: Option<&str>| {
            PluginFilesystemGrant::ReadOnly(PluginFilesystemScope::Mounted {
                installed_roots: 0,
                mounts: 1,
                project_tier: false,
                bound_input: bound_input.map(str::to_owned),
            })
        };
        assert_eq!(plain.receipt.authority.unwrap().filesystem, scope(None));

        let directory = tempfile::tempdir().unwrap();
        let bound = BoundDir::new(directory.path(), None).unwrap();
        let bound_call = executor(&home)
            .call_bound(&record, serde_json::json!({}), bound)
            .await
            .unwrap();
        let authority = bound_call.receipt.authority.unwrap();
        assert_eq!(authority.filesystem, scope(Some("path")));
        let encoded = serde_json::to_string(&authority).unwrap();
        for host in [home.path(), directory.path()] {
            let host = host.canonicalize().unwrap();
            assert!(!encoded.contains(host.to_str().unwrap()), "{encoded}");
        }
    }
}

mod data_calls {
    use std::collections::BTreeMap;

    use gents_protocol::job::{DataQueryResponse, PluginDataDeclaration, RoundResult};
    use serde_json::{json, Value};

    use super::*;
    use crate::config_client::ConfigAccess;
    use crate::plugin::data::DataCaller;
    use crate::plugin::PluginVerdict;

    const NODE: &str = "did:key:z6Mknode";

    async fn node_with(rows: &[(&str, &str)]) -> Arc<crate::defra_node::EmbeddedNode> {
        let node = Arc::new(
            crate::defra_node::EmbeddedNode::builder()
                .build()
                .await
                .unwrap(),
        );
        node.add_schema("type Note { owner: String body: String }")
            .await
            .unwrap();
        for (owner, body) in rows {
            ConfigAccess::transact_local_write(&node, None, "data_calls_test", |txn| {
                let mutation = format!(
                    "mutation {{ add_Note(input: {{ owner: \"{owner}\", body: \"{body}\" }}) {{ _docID }} }}"
                );
                Box::pin(async move { txn.execute(&mutation).await })
            })
            .await
            .unwrap();
        }
        node
    }

    fn declaration(ops: &[&str]) -> PluginDataDeclaration {
        PluginDataDeclaration {
            collections: vec!["Note".into()],
            ops: ops.iter().map(|op| (*op).to_owned()).collect(),
            stamped: BTreeMap::new(),
        }
    }

    fn query(id: &str, query: Value) -> Value {
        let mut query = query;
        let object = query.as_object_mut().unwrap();
        object.entry("collection").or_insert(json!("Note"));
        object.entry("filter").or_insert(json!({}));
        object.entry("fields").or_insert(json!(["body"]));
        json!({"id": id, "query": query})
    }

    /// Runs a plugin that asks for `requests` once and echoes the answers.
    async fn ask(
        data: Option<PluginDataDeclaration>,
        node: &Arc<crate::defra_node::EmbeddedNode>,
        requests: Value,
    ) -> crate::plugin::executor::PluginCall {
        let canned = json!({"data_calls": {"requests": requests, "state": {"page": 1}}});
        let (home, mut record) = installed_plugin(
            &asking_plugin_wat("data_calls", "data_results", &canned, false),
            None,
        );
        record.declaration.data = data;
        store::write_record(home.path(), &record).unwrap();
        PluginExecutor::new(Some(home.path().to_owned()))
            .call_with_data(
                &record,
                json!({}),
                DataCaller::new(node.clone(), NODE).unwrap(),
            )
            .await
            .unwrap()
    }

    fn results_of(
        call: &crate::plugin::executor::PluginCall,
    ) -> BTreeMap<String, RoundResult<DataQueryResponse>> {
        assert_eq!(
            call.outcome.verdict,
            PluginVerdict::Success,
            "{}",
            call.outcome.diagnostics
        );
        serde_json::from_value(call.outcome.output["data_results"].clone()).unwrap()
    }

    fn bodies(answer: &RoundResult<DataQueryResponse>) -> Vec<String> {
        match answer {
            RoundResult::Result { result } => result
                .rows
                .iter()
                .map(|row| row["body"].as_str().unwrap().to_owned())
                .collect(),
            RoundResult::Error { error } => panic!("refused: {error}"),
        }
    }

    fn refusal(answer: &RoundResult<DataQueryResponse>) -> &str {
        match answer {
            RoundResult::Error { error } => error,
            RoundResult::Result { .. } => panic!("expected a refusal"),
        }
    }

    #[tokio::test]
    async fn data_calls_query_substitutes_caller_node_did() {
        let node = node_with(&[(NODE, "mine"), ("did:key:z6Mkother", "theirs")]).await;
        let call = ask(
            Some(declaration(&["query"])),
            &node,
            json!([query(
                "q",
                json!({"filter": {"owner": {"_eq": "$caller.node_did"}}, "limit": 10})
            )]),
        )
        .await;
        assert_eq!(bodies(&results_of(&call)["q"]), ["mine"]);
        assert_eq!(call.outcome.output["state"], json!({"page": 1}));
    }

    #[tokio::test]
    async fn data_calls_query_outside_the_grant_is_refused() {
        let node = node_with(&[(NODE, "mine")]).await;
        let call = ask(
            Some(declaration(&["query"])),
            &node,
            json!([
                query("other", json!({"collection": "Other", "limit": 1})),
                query("runtime", json!({"collection": "AgentRequest", "limit": 1})),
            ]),
        )
        .await;
        let answers = results_of(&call);
        assert!(refusal(&answers["other"]).contains("data.collections"));
        assert!(refusal(&answers["runtime"]).contains("data.collections"));

        let call = ask(
            Some(declaration(&["insert"])),
            &node,
            json!([query("q", json!({"limit": 1}))]),
        )
        .await;
        assert!(refusal(&results_of(&call)["q"]).contains("data.ops"));
    }

    #[tokio::test]
    async fn data_calls_query_with_a_bad_field_name_is_refused() {
        let node = node_with(&[(NODE, "mine")]).await;
        let call = ask(
            Some(declaration(&["query"])),
            &node,
            json!([
                query("field", json!({"fields": ["body } { owner"], "limit": 1})),
                query(
                    "filter",
                    json!({"filter": {"body } x": {"_eq": "a"}}, "limit": 1})
                ),
                query("unknown", json!({"fields": ["nope"], "limit": 1})),
            ]),
        )
        .await;
        let answers = results_of(&call);
        for id in ["field", "filter", "unknown"] {
            refusal(&answers[id]);
        }
    }

    #[tokio::test]
    async fn data_calls_query_caps_results_at_the_limit() {
        let node = node_with(&[(NODE, "a"), (NODE, "b"), (NODE, "c")]).await;
        let call = ask(
            Some(declaration(&["query"])),
            &node,
            json!([
                query("two", json!({"limit": 2})),
                query("zero", json!({"limit": 0})),
                query("many", json!({"limit": 257})),
            ]),
        )
        .await;
        let answers = results_of(&call);
        assert_eq!(bodies(&answers["two"]).len(), 2);
        assert!(refusal(&answers["zero"]).contains("limit"));
        assert!(refusal(&answers["many"]).contains("256"));
    }

    #[tokio::test]
    async fn data_calls_query_returns_rows_after_offset() {
        let node = node_with(&[(NODE, "a"), (NODE, "b"), (NODE, "c")]).await;
        let ordered = |id: &str, extra: Value| {
            let mut request = json!({"order": {"body": "ASC"}});
            request
                .as_object_mut()
                .unwrap()
                .extend(extra.as_object().unwrap().clone());
            query(id, request)
        };
        let call = ask(
            Some(declaration(&["query"])),
            &node,
            json!([
                ordered("window", json!({"offset": 1, "limit": 2})),
                ordered("wide", json!({"offset": 1, "limit": 256})),
                ordered("start", json!({"limit": 2})),
            ]),
        )
        .await;
        let answers = results_of(&call);
        assert_eq!(bodies(&answers["window"]), ["b", "c"]);
        assert_eq!(bodies(&answers["wide"]), ["b", "c"]);
        assert_eq!(bodies(&answers["start"]), ["a", "b"]);
    }

    #[tokio::test]
    async fn data_calls_is_not_offered_to_plugins_without_data() {
        let node = node_with(&[(NODE, "mine")]).await;
        let call = ask(None, &node, json!([query("q", json!({"limit": 1}))])).await;
        assert_eq!(call.outcome.verdict, PluginVerdict::Success);
        assert!(call.outcome.output.get("data_calls").is_none());
        assert!(call.outcome.output.get("data_results").is_none());

        let (home, mut record) = installed_echo();
        record.declaration.data = Some(declaration(&["query"]));
        store::write_record(home.path(), &record).unwrap();
        let call = PluginExecutor::new(Some(home.path().to_owned()))
            .call(&record, json!({}))
            .await
            .unwrap();
        assert!(call.outcome.output.get("data_calls").is_none());
    }

    #[tokio::test]
    async fn data_calls_answers_are_keyed_by_id_in_data_results() {
        let node = node_with(&[(NODE, "mine")]).await;
        let call = ask(
            Some(declaration(&["query"])),
            &node,
            json!([
                query("a", json!({"limit": 5})),
                query("b", json!({"collection": "Other", "limit": 5})),
            ]),
        )
        .await;
        let answers = results_of(&call);
        assert_eq!(answers.keys().collect::<Vec<_>>(), ["a", "b"]);
        assert_eq!(bodies(&answers["a"]), ["mine"]);
        assert!(matches!(answers["b"], RoundResult::Error { .. }));
    }

    #[tokio::test]
    async fn data_call_with_an_unknown_key_is_a_bad_output() {
        let node = node_with(&[(NODE, "mine")]).await;
        let mut request = query("a", json!({"limit": 1}));
        request["extra"] = json!(true);
        let call = ask(Some(declaration(&["query"])), &node, json!([request])).await;
        assert_eq!(call.outcome.verdict, PluginVerdict::BadOutput);
        assert!(
            call.outcome.diagnostics.contains("DataCall"),
            "{}",
            call.outcome.diagnostics
        );
    }
}

mod tool_writes {
    use std::collections::BTreeMap;

    use gents_protocol::job::{PluginDataDeclaration, StampedField};
    use serde_json::{json, Value};

    use super::*;
    use crate::config_client::ConfigAccess;
    use crate::tool_call_lifecycle::admission_fixture::published_spawn_parent;

    const NODE: &str = "did:key:z6Mknode";

    fn grant(
        collections: &[&str],
        stamped: &[(&str, StampedField)],
        fields: &[(&str, &[&str])],
    ) -> (PluginDataDeclaration, BTreeMap<String, Vec<String>>) {
        (
            PluginDataDeclaration {
                collections: collections.iter().map(|c| (*c).to_owned()).collect(),
                ops: vec!["insert".to_owned()],
                stamped: stamped
                    .iter()
                    .map(|(field, stamp)| ((*field).to_owned(), *stamp))
                    .collect(),
            },
            fields
                .iter()
                .map(|(collection, names)| {
                    (
                        (*collection).to_owned(),
                        names.iter().map(|n| (*n).to_owned()).collect(),
                    )
                })
                .collect(),
        )
    }

    fn insert(collection: &str, body: &str) -> Value {
        json!({"op": "insert", "collection": collection, "fields": {"body": body}})
    }

    struct Finished {
        node: Arc<crate::defra_node::EmbeddedNode>,
        path: std::path::PathBuf,
        tool_row: Value,
        result: Result<String, String>,
    }

    impl Finished {
        async fn bodies(&self, collection: &str) -> Vec<String> {
            let rows = ConfigAccess::local(self.node.clone())
                .execute(&format!("{{ {collection} {{ body }} }}"))
                .await
                .unwrap();
            rows["data"][collection]
                .as_array()
                .unwrap()
                .iter()
                .map(|row| row["body"].as_str().unwrap().to_owned())
                .collect()
        }

        async fn finish(self) {
            self.node.shutdown().await;
            std::fs::remove_dir_all(self.path).unwrap();
        }
    }

    /// Runs the echo plugin, which returns `output` as its result, as the model
    /// tool of one admitted tool call, then terminalizes that call with the
    /// result the tool returned.
    async fn run(
        name: &str,
        data: Option<(PluginDataDeclaration, BTreeMap<String, Vec<String>>)>,
        output: Value,
    ) -> Finished {
        run_with_terminal(name, data, output, false).await
    }

    /// `transient_terminal` makes the first terminal transaction fail with a
    /// conflict the transaction owner replays.
    async fn run_with_terminal(
        name: &str,
        data: Option<(PluginDataDeclaration, BTreeMap<String, Vec<String>>)>,
        output: Value,
        transient_terminal: bool,
    ) -> Finished {
        let (node, path, mut call) = published_spawn_parent(name).await;
        node.add_schema(
            "type Stamped { body: String job_row_doc_id: String } type Plain { body: String }",
        )
        .await
        .unwrap();
        let (home, mut record) = installed_echo();
        if let Some((declaration, fields)) = data {
            record.declaration.data = Some(declaration);
            record.data_collection_fields = fields;
        }
        store::write_record(home.path(), &record).unwrap();
        let executor = Arc::new(PluginExecutor::new(Some(home.path().to_owned())));
        let tool = PluginTool::resolve(executor, &tool_ref(Some(&record.digest)), None)
            .unwrap()
            .with_data_node(node.clone());
        let doc_id = call.doc_id().unwrap().to_owned();
        let registry = gents_loop::live_output::LiveToolOutputRegistry::default();
        let writer = registry.writer_for(doc_id.clone()).await;
        writer
            .record_deferred_writes(gents_loop::live_output::DeferredToolWrites::new(
                "independent host observation",
            ))
            .await
            .unwrap();

        let dispatched = gents_loop::tool_call_lifecycle::runtime::scope_tool_request_identity(
            None,
            Some(NODE.into()),
            None,
            None,
            gents_loop::tool_call_lifecycle::runtime::scope_request_tool_execution_with_trigger_context(
                None,
                tokio_util::sync::CancellationToken::new(),
                None,
                Some(writer),
                Some(format!("session-{name}")),
                None,
                BTreeMap::new(),
                false,
                tool.call_with_receipt(output.to_string()),
            ),
        )
        .await;
        if let Some(writes) = dispatched.deferred_writes {
            registry
                .writer_for(doc_id.clone())
                .await
                .record_deferred_writes(writes)
                .await
                .unwrap();
        }
        let result = dispatched.result.map_err(|error| error.to_string());
        let pending = registry.deferred_writes(&doc_id).await.unwrap();
        assert_eq!(
            pending.downcast_ref::<&'static str>(),
            Some(&"independent host observation"),
        );
        call.stage_deferred_writes(Some(pending));
        if transient_terminal {
            crate::tool_call_lifecycle::delivery::fail_next_terminal_for_test(&doc_id);
        }
        match &result {
            Ok(text) => call.complete(text).await.unwrap(),
            Err(text) => call
                .fail(
                    text,
                    crate::tool_call_lifecycle::FailureClass::ToolReturnedError,
                )
                .await
                .unwrap(),
        }
        assert!(
            !transient_terminal
                || !crate::tool_call_lifecycle::delivery::terminal_failure_pending_for_test(
                    &doc_id
                ),
            "the first terminal transaction must have failed before the replay"
        );
        let tool_row = ConfigAccess::local(node.clone())
            .execute(&format!(
                r#"{{ AgentToolCall(docID: "{doc_id}") {{ lifecycle_state }} }}"#
            ))
            .await
            .unwrap()["data"]["AgentToolCall"][0]
            .clone();
        Finished {
            node,
            path,
            tool_row,
            result,
        }
    }

    #[tokio::test]
    async fn data_plugin_tool_writes_commit_with_its_tool_row() {
        let data = grant(&["Plain"], &[], &[("Plain", &["body"])]);
        let done = run(
            "tool-writes-commit",
            Some(data),
            json!({"response": "ok", "writes": [insert("Plain", "x")]}),
        )
        .await;
        assert_eq!(done.tool_row["lifecycle_state"], "completed");
        assert_eq!(done.bodies("Plain").await, ["x"]);
        let text = done.result.as_ref().unwrap();
        assert!(text.contains("\"response\":\"ok\""), "{text}");
        assert!(
            !text.contains("\"writes\""),
            "the model sees the output, not the writes: {text}"
        );
        done.finish().await;
    }

    #[tokio::test]
    async fn replayed_terminal_commits_the_writes_that_travel_with_the_call_once() {
        let data = grant(&["Plain"], &[], &[("Plain", &["body"])]);
        let done = run_with_terminal(
            "tool-writes-replay",
            Some(data),
            json!({"response": "ok", "writes": [insert("Plain", "x")]}),
            true,
        )
        .await;
        assert_eq!(done.tool_row["lifecycle_state"], "completed");
        assert_eq!(done.bodies("Plain").await, ["x"]);
        done.finish().await;
    }

    #[tokio::test]
    async fn refused_plugin_tool_write_fails_the_tool_and_commits_no_document() {
        let data = grant(&["Plain"], &[], &[("Plain", &["body"])]);
        let done = run(
            "tool-writes-refused",
            Some(data),
            json!({"writes": [insert("Plain", "x"), insert("Elsewhere", "y")]}),
        )
        .await;
        assert_eq!(done.tool_row["lifecycle_state"], "failed");
        assert!(done.bodies("Plain").await.is_empty());
        done.finish().await;
    }

    #[tokio::test]
    async fn non_job_stamp_context_refuses_job_only_stamps() {
        let data = grant(
            &["Stamped"],
            &[("job_row_doc_id", StampedField::JobRowDocId)],
            &[("Stamped", &["body", "job_row_doc_id"])],
        );
        let done = run(
            "tool-writes-job-stamp",
            Some(data),
            json!({"writes": [insert("Stamped", "x")]}),
        )
        .await;
        assert_eq!(done.tool_row["lifecycle_state"], "failed");
        assert!(done.bodies("Stamped").await.is_empty());
        done.finish().await;
    }

    #[tokio::test]
    async fn plugin_without_data_is_unaffected() {
        let output = json!({"writes": [insert("Plain", "x")]});
        let done = run("tool-writes-no-data", None, output.clone()).await;
        assert_eq!(done.tool_row["lifecycle_state"], "completed");
        assert!(done.bodies("Plain").await.is_empty());
        let text: Value = serde_json::from_str(done.result.as_ref().unwrap()).unwrap();
        assert_eq!(text["writes"], output["writes"]);
        done.finish().await;
    }

    #[tokio::test]
    async fn stamp_scoped_to_written_collection() {
        let data = || {
            grant(
                &["Stamped", "Plain"],
                &[("job_row_doc_id", StampedField::JobRowDocId)],
                &[
                    ("Stamped", &["body", "job_row_doc_id"]),
                    ("Plain", &["body"]),
                ],
            )
        };
        let plain = run(
            "tool-writes-scope-plain",
            Some(data()),
            json!({"writes": [insert("Plain", "x")]}),
        )
        .await;
        assert_eq!(plain.tool_row["lifecycle_state"], "completed");
        assert_eq!(plain.bodies("Plain").await, ["x"]);
        plain.finish().await;
        let stamped = run(
            "tool-writes-scope-stamped",
            Some(data()),
            json!({"writes": [insert("Stamped", "x")]}),
        )
        .await;
        assert_eq!(stamped.tool_row["lifecycle_state"], "failed");
        stamped.finish().await;
    }
}

mod job_calls {
    use std::collections::BTreeMap;

    use gents_protocol::job::{
        ChildOutputResponse, JobResultResponse, JobState, PluginJobDeclaration, RoundResult,
    };
    use serde_json::{json, Value};

    use super::*;
    use crate::background_tools::ProcessControlScope;
    use crate::config_client::ConfigAccess;
    use crate::plugin::data::StampContext;
    use crate::plugin::executor::PluginCall;
    use crate::plugin::job_calls::JobCaller;
    use crate::plugin::PluginVerdict;
    use crate::plugin_job::driver::{settle_plugin_job, Settlement};
    use crate::plugin_job::input::tests::{completed_request, open};
    use crate::plugin_job::keys::{job_session_id, job_spawn_key};
    use crate::tool_call_lifecycle::admission_fixture::{
        fixture_job_pin, published_admission, PublishedAdmission, PublishedAdmissionOptions,
    };
    use crate::tool_call_lifecycle::AwaitMode;

    const CHAIN: &str = "chain-row";

    fn tool_output() -> StampContext {
        StampContext::ToolOutput {
            node_did: "did:test:test".into(),
            session_id: "session".into(),
            request_doc_id: "request".into(),
            tool_call_doc_id: "tool".into(),
        }
    }

    fn scope(session_id: &str) -> ProcessControlScope {
        ProcessControlScope {
            request_id: "caller".into(),
            session_id: session_id.into(),
            node_did: "did:test:test".into(),
            requester_did: None,
        }
    }

    /// Runs a plugin of `team/plugin` that asks for `requests` once and echoes
    /// the answers.
    async fn run(requests: Value, declared: bool, caller: JobCaller) -> PluginCall {
        let canned = json!({"job_calls": {"requests": requests}});
        let (home, mut record) = installed_plugin(
            &asking_plugin_wat("job_calls", "job_results", &canned, false),
            None,
        );
        record.declaration.job = PluginJobDeclaration(declared);
        store::write_record(home.path(), &record).unwrap();
        PluginExecutor::new(Some(home.path().to_owned()))
            .call_with_jobs(&record, json!({}), caller)
            .await
            .unwrap()
    }

    fn answers<T>(call: &PluginCall) -> BTreeMap<String, RoundResult<T>>
    where
        T: serde::de::DeserializeOwned,
    {
        assert_eq!(
            call.outcome.verdict,
            PluginVerdict::Success,
            "{}",
            call.outcome.diagnostics
        );
        serde_json::from_value(call.outcome.output["job_results"].clone()).unwrap()
    }

    fn child_output(id: &str, slots: &[&str], offset: u64, limit: u64) -> Value {
        json!({"id": id, "child_output": {"slots": slots, "offset": offset, "limit": limit}})
    }

    fn result_request(id: &str, row_doc_id: &str, offset: u64, limit: u64) -> Value {
        json!({"id": id, "result": {"row_doc_id": row_doc_id, "offset": offset, "limit": limit}})
    }

    async fn child_node(text: &str) -> Arc<crate::defra_node::EmbeddedNode> {
        let node = open().await;
        completed_request(
            &node,
            CHAIN,
            "done",
            &job_session_id("root", "slot-a", 0),
            &job_spawn_key("root", "slot-a", 0),
            text,
        )
        .await;
        node
    }

    /// A background job row pinned to `plugin_ref`.
    async fn job_row(name: &str, plugin_ref: &str) -> PublishedAdmission {
        let mut admission = published_admission(PublishedAdmissionOptions {
            name: name.to_owned(),
            real_identity: true,
            await_mode: AwaitMode::Background,
            job_pin: true,
            ..Default::default()
        })
        .await
        .unwrap();
        assert!(admission
            .tool
            .publish_background_receipt("started")
            .await
            .unwrap());
        let mutation = format!(
            r#"mutation($pin: JSON) {{ update_AgentToolCall(filter: {{ _docID: {{ _eq: "{}" }} }}, input: {{ job_pin: $pin }}) {{ _docID }} }}"#,
            admission.tool.doc_id().unwrap()
        );
        let pin = json!({"pin": gents_protocol::job::JobPin {
            plugin_ref: plugin_ref.to_owned(),
            ..fixture_job_pin()
        }});
        ConfigAccess::transact_local_write(&admission.node, None, "test.job_calls.pin", |txn| {
            let (mutation, pin) = (&mutation, &pin);
            Box::pin(async move { txn.execute_with_variables(mutation, pin).await })
        })
        .await
        .unwrap();
        admission
    }

    async fn settle(admission: &mut PublishedAdmission, text: &str) {
        settle_plugin_job(&mut admission.tool, Settlement::Result(json!(text)))
            .await
            .unwrap();
    }

    async fn finish(admission: PublishedAdmission) {
        admission.node.shutdown().await;
        let _ = std::fs::remove_dir_all(admission.path);
    }

    fn row_scope(admission: &PublishedAdmission) -> ProcessControlScope {
        ProcessControlScope {
            request_id: "caller".into(),
            session_id: admission.tool.session_id().into(),
            node_did: admission.tool.node_did().into(),
            requester_did: admission.tool.requester_did().map(str::to_owned),
        }
    }

    fn page_of(call: &PluginCall) -> JobResultResponse {
        match answers::<JobResultResponse>(call).remove("r").unwrap() {
            RoundResult::Result { result } => result,
            RoundResult::Error { error } => panic!("refused: {error}"),
        }
    }

    fn refusal(call: &PluginCall) -> String {
        match answers::<JobResultResponse>(call).remove("r").unwrap() {
            RoundResult::Error { error } => error,
            RoundResult::Result { .. } => panic!("expected a refusal"),
        }
    }

    #[tokio::test]
    async fn job_calls_child_output_pages_terminal_text_capped_at_result_bytes() {
        let node = child_node("twelve bytes").await;
        let caller = JobCaller::new(node.clone(), scope("session"), tool_output())
            .with_children(vec![CHAIN.into()], 5);
        let call = run(
            json!([
                child_output("head", &["slot-a"], 0, 100),
                child_output("middle", &["slot-a"], 5, 100),
                child_output("tail", &["slot-a"], 10, 100),
                child_output("short", &["slot-a"], 0, 3),
                child_output("missing", &["nope"], 0, 100),
            ]),
            true,
            caller,
        )
        .await;
        let answers = answers::<ChildOutputResponse>(&call);
        let slot = |id: &str| match &answers[id] {
            RoundResult::Result { result } => result.slots["slot-a"].clone(),
            RoundResult::Error { error } => panic!("refused: {error}"),
        };
        assert_eq!(
            (slot("head").text, slot("head").truncated),
            ("twelv".into(), true)
        );
        assert_eq!(
            (slot("middle").text, slot("middle").truncated),
            ("e byt".into(), true)
        );
        assert_eq!(
            (slot("tail").text, slot("tail").truncated),
            ("es".into(), false)
        );
        assert_eq!(
            (slot("short").text, slot("short").truncated),
            ("twe".into(), true)
        );
        assert!(
            matches!(&answers["missing"], RoundResult::Error { error } if error.contains("nope"))
        );
        node.shutdown().await;
    }

    #[tokio::test]
    async fn job_calls_result_pages_a_result_over_4000_bytes_byte_identically() {
        let text = "0123456789abcdef".repeat(563);
        assert!(text.len() > 4000);
        let mut admission = job_row("job-calls-result-pages", "team/plugin").await;
        settle(&mut admission, &text).await;
        let row = admission.tool.doc_id().unwrap().to_owned();
        let mut paged = String::new();
        loop {
            let caller =
                JobCaller::new(admission.node.clone(), row_scope(&admission), tool_output());
            let offset = paged.len() as u64;
            let call = run(
                json!([result_request("r", &row, offset, 4000)]),
                true,
                caller,
            )
            .await;
            let page = page_of(&call);
            assert_eq!(page.row_doc_id, row);
            assert_eq!(page.state, JobState::Completed);
            assert_eq!((page.offset, page.total_bytes), (offset, text.len() as u64));
            assert!(page.result.len() <= 4000);
            paged.push_str(&page.result);
            if !page.truncated {
                break;
            }
        }
        assert_eq!(paged, text);
        finish(admission).await;
    }

    #[tokio::test]
    async fn job_calls_result_refuses_a_row_outside_scope_or_of_another_plugin() {
        let mut ours = job_row("job-calls-result-ours", "team/plugin").await;
        settle(&mut ours, "done").await;
        let row = ours.tool.doc_id().unwrap().to_owned();
        let outsider = JobCaller::new(ours.node.clone(), scope("another-session"), tool_output());
        let call = run(json!([result_request("r", &row, 0, 100)]), true, outsider).await;
        assert!(refusal(&call).contains("scope"), "{}", refusal(&call));
        let missing = JobCaller::new(ours.node.clone(), row_scope(&ours), tool_output());
        let call = run(
            json!([result_request("r", "no-such-row", 0, 100)]),
            true,
            missing,
        )
        .await;
        assert!(refusal(&call).contains("scope"), "{}", refusal(&call));
        finish(ours).await;

        let mut theirs = job_row("job-calls-result-theirs", "other/plugin").await;
        settle(&mut theirs, "done").await;
        let row = theirs.tool.doc_id().unwrap().to_owned();
        let caller = JobCaller::new(theirs.node.clone(), row_scope(&theirs), tool_output());
        let call = run(json!([result_request("r", &row, 0, 100)]), true, caller).await;
        assert!(
            refusal(&call).contains("another plugin"),
            "{}",
            refusal(&call)
        );
        finish(theirs).await;
    }

    #[tokio::test]
    async fn job_calls_result_on_a_running_row_is_not_terminal_with_the_next_call() {
        let admission = job_row("job-calls-result-running", "team/plugin").await;
        let row = admission.tool.doc_id().unwrap().to_owned();
        let caller = JobCaller::new(admission.node.clone(), row_scope(&admission), tool_output());
        let call = run(json!([result_request("r", &row, 0, 100)]), true, caller).await;
        let error = refusal(&call);
        assert!(error.starts_with("not_terminal"), "{error}");
        assert!(error.contains("status"), "{error}");
        finish(admission).await;
    }

    #[tokio::test]
    async fn callback_invocation_is_not_offered_job_calls_result() {
        let node = open().await;
        let callback = StampContext::Callback {
            node_did: "did:test:test".into(),
        };
        let requests = json!([result_request("r", "row", 0, 100)]);
        let call = run(
            requests.clone(),
            true,
            JobCaller::new(node.clone(), scope("session"), callback),
        )
        .await;
        assert_eq!(call.outcome.verdict, PluginVerdict::Success);
        assert!(call.outcome.output.get("job_calls").is_none());
        assert!(call.outcome.output.get("job_results").is_none());

        let call = run(
            requests,
            false,
            JobCaller::new(node.clone(), scope("session"), tool_output()),
        )
        .await;
        assert!(call.outcome.output.get("job_calls").is_none());
        node.shutdown().await;
    }

    #[tokio::test]
    async fn job_calls_answers_are_keyed_by_id_in_job_results() {
        let mut admission = job_row("job-calls-keyed", "team/plugin").await;
        settle(&mut admission, "stored").await;
        let row = admission.tool.doc_id().unwrap().to_owned();
        completed_request(
            &admission.node,
            CHAIN,
            "done",
            &job_session_id("root", "slot-a", 0),
            &job_spawn_key("root", "slot-a", 0),
            "child text",
        )
        .await;
        let caller = JobCaller::new(admission.node.clone(), row_scope(&admission), tool_output())
            .with_children(vec![CHAIN.into()], 64);
        let call = run(
            json!([
                child_output("c", &["slot-a"], 0, 100),
                result_request("r", &row, 0, 100)
            ]),
            true,
            caller,
        )
        .await;
        assert_eq!(call.outcome.verdict, PluginVerdict::Success);
        let output = &call.outcome.output["job_results"];
        assert_eq!(
            output.as_object().unwrap().keys().collect::<Vec<_>>(),
            ["c", "r"]
        );
        let RoundResult::Result { result } =
            serde_json::from_value::<RoundResult<ChildOutputResponse>>(output["c"].clone())
                .unwrap()
        else {
            panic!("child_output refused: {output}");
        };
        assert_eq!(result.slots.len(), 1);
        assert_eq!(result.slots["slot-a"].text, "child text");
        assert!(output["c"]["result"]["slots"]["slot-a"]
            .get("truncated")
            .is_none());
        let RoundResult::Result { result } =
            serde_json::from_value::<RoundResult<JobResultResponse>>(output["r"].clone()).unwrap()
        else {
            panic!("result refused: {output}");
        };
        assert_eq!(result.result, "stored");

        let caller = JobCaller::new(admission.node.clone(), row_scope(&admission), tool_output());
        let mut unknown = result_request("r", &row, 0, 1);
        unknown["extra"] = json!(true);
        let call = run(json!([unknown]), true, caller).await;
        assert_eq!(call.outcome.verdict, PluginVerdict::BadOutput);
        assert!(
            call.outcome.diagnostics.contains("JobCall"),
            "{}",
            call.outcome.diagnostics
        );
        finish(admission).await;
    }
}

mod catalog {
    use gents_protocol::job::{
        CatalogEntry, CatalogSource, PluginCatalogDeclaration, PluginDataDeclaration,
    };
    use serde_json::{json, Value};

    use super::mounted::{executor, mounted_plugin, ECHO_BIG_WAT};
    use super::*;
    use crate::config_client::ConfigAccess;
    use crate::plugin::catalog;
    use crate::plugin::data::DataCaller;
    use crate::plugin::tests::{constant_output_wat, filled_output_wat};

    const NODE: &str = "did:key:z6Mknode";

    async fn test_node() -> Arc<crate::defra_node::EmbeddedNode> {
        let node = Arc::new(
            crate::defra_node::EmbeddedNode::builder()
                .build()
                .await
                .unwrap(),
        );
        node.add_schema("type FixtureNote { body: String }")
            .await
            .unwrap();
        node
    }

    fn declare_catalog(home: &tempfile::TempDir, record: &mut InstalledPlugin) {
        record.declaration.catalog = Some(PluginCatalogDeclaration {
            argv: vec!["catalog".into()],
        });
        store::write_record(home.path(), record).unwrap();
    }

    fn plugin_ref(record: &InstalledPlugin) -> PluginToolRef {
        serde_json::from_value(json!({
            "plugin": format!("{}/{}", record.namespace, record.name),
            "digest": record.digest,
        }))
        .unwrap()
    }

    async fn list(
        home: &tempfile::TempDir,
        record: &InstalledPlugin,
        node: &Arc<crate::defra_node::EmbeddedNode>,
    ) -> anyhow::Result<Vec<CatalogEntry>> {
        catalog(
            &executor(home),
            &plugin_ref(record),
            &DataCaller::new(node.clone(), NODE).unwrap(),
        )
        .await
    }

    #[tokio::test]
    async fn catalog_entries_decode() {
        let wat = constant_output_wat(
            br#"[{"name":"triage","description":"Sort issues","when_to_use":"new issues","source":"pack","path":"workflows/triage.rhai"}]"#,
        );
        let (home, mut record) = installed_plugin(&wat, None);
        declare_catalog(&home, &mut record);
        let entries = list(&home, &record, &test_node().await).await.unwrap();
        assert_eq!(
            entries,
            [CatalogEntry {
                name: "triage".into(),
                description: "Sort issues".into(),
                when_to_use: "new issues".into(),
                source: CatalogSource::Pack,
                path: Some("workflows/triage.rhai".into()),
            }]
        );
    }

    #[tokio::test]
    async fn catalog_input_carries_argv_and_asset_mounts_never_job_step_input() {
        let (home, mut record) = mounted_plugin(ECHO_BIG_WAT, None);
        declare_catalog(&home, &mut record);
        let error = list(&home, &record, &test_node().await).await.unwrap_err();
        let message = format!("{}", ErrorChain(error.as_ref()));
        assert!(message.contains(r#""argv":["catalog"]"#), "{message}");
        assert!(message.contains(r#""asset_mounts""#), "{message}");
        for key in ["protocol", "job", "caller", "ceiling"] {
            assert!(
                !message.contains(&format!("\"{key}\":")),
                "{key}: {message}"
            );
        }
    }

    #[tokio::test]
    async fn catalog_offers_read_only_data_calls_and_no_job_calls_or_tool_calls() {
        let node = test_node().await;
        let data = Some(PluginDataDeclaration {
            collections: vec!["FixtureNote".into()],
            ops: vec!["insert".into()],
            stamped: Default::default(),
        });
        let (home, mut record) = installed_plugin(ECHO_BIG_WAT, None);
        record.declaration.data = data.clone();
        declare_catalog(&home, &mut record);
        let message = format!(
            "{}",
            ErrorChain(list(&home, &record, &node).await.unwrap_err().as_ref())
        );
        assert!(message.contains(r#""data_calls":true"#), "{message}");
        assert!(!message.contains("job_calls"), "{message}");
        assert!(!message.contains("tool_calls"), "{message}");

        let wat = constant_output_wat(
            br#"{"writes":[{"op":"insert","collection":"FixtureNote","fields":{}}]}"#,
        );
        let (home, mut record) = installed_plugin(&wat, None);
        record.declaration.data = data;
        declare_catalog(&home, &mut record);
        list(&home, &record, &node).await.unwrap_err();
        let rows = ConfigAccess::local(node.clone())
            .execute("{ FixtureNote { body } }")
            .await
            .unwrap();
        assert_eq!(rows["data"]["FixtureNote"], Value::Array(vec![]));
    }

    #[tokio::test]
    async fn catalog_output_over_256_kib_is_an_error_naming_the_plugin() {
        let wat = filled_output_wat(
            br#"[{"name":"x","description":""#,
            262_144,
            br#"","when_to_use":"w","source":"pack"}]"#,
        );
        let (home, mut record) = installed_plugin(&wat, None);
        declare_catalog(&home, &mut record);
        let message = format!(
            "{}",
            ErrorChain(
                list(&home, &record, &test_node().await)
                    .await
                    .unwrap_err()
                    .as_ref()
            )
        );
        assert!(
            message.contains(&format!("{}/{}", record.namespace, record.name)),
            "{message}"
        );
        assert!(message.contains("at most 262144"), "{message}");
    }

    #[tokio::test]
    async fn catalog_without_a_declaration_returns_the_next_call() {
        let (home, record) = installed_plugin(ECHO_BIG_WAT, None);
        let message = format!(
            "{}",
            ErrorChain(
                list(&home, &record, &test_node().await)
                    .await
                    .unwrap_err()
                    .as_ref()
            )
        );
        assert!(
            message.contains(&format!("{}/{}", record.namespace, record.name)),
            "{message}"
        );
        assert!(message.contains("declare `catalog`"), "{message}");
    }
}
