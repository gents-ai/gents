use gents_pack::plugin::test_fixtures::ECHO_WAT;

fn job_tool_ref(job: Option<serde_json::Value>) -> gents_documents::PluginToolRef {
    gents_documents::PluginToolRef {
        plugin: "team/plugin".into(),
        digest: None,
        tool_calls: false,
        input_fields: Vec::new(),
        job: job.map(|job| serde_json::from_value(job).unwrap()),
    }
}

#[test]
fn job_block_on_a_plugin_without_job_true_is_refused() {
    let (home, record) = gents_pack::plugin::test_fixtures::installed_echo();
    assert!(!record.declaration.job.0);
    let executor = std::sync::Arc::new(crate::plugin::executor::PluginExecutor::new(Some(
        home.path().to_owned(),
    )));
    let block = serde_json::json!({"agent_targets": ["worker"]});
    let error =
        crate::plugin::tool::PluginTool::resolve(executor, &job_tool_ref(Some(block)), None)
            .err()
            .expect("a job block needs a plugin that declares job dispatch");
    assert_eq!(
        format!("{error:#}"),
        "plugin team/plugin does not declare job dispatch; install a pack whose plugin sets `job: true`"
    );
}

fn job_resolutions() -> (
    tempfile::TempDir,
    Vec<(
        gents_documents::PluginToolRef,
        Option<gents_pack::plugin::store::InstalledPlugin>,
    )>,
) {
    let (home, plain) = gents_pack::plugin::test_fixtures::installed_echo();
    let mut job_capable = plain.clone();
    job_capable.declaration.job = gents_protocol::job::PluginJobDeclaration(true);
    let mut not_job = job_tool_ref(None);
    not_job.plugin = "team/plain".into();
    let mut not_installed = job_tool_ref(None);
    not_installed.plugin = "team/missing".into();
    let resolutions = vec![
        (job_tool_ref(None), Some(job_capable)),
        (not_job, Some(plain)),
        (not_installed, None),
    ];
    (home, resolutions)
}

#[test]
fn job_tools_are_bound_only_for_job_capable_plugins() {
    let (_home, resolutions) = job_resolutions();
    let job_tools = crate::background::job::JobBinding::bind_all(&resolutions);
    assert_eq!(
        job_tools.keys().collect::<Vec<_>>(),
        vec!["plugin"],
        "only the entry whose installed plugin declares job: true binds"
    );
    let binding = &job_tools["plugin"];
    assert_eq!(binding.tool_name, "plugin");
    assert_eq!(binding.plugin, job_tool_ref(None));
    assert_eq!(
        binding.settings, None,
        "an absent block binds as the empty ceiling"
    );
}

#[test]
fn plugin_child_call_naming_a_job_tool_uses_the_selected_surface() {
    let (_home, resolutions) = job_resolutions();
    let job_tools: std::collections::BTreeSet<String> =
        crate::background::job::JobBinding::bind_all(&resolutions)
            .into_keys()
            .collect();
    assert!(crate::plugin::tool_calls::supported("plugin", &job_tools));
    assert!(crate::plugin::tool_calls::supported("plain", &job_tools));
}

#[test]
fn plugin_job_keys_reexports_the_encoders_and_the_decoder() {
    use gents_protocol::job::keys::{
        decode_job_retry_key, job_message_key, job_session_id, job_spawn_key,
    };
    let _ = (job_message_key, job_session_id);
    assert_eq!(
        decode_job_retry_key(&job_spawn_key("r", "s", 2)),
        Some(("s".to_string(), 2))
    );
}

pub mod executor;

#[tokio::test]
async fn generated_plugin_call_access_cases_drive_bind_admission() {
    const CHILD_ENV: &str = "GENTS_PLUGIN_CALL_ACCESS_FIXTURE";
    const TEST: &str = "plugin::tests::generated_plugin_call_access_cases_drive_bind_admission";
    let Some(marker) = std::env::var_os(CHILD_ENV) else {
        // Permission fixtures mutate a process-wide protected-path registry. The
        // modeled cases require a stable filesystem, independent of those tests.
        let directory = tempfile::tempdir().unwrap();
        let marker = directory.path().join("verified");
        let cases = tokio::task::spawn_blocking(|| {
            gents_lean_vocab::lean_contract_snapshot()
                .plugin_resource_call_access_cases
                .clone()
        })
        .await
        .unwrap();
        tokio::fs::write(
            directory.path().join("cases.json"),
            serde_json::to_vec(&cases).unwrap(),
        )
        .await
        .unwrap();
        let output_path = directory.path().join("output");
        let output = tokio::fs::File::create(&output_path)
            .await
            .unwrap()
            .into_std()
            .await;
        let error = output.try_clone().unwrap();
        let mut child = tokio::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", TEST, "--nocapture", "--test-threads=1"])
            .env(CHILD_ENV, &marker)
            .stdin(std::process::Stdio::null())
            .stdout(output)
            .stderr(error)
            .kill_on_drop(true)
            .spawn()
            .unwrap();
        let status = tokio::time::timeout(std::time::Duration::from_secs(180), child.wait()).await;
        if status.is_err() {
            child.kill().await.unwrap();
            child.wait().await.unwrap();
        }
        let mut output = tokio::fs::File::open(output_path).await.unwrap();
        let mut diagnostics = Vec::new();
        tokio::io::AsyncReadExt::read_to_end(
            &mut tokio::io::AsyncReadExt::take(&mut output, 65_536),
            &mut diagnostics,
        )
        .await
        .unwrap();
        assert!(
            matches!(status, Ok(Ok(status)) if status.success()),
            "isolated admission cases failed: {status:?}\n{}",
            String::from_utf8_lossy(&diagnostics)
        );
        assert_eq!(tokio::fs::read(marker).await.unwrap(), b"24 cases verified");
        return;
    };
    use gents_pack::pack::BindAccess;
    let marker = std::path::PathBuf::from(marker);
    let cases: serde_json::Value = serde_json::from_slice(
        &tokio::fs::read(marker.parent().unwrap().join("cases.json"))
            .await
            .unwrap(),
    )
    .unwrap();
    let access = |value: &serde_json::Value| -> Option<BindAccess> {
        value.as_str().map(|text| text.parse().unwrap())
    };
    let cases = cases.as_array().unwrap();
    assert_eq!(cases.len(), 24);
    for case in cases {
        let declared = access(&case["declared"]).unwrap();
        let write_fields = case["write_fields"].as_bool().unwrap();
        let sets = case["sets_write_field"].as_bool().unwrap();
        let (home, mut record) =
            gents_pack::plugin::test_fixtures::installed_plugin(ECHO_WAT, Some(declared));
        let binding = record.declaration.bind_dir.as_mut().unwrap();
        if write_fields {
            binding.write_fields = vec!["output".into()];
        }
        record.declaration.input_schema = serde_json::json!({
            "type": "object",
            "properties": {"path": {"type": "string"}, "output": {"type": "string"}},
        });
        assert_eq!(
            record.declaration.validate().is_ok(),
            case["valid"].as_bool().unwrap(),
            "{case}"
        );
        if !case["valid"].as_bool().unwrap() {
            continue;
        }
        gents_pack::plugin::store::write_record(home.path(), &record).unwrap();
        let folder = tempfile::tempdir().unwrap();
        if let Some(granted) = access(&case["granted"]) {
            gents_pack::plugin::allowed::add(home.path(), folder.path(), granted).unwrap();
        }
        let mut input = serde_json::json!({ "path": folder.path() });
        if sets {
            input["output"] = serde_json::json!("out.json");
        }
        let binding = record.declaration.bind_dir.as_ref().unwrap();
        let call = access(&case["call"]).unwrap();
        assert_eq!(binding.call_access(&input), call, "{case}");
        let bound = crate::plugin::executor::PluginExecutor::new(Some(home.path().to_owned()))
            .bind_input(
                &record,
                &input,
                &crate::plugin::executor::BindContext::headless(None),
            )
            .await;
        match bound {
            Ok(Some(bound)) => {
                assert!(case["admitted"].as_bool().unwrap(), "{case}");
                assert_eq!(bound.access(), call, "{case}");
            }
            Ok(None) => panic!("the path field is set: {case}"),
            Err(reason) => {
                assert!(!case["admitted"].as_bool().unwrap(), "{case}: {reason}");
                assert!(
                    reason.contains("gents config pack plugin dirs add"),
                    "{reason}"
                );
            }
        }
    }
    tokio::fs::write(marker, b"24 cases verified")
        .await
        .unwrap();
}
