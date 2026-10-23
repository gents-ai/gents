use super::*;
use crate::plugin::approval::workspace;

fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().canonicalize().unwrap();
    let home = root.join("runtime");
    std::fs::create_dir(&home).unwrap();
    let key = workspace::WorkspaceKey::canonical("node", "requester", "agent", &root).unwrap();
    workspace::save_mode(&home, &key, 0, workspace::Mode::FullAccess, None).unwrap();
    std::fs::write(
        home.join(crate::home::ALLOWED_DIRS_FILE_NAME),
        "operator permission data",
    )
    .unwrap();
    (directory, root, home)
}

async fn run(
    root: &Path,
    home: &Path,
    mode: CommandExecutionMode,
    command: &str,
    args: &[String],
) -> (Option<i32>, Vec<u8>) {
    let scope =
        workspace::for_request(Some(home), "node", Some("requester"), "agent", Some(root)).unwrap();
    let policy = if mode == CommandExecutionMode::ReadOnly {
        CommandExecutionPolicy::read_only(vec!["cat".into()])
    } else {
        CommandExecutionPolicy::write_capable().with_mode(mode)
    };
    let launch = workspace::scope(scope, prepare_command_launch(root, command, args, &policy))
        .await
        .unwrap();
    let mut environment = launch.environment;
    environment.insert("GIT_CONFIG_GLOBAL".into(), "/dev/null".into());
    environment.insert("GIT_CONFIG_NOSYSTEM".into(), "1".into());
    environment.insert("GIT_TERMINAL_PROMPT".into(), "0".into());
    let outcome = run_managed_exec(ManagedExecRequest {
        argv: std::iter::once(launch.program).chain(launch.args).collect(),
        cwd: root.to_owned(),
        deadline_at: Some(chrono::Utc::now() + chrono::Duration::seconds(30)),
        cancellation_token: tokio_util::sync::CancellationToken::new(),
        max_output_bytes: 65536,
        stdin: Vec::new(),
        environment: Some(environment),
        tool_name: Some("command-guard-fixture".into()),
        live_output: None,
    })
    .await;
    match outcome {
        ManagedExecOutcome::Exited { code, stdout, .. } => (code, stdout),
        other => panic!("guarded command did not exit: {other:?}"),
    }
}

#[tokio::test]
async fn kernel_guard_preserves_work_and_denies_permission_names_links_and_moves() {
    for mode in [
        CommandExecutionMode::WorkspaceWrite,
        CommandExecutionMode::Unrestricted,
    ] {
        let (_directory, root, home) = fixture();
        let state = home.join(crate::home::ALLOWED_DIRS_FILE_NAME);
        let staging = home.join(crate::home::PERMISSION_STAGING_DIR_NAME);
        let questions = home.join(crate::home::PLUGIN_APPROVALS_DIR_NAME);
        for (script, allowed) in [
            ("printf allowed > \"$1/ordinary.tmp\" && mv \"$1/ordinary.tmp\" \"$1/ordinary\"", true),
            ("/usr/bin/git init -q \"$1/project\" && printf source > \"$1/project/source\" && /usr/bin/git -C \"$1/project\" add source && /usr/bin/git -C \"$1/project\" -c user.name=Fixture -c user.email=fixture@example.invalid -c commit.gpgsign=false -c core.hooksPath=/dev/null commit -qm Record", true),
            ("cat \"$2\"", false),
            ("printf changed > \"$2\"", false),
            ("chmod 777 \"$2\"", false),
            ("ln \"$2\" \"$1/permission-alias\"", false),
            ("mv \"$3\" \"$1/moved-runtime\"", false),
            ("printf changed > \"$4/new-state\"", false),
            ("mkdir \"$5\"", false),
        ] {
            let args = vec![
                "-c".into(),
                script.into(),
                "fixture".into(),
                root.to_str().unwrap().into(),
                state.to_str().unwrap().into(),
                home.to_str().unwrap().into(),
                staging.to_str().unwrap().into(),
                questions.to_str().unwrap().into(),
            ];
            let (code, output) = run(&root, &home, mode, "/bin/sh", &args).await;
            assert_eq!(code == Some(0), allowed, "{mode:?}: {script}");
            assert!(!String::from_utf8_lossy(&output).contains("operator permission data"));
            assert_eq!(std::fs::read(&state).unwrap(), b"operator permission data");
        }
        assert_eq!(std::fs::read(root.join("ordinary")).unwrap(), b"allowed");
        assert!(!root.join("permission-alias").exists());
        assert!(!root.join("moved-runtime").exists());
        assert!(!staging.join("new-state").exists());
    }
}

#[tokio::test]
async fn read_only_commands_keep_the_permission_guard() {
    let (_directory, root, home) = fixture();
    let target = home
        .join(crate::home::ALLOWED_DIRS_FILE_NAME)
        .to_str()
        .unwrap()
        .to_owned();
    let (code, output) = run(
        &root,
        &home,
        CommandExecutionMode::ReadOnly,
        "/bin/cat",
        &[target],
    )
    .await;
    assert_ne!(code, Some(0));
    assert!(!String::from_utf8_lossy(&output).contains("operator permission data"));
}

#[tokio::test]
async fn guarded_commands_build_and_run_a_rust_workspace() {
    for mode in [
        CommandExecutionMode::WorkspaceWrite,
        CommandExecutionMode::Unrestricted,
    ] {
        let (_directory, root, home) = fixture();
        std::fs::write(
            root.join("Cargo.toml"),
            "[package]\nname = \"guarded-build-fixture\"\nversion = \"0.1.0\"\nedition = \"2024\"\n\
             [workspace]\n[[bin]]\nname = \"guarded-build-fixture\"\npath = \"main.rs\"\n",
        )
        .unwrap();
        std::fs::write(root.join("main.rs"), "fn main() { print!(\"built\"); }\n").unwrap();
        let args = vec![
            "-c".into(),
            "RUSTC_WRAPPER= RUSTC_WORKSPACE_WRAPPER= TMPDIR=\"$2\" \"$1\" build --offline --manifest-path \"$2/Cargo.toml\" --target-dir \"$2/target\" 2>&1 && \"$2/target/debug/guarded-build-fixture\"".into(),
            "fixture".into(),
            env!("CARGO").into(),
            root.to_str().unwrap().into(),
        ];
        let (code, output) = run(&root, &home, mode, "/bin/sh", &args).await;
        assert_eq!(
            code,
            Some(0),
            "{mode:?}: {}",
            String::from_utf8_lossy(&output)
        );
        assert!(output.ends_with(b"built"));
        assert_eq!(
            std::fs::read(home.join(crate::home::ALLOWED_DIRS_FILE_NAME)).unwrap(),
            b"operator permission data"
        );
    }
}

#[tokio::test]
async fn configured_cli_tools_keep_the_guard_and_declared_environment() {
    let (_directory, root, home) = fixture();
    let tool = crate::toolset::cli_tool_for_test(crate::toolset::CliToolConfig {
        name: "configured-shell".into(),
        binary_path: "/bin/sh".into(),
        description: String::new(),
        allowed_argv_prefixes: vec![vec!["-c".into()]],
        env_vars: RapidHashMap::from_iter([("FIXTURE_WORD".into(), "kept".into())]),
        working_dir: Some(root.clone()),
        timeout_secs: 30,
        max_output_chars: 4096,
    });
    for (target, allowed) in [
        (root.join("cli-output"), true),
        (home.join(crate::home::ALLOWED_DIRS_FILE_NAME), false),
    ] {
        let scope =
            workspace::for_request(Some(&home), "node", Some("requester"), "agent", Some(&root))
                .unwrap();
        let args = serde_json::json!({"argv": ["-c", "printf '%s' \"$FIXTURE_WORD\" > \"$1\"", "fixture", target]}).to_string();
        let result = workspace::scope(scope, tool.call(args)).await;
        if allowed {
            result.unwrap();
        }
        assert_eq!(
            std::fs::read(&target).unwrap(),
            if allowed {
                b"kept".as_slice()
            } else {
                b"operator permission data".as_slice()
            }
        );
    }
}
