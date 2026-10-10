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
    };
    store::write_record(home.path(), &record).unwrap();
    (home, record)
}

/// A plugin that reads its input and, while it is offered `service`
/// (`"<service>":true` in its input), writes `canned` instead of a result:
/// once (until `results` arrives) or forever. Otherwise, or once satisfied, it
/// echoes its input as the result.
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
    store::write_record(home.path(), &record).unwrap();
    let call = executor.call(&record, serde_json::json!(2)).await.unwrap();
    assert_eq!(call.outcome.output, serde_json::json!(2));
    assert_eq!(
        executor.admitted_len(),
        1,
        "the new grant replaces the old admission"
    );
}

/// A caller holds the record it resolved, and the artifact stays admitted in
/// memory, yet a call after `gents plugin remove` fails closed.
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
        format!("{error:#}").contains("is not installed"),
        "{error:#}"
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

    /// One `read_write` plugin with a write field serves readers and writers
    /// (#2301): a call that does not set `output` asks for `read`, so it binds
    /// under a read-only folder and runs read-only even where writing is
    /// allowed; a call that sets it asks for `read_write`.
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

        let read = format!("{:#}", call(&reader, false).await.unwrap_err());
        assert!(
            read.contains("did not return a result") && !read.contains("dirs add"),
            "a reading call binds under a read-only folder and its sandbox refuses the write: {read}"
        );
        let escalation = format!("{:#}", call(&reader, true).await.unwrap_err());
        for expected in [
            "allowed read-only",
            r#"sets "output""#,
            "--access read_write",
            r#"call again without "output""#,
        ] {
            assert!(escalation.contains(expected), "{escalation}");
        }
        assert!(!reader.join("out.json").exists());
        let both = format!(
            "{:#}",
            tool.call(
                serde_json::json!({ "path": reader, "output": "out.json", "delete": true })
                    .to_string()
            )
            .await
            .unwrap_err()
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
            .call_data_bound(&fx.record, input.clone(), Some(&work), false)
            .await
            .unwrap();
        assert_eq!(call.outcome.output, serde_json::json!({ "read": true }));
        let error = executor
            .call_data_bound(&fx.record, input, None, false)
            .await
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("gents plugin dirs add"),
            "{error:#}"
        );
    }

    /// A directory authorized under one declaration is never handed to a
    /// reinstall that declares the binding differently.
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
        assert!(format!("{error:#}").contains("reinstalled"), "{error:#}");
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
async fn execution_receipts_distinguish_refusal_bad_output_and_trap() {
    use gents_protocol::plugin::PluginExecutionVerdict;
    for (wat, verdict) in [
        (
            crate::plugin::tests::constant_output_wat(b"invalid JSON"),
            PluginExecutionVerdict::BadOutput,
        ),
        (
            r#"(module (memory (export "memory") 1) (func (export "_start") unreachable))"#.into(),
            PluginExecutionVerdict::ExecutionError,
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
