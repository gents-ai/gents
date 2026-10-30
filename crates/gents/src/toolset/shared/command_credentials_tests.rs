use super::*;
use crate::toolset::shared::command::command_launch::macos_workspace_write_policy;
use crate::toolset::shared::command::{
    CommandExecutionMode, CommandExecutionPolicy, CommandNetworkMode, SANDBOX_EXEC,
};

#[derive(serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct Case {
    mode: CommandExecutionMode,
    network: CommandNetworkMode,
    effective_network: CommandNetworkMode,
    protects_credentials: bool,
}

async fn run(
    profile: String,
    parameters: Vec<String>,
    root: &Path,
    script: &str,
    args: &[String],
) -> std::process::Output {
    tokio::process::Command::new(SANDBOX_EXEC)
        .args(["-p", &profile])
        .args(parameters)
        .args(["--", "/usr/bin/python3", "-c", script])
        .args(args)
        .current_dir(root)
        .kill_on_drop(true)
        .output()
        .await
        .unwrap()
}

#[tokio::test]
async fn generated_bounded_sandboxes_deny_credentials_and_default_network() {
    let cases = &crate::lean_vocab_test::lean_contract_snapshot().command_isolation_cases;
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path().canonicalize().unwrap();
    let home = root.join("user");
    let mut paths = Vec::new();
    for name in USER_CREDENTIALS {
        let path = home.join(name);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, "fixture credential").unwrap();
        paths.push(path);
    }
    let alias = root.join("alias");
    std::os::unix::fs::symlink(&paths[0], &alias).unwrap();
    let outside = root.join("linked-secret");
    std::fs::write(&outside, "fixture credential").unwrap();
    let redirected = home.join("redirected");
    std::os::unix::fs::symlink(&outside, &redirected).unwrap();
    let nested = home.join("directory");
    std::fs::create_dir(&nested).unwrap();
    std::os::unix::fs::symlink(&outside, nested.join("link")).unwrap();
    paths.extend([redirected, nested]);
    let protection = CredentialProtection::capture(paths.clone()).unwrap();
    let mut targets = paths
        .into_iter()
        .map(|path| path.to_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    targets.extend([
        alias.to_str().unwrap().into(),
        outside.to_str().unwrap().into(),
    ]);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    for value in cases {
        let case: Case = serde_json::from_value(value.clone()).unwrap();
        if !case.protects_credentials {
            continue;
        }
        let policy = CommandExecutionPolicy::write_capable()
            .with_mode(case.mode)
            .with_network_mode(case.network);
        let mut profile = macos_workspace_write_policy(policy.effective_network_mode());
        let mut parameters = vec![format!("-DWRITABLE_ROOT={}", root.display())];
        protection.apply(&mut profile, &mut parameters).unwrap();
        let script = r#"import errno, os, pathlib, socket, sys
for name in sys.argv[3:]:
    try:
        pathlib.Path(name).read_bytes()
    except PermissionError:
        pass
    else:
        raise AssertionError('credential read permitted: ' + name)
for operation in [
    lambda: os.link(sys.argv[3], 'credential-alias'),
    lambda: pathlib.Path(sys.argv[3]).write_text('changed'),
    lambda: os.rename('user', 'moved-user'),
]:
    try:
        operation()
    except PermissionError:
        pass
    else:
        raise AssertionError('credential namespace modification permitted')
pathlib.Path('ordinary').write_text('workspace write')
assert pathlib.Path('ordinary').read_text() == 'workspace write'
with socket.socket() as connection:
    connection.settimeout(2)
    try:
        connection.connect(('127.0.0.1', int(sys.argv[1])))
    except PermissionError as error:
        assert sys.argv[2] == 'disabled', error
        assert error.errno in (errno.EPERM, errno.EACCES), error
    else:
        assert sys.argv[2] == 'enabled'
print('verified credential, workspace and network boundaries')
"#;
        let args = [
            vec![port.to_string(), case.effective_network.as_str().into()],
            targets.clone(),
        ]
        .concat();
        let output = run(profile, parameters, &root, script, &args).await;
        assert!(
            output.status.success(),
            "{value}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("verified credential"));
    }
}

#[tokio::test]
async fn protected_hard_link_names_inside_one_namespace_are_allowed_and_unreadable() {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path().canonicalize().unwrap();
    let namespace = root.join(".gnupg");
    std::fs::create_dir(&namespace).unwrap();
    let first = namespace.join("private-keys-v1.d");
    let second = namespace.join("private-keys-v1.lock");
    std::fs::write(&first, "fixture credential").unwrap();
    std::fs::hard_link(&first, &second).unwrap();

    let protection = CredentialProtection::capture([namespace]).unwrap();
    let mut profile = macos_workspace_write_policy(CommandNetworkMode::Disabled);
    let mut parameters = vec![format!("-DWRITABLE_ROOT={}", root.display())];
    protection.apply(&mut profile, &mut parameters).unwrap();
    let script = r#"import pathlib, sys
for name in sys.argv[1:]:
    try:
        pathlib.Path(name).read_bytes()
    except PermissionError:
        continue
    raise AssertionError('protected hard-link alias was readable: ' + name)
print('protected hard-link aliases denied')
"#;
    let targets = [first, second]
        .map(|path| path.to_string_lossy().into_owned())
        .to_vec();
    let output = run(profile, parameters, &root, script, &targets).await;
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(String::from_utf8_lossy(&output.stdout).contains("protected hard-link aliases denied"));
}

#[test]
fn credential_aliases_and_unreadable_namespaces_cannot_fall_back() {
    let folder = tempfile::tempdir().unwrap();
    let root = folder.path().canonicalize().unwrap();
    let key = root.join("secret");
    std::fs::write(&key, "fixture credential").unwrap();
    std::fs::hard_link(&key, root.join("alias")).unwrap();
    let error = CredentialProtection::capture([key.clone()]).unwrap_err();
    assert!(
        format!("{error:#}").contains("hard-link alias"),
        "{error:#}"
    );
    let duplicate_roots_error = CredentialProtection::capture([key.clone(), key.clone()])
        .expect_err("repeated protected roots must not account for an external alias");
    assert!(
        format!("{duplicate_roots_error:#}").contains("hard-link alias outside"),
        "{duplicate_roots_error:#}"
    );
    let dangling = root.join("dangling");
    std::os::unix::fs::symlink(root.join("absent"), &dangling).unwrap();
    assert!(CredentialProtection::capture([dangling]).is_err());
}

#[test]
fn runtime_credentials_include_the_configured_identity_and_store_keys() {
    let folder = tempfile::tempdir().unwrap();
    let home = folder.path().canonicalize().unwrap();
    let custom_key = home.join("custom-identity");
    write_identity_config(&home, &custom_key);
    let protection = CredentialProtection::capture(runtime_credentials(&home).unwrap()).unwrap();
    assert!(protection.paths.contains(&custom_key));
    assert!(protection.paths.contains(&crate::home::keys_dir(&home)));
    assert!(protection
        .paths
        .contains(&home.join(crate::home::P2P_SECRET_KEY_FILE_NAME)));
}

fn write_identity_config(home: &Path, custom_key: &Path) {
    crate::home::write_init_config(
        home,
        &crate::home::StoredInitConfig::<String, String> {
            home: home.to_str().unwrap().into(),
            node_name: "fixture".into(),
            node_did: "node".into(),
            key_path: Some(custom_key.to_str().unwrap().into()),
            identity_backend: None,
            keychain_label: None,
            secure_enclave_label: None,
            tool_package: None,
            tool_ceiling: "readwrite".into(),
            tool_root: None,
            store_encryption: None,
        },
    )
    .unwrap();
}

#[tokio::test]
async fn changed_identity_configuration_invalidates_waiting_command_exclusions() {
    use crate::tool_call_lifecycle::runtime::{
        scope_request_tool_execution_with_workspace_overlay, ToolWorkspaceScope,
    };
    let folder = tempfile::tempdir().unwrap();
    let home = folder.path().canonicalize().unwrap();
    let original = home.join("original-key");
    let replacement = home.join("replacement-key");
    write_identity_config(&home, &original);
    scope_request_tool_execution_with_workspace_overlay(
        None,
        Default::default(),
        ToolWorkspaceScope {
            runtime_home: Some(home.clone()),
            ..Default::default()
        },
        None,
        None,
        None,
        Default::default(),
        false,
        async {
            let waiting = capture().await.unwrap();
            write_identity_config(&home, &replacement);
            let error = waiting.revalidate().await.unwrap_err();
            assert!(
                error.to_string().contains("exclusions changed"),
                "{error:#}"
            );
            let current = capture().await.unwrap();
            assert!(current.paths.contains(&replacement));
            assert!(!current.paths.contains(&original));
        },
    )
    .await;
}
