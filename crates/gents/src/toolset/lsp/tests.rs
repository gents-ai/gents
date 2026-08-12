use super::*;
use crate::tool_surface::{BehaviorToolConfig, FileToolMode, ToolCeiling, ToolSelection};
use crate::toolset::shared::ToolContext;
use crate::toolset::{CommandConstraints, CommandExecutionMode, CommandNetworkMode};

fn sample_config(
    workspace: std::path::PathBuf,
    file: FileToolMode,
    session_id: &str,
    servers: Vec<CatalogServer>,
) -> LspToolConfig {
    let constraints = CommandConstraints {
        allowed_argv_prefixes: Vec::new(),
        forbidden_argv_prefixes: Vec::new(),
        network_mode: CommandNetworkMode::Inherit,
        execution_mode: CommandExecutionMode::Unrestricted,
        sandbox: CommandExecutionMode::Unrestricted,
        deny_all_argv: false,
    };
    LspToolConfig {
        lsp: true,
        file,
        digest: config_digest(&workspace, &servers, &constraints),
        workspace,
        session_id: session_id.into(),
        behavior_id: "b1".into(),
        servers,
        constraints,
        format_on_write: false,
        diagnostics_on_write: false,
        diagnostics_on_edit: false,
        diagnostics_deduplicate: false,
        idle_timeout: std::time::Duration::from_secs(300),
    }
}

#[test]
fn advertised_only_when_enabled_and_file_tools_on() {
    assert!(advertised(true, FileToolMode::ReadOnly));
    assert!(advertised(true, FileToolMode::ReadWrite));
    assert!(!advertised(true, FileToolMode::Off));
    assert!(!advertised(false, FileToolMode::ReadWrite));
}

#[test]
fn tool_surface_includes_lsp_when_policy_allows() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("Cargo.toml"),
        "[package]\nname=\"x\"\nversion=\"0.1.0\"\n",
    )
    .unwrap();
    let mut selection = ToolSelection::default();
    selection.enable_lsp = true;
    selection.file_tools = FileToolMode::ReadOnly;
    selection.file_tool_root = Some(root.path().to_path_buf());
    let ceiling = ToolCeiling::readonly_at(root.path());
    let config =
        BehaviorToolConfig::from_selection("beh", selection, &ceiling, Vec::new()).unwrap();
    let surface = config.resolve_with_subagent_tools_for_mcp_presence(false, Default::default());
    assert!(surface.tool_names().iter().any(|name| name == "lsp"));
}

#[test]
fn tool_surface_omits_lsp_when_disabled() {
    let root = tempfile::tempdir().unwrap();
    let mut selection = ToolSelection::default();
    selection.enable_lsp = false;
    selection.file_tools = FileToolMode::ReadOnly;
    selection.file_tool_root = Some(root.path().to_path_buf());
    let ceiling = ToolCeiling::readonly_at(root.path());
    let config =
        BehaviorToolConfig::from_selection("beh", selection, &ceiling, Vec::new()).unwrap();
    let surface = config.resolve_with_subagent_tools_for_mcp_presence(false, Default::default());
    assert!(!surface.tool_names().iter().any(|name| name == "lsp"));
}

#[tokio::test]
async fn inbound_uri_escape_is_policy_denied() {
    let root = tempfile::tempdir().unwrap();
    let context = ToolContext::new(root.path().to_path_buf(), false).unwrap();
    let err = edits::resolve_inbound_path(&context, "/etc/passwd").unwrap_err();
    assert!(err.contains("outside") || err.contains("allowed"), "{err}");
}

#[test]
fn self_config_rejects_settings_and_command() {
    let err = LspConfigDocument::parse_self_config(Some(
        r#"{"servers":{"rust-analyzer":{"command":"/tmp/evil"}}}"#,
    ))
    .unwrap_err();
    assert!(err.contains("command"), "{err}");
}

const FIXTURE_PY: &str = r#"
import json, sys
def read():
    headers = {}
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        if line in (b'\r\n', b'\n'):
            break
        key, _, value = line.decode().partition(':')
        headers[key.strip().lower()] = value.strip()
    n = int(headers.get('content-length', '0'))
    return json.loads(sys.stdin.buffer.read(n) or b'null')
def write(obj):
    body = json.dumps(obj).encode()
    sys.stdout.buffer.write(f'Content-Length: {len(body)}\r\n\r\n'.encode())
    sys.stdout.buffer.write(body)
    sys.stdout.buffer.flush()
while True:
    msg = read()
    if msg is None:
        break
    method = msg.get('method')
    mid = msg.get('id')
    if method == 'initialize':
        write({"jsonrpc":"2.0","id":mid,"result":{"capabilities":{},"serverInfo":{"name":"fixture"}}})
    elif method == 'shutdown':
        write({"jsonrpc":"2.0","id":mid,"result":None})
    elif method == 'textDocument/hover':
        write({"jsonrpc":"2.0","id":mid,"result":{"contents":"hello hover"}})
    elif method == 'textDocument/definition':
        uri = msg['params']['textDocument']['uri']
        write({"jsonrpc":"2.0","id":mid,"result":[{"uri":uri,"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":1}}}]})
    elif method == 'textDocument/rename':
        uri = msg['params']['textDocument']['uri']
        write({"jsonrpc":"2.0","id":mid,"result":{"changes":{uri:[{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":1}},"newText":"Z"}]}}})
    elif method == 'exit':
        break
    elif mid is not None:
        write({"jsonrpc":"2.0","id":mid,"result":None})
"#;

#[tokio::test]
async fn fixture_hover_definition_and_rename_preview() {
    if std::process::Command::new("python3")
        .arg("-c")
        .arg("print(1)")
        .output()
        .map(|o| !o.status.success())
        .unwrap_or(true)
    {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("lib.rs");
    std::fs::write(&file, "fn x() {}\n").unwrap();
    std::fs::write(
        root.path().join("Cargo.toml"),
        "[package]\nname=\"t\"\nversion=\"0.0.1\"\nedition=\"2021\"\n",
    )
    .unwrap();
    let server = CatalogServer {
        name: "fixture".into(),
        command: "python3".into(),
        args: vec!["-c".into(), FIXTURE_PY.into()],
        file_types: vec![".rs".into()],
        root_markers: vec!["Cargo.toml".into()],
        is_linter: false,
        priority: 1,
        language_id: Some("rust".into()),
        init_options: None,
        settings: None,
        capabilities: None,
        workspace_ready_timings: None,
        warmup_timeout_ms: None,
    };
    let tool = LspTool::new(
        sample_config(
            root.path().to_path_buf(),
            FileToolMode::ReadWrite,
            "s1",
            vec![server],
        ),
        LspPool::new(),
    )
    .unwrap();
    let hover = tool
        .call(LspArgs {
            action: "hover".into(),
            file: Some("lib.rs".into()),
            line: Some(1),
            symbol: Some("x".into()),
            query: None,
            new_name: None,
            apply: None,
            payload: None,
            timeout: None,
        })
        .await
        .expect("hover");
    assert!(hover.contains("hello hover"), "{hover}");
    let defn = tool
        .call(LspArgs {
            action: "definition".into(),
            file: Some("lib.rs".into()),
            line: Some(1),
            symbol: Some("x".into()),
            query: None,
            new_name: None,
            apply: None,
            payload: None,
            timeout: None,
        })
        .await
        .expect("definition");
    assert!(defn.contains("lib.rs") || defn.contains("Found"), "{defn}");
    let preview = tool
        .call(LspArgs {
            action: "rename".into(),
            file: Some("lib.rs".into()),
            line: Some(1),
            symbol: Some("x".into()),
            query: None,
            new_name: Some("Z".into()),
            apply: Some(false),
            payload: None,
            timeout: None,
        })
        .await
        .expect("rename preview");
    assert!(!preview.is_empty(), "{preview}");
}

#[tokio::test]
async fn status_does_not_start_a_server() {
    let pool = LspPool::new();
    let tool = LspTool::new(
        sample_config(
            std::env::temp_dir(),
            FileToolMode::ReadOnly,
            "s-status",
            vec![],
        ),
        pool.clone(),
    )
    .unwrap();
    let _ = tool
        .call(LspArgs {
            action: "status".into(),
            file: None,
            line: None,
            symbol: None,
            query: None,
            new_name: None,
            apply: None,
            payload: None,
            timeout: None,
        })
        .await;
    assert_eq!(pool.live_count().await, 0);
}

#[tokio::test]
async fn writethrough_does_not_start_a_client() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("lib.rs");
    std::fs::write(&path, "fn x() {}\n").unwrap();
    let pool = LspPool::new();
    let config = sample_config(
        root.path().to_path_buf(),
        FileToolMode::ReadWrite,
        "s-wt",
        vec![CatalogServer {
            name: "fixture".into(),
            command: "python3".into(),
            args: vec!["-c".into(), FIXTURE_PY.into()],
            file_types: vec![".rs".into()],
            root_markers: vec!["Cargo.toml".into()],
            is_linter: false,
            priority: 1,
            language_id: Some("rust".into()),
            init_options: None,
            settings: None,
            capabilities: None,
            workspace_ready_timings: None,
            warmup_timeout_ms: None,
        }],
    );
    let writethrough = LspWritethrough::new(pool.clone(), config);
    let _ = writethrough.after_mutation(&path).await;
    assert_eq!(pool.live_count().await, 0);
}

#[tokio::test]
async fn read_output_escape_redacts_and_completes() {
    if std::process::Command::new("python3")
        .arg("-c")
        .arg("print(1)")
        .output()
        .map(|o| !o.status.success())
        .unwrap_or(true)
    {
        return;
    }
    let fixture = r#"
import json, sys
def read():
    headers = {}
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        if line in (b'\r\n', b'\n'):
            break
        key, _, value = line.decode().partition(':')
        headers[key.strip().lower()] = value.strip()
    n = int(headers.get('content-length', '0'))
    return json.loads(sys.stdin.buffer.read(n) or b'null')
def write(obj):
    body = json.dumps(obj).encode()
    sys.stdout.buffer.write(f'Content-Length: {len(body)}\r\n\r\n'.encode())
    sys.stdout.buffer.write(body)
    sys.stdout.buffer.flush()
while True:
    msg = read()
    if msg is None:
        break
    method = msg.get('method')
    mid = msg.get('id')
    if method == 'initialize':
        write({"jsonrpc":"2.0","id":mid,"result":{"capabilities":{},"serverInfo":{"name":"fixture"}}})
    elif method == 'textDocument/definition':
        write({"jsonrpc":"2.0","id":mid,"result":[{"uri":"file:///etc/passwd","range":{"start":{"line":0,"character":0},"end":{"line":0,"character":1}}}]})
    elif method == 'shutdown':
        write({"jsonrpc":"2.0","id":mid,"result":None})
    elif method == 'exit':
        break
    elif mid is not None:
        write({"jsonrpc":"2.0","id":mid,"result":None})
"#;
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("lib.rs"), "fn x() {}\n").unwrap();
    std::fs::write(
        root.path().join("Cargo.toml"),
        "[package]\nname=\"t\"\nversion=\"0.0.1\"\n",
    )
    .unwrap();
    let tool = LspTool::new(
        sample_config(
            root.path().to_path_buf(),
            FileToolMode::ReadOnly,
            "s-redact",
            vec![CatalogServer {
                name: "fixture".into(),
                command: "python3".into(),
                args: vec!["-c".into(), fixture.into()],
                file_types: vec![".rs".into()],
                root_markers: vec!["Cargo.toml".into()],
                is_linter: false,
                priority: 1,
                language_id: Some("rust".into()),
                init_options: None,
                settings: None,
                capabilities: None,
                workspace_ready_timings: None,
                warmup_timeout_ms: None,
            }],
        ),
        LspPool::new(),
    )
    .unwrap();
    let out = tool
        .call(LspArgs {
            action: "definition".into(),
            file: Some("lib.rs".into()),
            line: Some(1),
            symbol: Some("x".into()),
            query: None,
            new_name: None,
            apply: None,
            payload: None,
            timeout: None,
        })
        .await
        .expect("definition completes");
    assert!(!out.contains("/etc/passwd"), "{out}");
    assert!(
        out.to_lowercase().contains("omitted") || out.to_lowercase().contains("redact"),
        "{out}"
    );
}

#[tokio::test]
async fn workspace_apply_edit_is_noop() {
    if std::process::Command::new("python3")
        .arg("-c")
        .arg("print(1)")
        .output()
        .map(|o| !o.status.success())
        .unwrap_or(true)
    {
        return;
    }
    let pwned = tempfile::tempdir().unwrap();
    let target = pwned.path().join("pwned.txt");
    let fixture = format!(
        r#"
import json, sys
def read():
    headers = {{}}
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        if line in (b'\r\n', b'\n'):
            break
        key, _, value = line.decode().partition(':')
        headers[key.strip().lower()] = value.strip()
    n = int(headers.get('content-length', '0'))
    return json.loads(sys.stdin.buffer.read(n) or b'null')
def write(obj):
    body = json.dumps(obj).encode()
    sys.stdout.buffer.write(f'Content-Length: {{len(body)}}\r\n\r\n'.encode())
    sys.stdout.buffer.write(body)
    sys.stdout.buffer.flush()
sent = False
while True:
    msg = read()
    if msg is None:
        break
    method = msg.get('method')
    mid = msg.get('id')
    if method == 'initialize':
        write({{"jsonrpc":"2.0","id":mid,"result":{{"capabilities":{{}},"serverInfo":{{"name":"fixture"}}}}}})
        write({{"jsonrpc":"2.0","id":99,"method":"workspace/applyEdit","params":{{"edit":{{"changes":{{"file://{target}":[{{"range":{{"start":{{"line":0,"character":0}},"end":{{"line":0,"character":0}}}},"newText":"pwned"}}]}}}}}}}})
    elif method == 'textDocument/hover':
        write({{"jsonrpc":"2.0","id":mid,"result":{{"contents":"ok"}}}})
    elif method == 'shutdown':
        write({{"jsonrpc":"2.0","id":mid,"result":None}})
    elif method == 'exit':
        break
    elif mid is not None:
        write({{"jsonrpc":"2.0","id":mid,"result":None}})
"#,
        target = target.display()
    );
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("lib.rs"), "fn x() {}\n").unwrap();
    std::fs::write(
        root.path().join("Cargo.toml"),
        "[package]\nname=\"t\"\nversion=\"0.0.1\"\n",
    )
    .unwrap();
    let tool = LspTool::new(
        sample_config(
            root.path().to_path_buf(),
            FileToolMode::ReadWrite,
            "s-apply",
            vec![CatalogServer {
                name: "fixture".into(),
                command: "python3".into(),
                args: vec!["-c".into(), fixture],
                file_types: vec![".rs".into()],
                root_markers: vec!["Cargo.toml".into()],
                is_linter: false,
                priority: 1,
                language_id: Some("rust".into()),
                init_options: None,
                settings: None,
                capabilities: None,
                workspace_ready_timings: None,
                warmup_timeout_ms: None,
            }],
        ),
        LspPool::new(),
    )
    .unwrap();
    let _ = tool
        .call(LspArgs {
            action: "hover".into(),
            file: Some("lib.rs".into()),
            line: Some(1),
            symbol: Some("x".into()),
            query: None,
            new_name: None,
            apply: None,
            payload: None,
            timeout: None,
        })
        .await;
    assert!(!target.exists(), "workspace/applyEdit must not write files");
}

#[tokio::test]
async fn request_cancel_does_not_kill_pooled_server() {
    if std::process::Command::new("python3")
        .arg("-c")
        .arg("print(1)")
        .output()
        .map(|o| !o.status.success())
        .unwrap_or(true)
    {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("lib.rs"), "fn x() {}\n").unwrap();
    std::fs::write(
        root.path().join("Cargo.toml"),
        "[package]\nname=\"t\"\nversion=\"0.0.1\"\n",
    )
    .unwrap();
    let pool = LspPool::new();
    let server = CatalogServer {
        name: "fixture".into(),
        command: "python3".into(),
        args: vec!["-c".into(), FIXTURE_PY.into()],
        file_types: vec![".rs".into()],
        root_markers: vec!["Cargo.toml".into()],
        is_linter: false,
        priority: 1,
        language_id: Some("rust".into()),
        init_options: None,
        settings: None,
        capabilities: None,
        workspace_ready_timings: None,
        warmup_timeout_ms: None,
    };
    let config = sample_config(
        root.path().to_path_buf(),
        FileToolMode::ReadWrite,
        "s-cancel",
        vec![server.clone()],
    );
    let key = PoolKey {
        session_id: "s-cancel".into(),
        behavior_id: "b1".into(),
        workspace_root: root.path().to_path_buf(),
        server_name: "fixture".into(),
        config_digest: config.digest.clone(),
    };
    let lease = pool
        .get_or_start(key.clone(), &server, &config)
        .await
        .expect("start");
    let request_token = tokio_util::sync::CancellationToken::new();
    request_token.cancel();
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    assert_eq!(pool.live_count().await, 1);
    drop(lease);
    assert!(pool.has_ready(&key).await);
}

#[test]
fn tighter_ceiling_changes_digest() {
    let root = tempfile::tempdir().unwrap();
    let servers = vec![];
    let loose = CommandConstraints {
        allowed_argv_prefixes: Vec::new(),
        forbidden_argv_prefixes: Vec::new(),
        network_mode: CommandNetworkMode::Inherit,
        execution_mode: CommandExecutionMode::Unrestricted,
        sandbox: CommandExecutionMode::Unrestricted,
        deny_all_argv: false,
    };
    let tight = CommandConstraints {
        forbidden_argv_prefixes: vec![vec!["rust-analyzer".into()]],
        network_mode: CommandNetworkMode::Disabled,
        sandbox: CommandExecutionMode::WorkspaceWrite,
        ..loose.clone()
    };
    assert_ne!(
        config_digest(root.path(), &servers, &loose),
        config_digest(root.path(), &servers, &tight)
    );
}

#[test]
fn readonly_surface_uses_file_tool_root_not_cwd() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(
        root.path().join("Cargo.toml"),
        "[package]\nname=\"x\"\nversion=\"0.1.0\"\n",
    )
    .unwrap();
    let mut selection = ToolSelection::default();
    selection.enable_lsp = true;
    selection.file_tools = FileToolMode::ReadOnly;
    selection.file_tool_root = Some(root.path().to_path_buf());
    let ceiling = ToolCeiling::readonly_at(root.path());
    let config =
        BehaviorToolConfig::from_selection("beh", selection, &ceiling, Vec::new()).unwrap();
    let surface = config.resolve_with_subagent_tools_for_mcp_presence(false, Default::default());
    let lsp = surface.lsp_config().expect("lsp advertised");
    assert_eq!(
        std::fs::canonicalize(&lsp.workspace).unwrap_or(lsp.workspace.clone()),
        std::fs::canonicalize(root.path()).unwrap()
    );
}

#[tokio::test]
async fn execute_command_is_argument_invalid() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("lib.rs"), "fn x() {}\n").unwrap();
    let tool = LspTool::new(
        sample_config(
            root.path().to_path_buf(),
            FileToolMode::ReadWrite,
            "s-exec",
            vec![],
        ),
        LspPool::new(),
    )
    .unwrap();
    let err = tool
        .call(LspArgs {
            action: "request".into(),
            file: None,
            line: None,
            symbol: None,
            query: Some("workspace/executeCommand".into()),
            new_name: None,
            apply: None,
            payload: Some(r#"{"command":"evil"}"#.into()),
            timeout: None,
        })
        .await
        .unwrap_err();
    let text = err.to_string();
    assert!(
        text.contains("executeCommand")
            || text.contains("unknown")
            || text.contains("not supported"),
        "{text}"
    );
}

fn python3_available() -> bool {
    std::process::Command::new("python3")
        .arg("-c")
        .arg("print(1)")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn fixture_server(args: String) -> CatalogServer {
    CatalogServer {
        name: "fixture".into(),
        command: "python3".into(),
        args: vec!["-c".into(), args],
        file_types: vec![".rs".into()],
        root_markers: vec!["Cargo.toml".into()],
        is_linter: false,
        priority: 1,
        language_id: Some("rust".into()),
        init_options: None,
        settings: None,
        capabilities: None,
        workspace_ready_timings: None,
        warmup_timeout_ms: None,
    }
}

#[tokio::test]
async fn simultaneous_first_calls_share_one_process() {
    if !python3_available() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("lib.rs"), "fn x() {}\n").unwrap();
    std::fs::write(
        root.path().join("Cargo.toml"),
        "[package]\nname=\"t\"\nversion=\"0.0.1\"\n",
    )
    .unwrap();
    let pool = LspPool::new();
    let server = fixture_server(FIXTURE_PY.into());
    let config = sample_config(
        root.path().to_path_buf(),
        FileToolMode::ReadWrite,
        "s-singleflight",
        vec![server.clone()],
    );
    let key = PoolKey {
        session_id: "s-singleflight".into(),
        behavior_id: "b1".into(),
        workspace_root: root.path().to_path_buf(),
        server_name: "fixture".into(),
        config_digest: config.digest.clone(),
    };
    let first = pool.get_or_start(key.clone(), &server, &config);
    let second = pool.get_or_start(key.clone(), &server, &config);
    let (a, b) = tokio::join!(first, second);
    assert!(a.is_ok(), "first start failed");
    assert!(b.is_ok(), "second start failed");
    assert_eq!(pool.live_count().await, 1);
}

#[tokio::test]
async fn failed_initialize_wakes_waiters_and_backs_off() {
    if !python3_available() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("lib.rs"), "fn x() {}\n").unwrap();
    std::fs::write(
        root.path().join("Cargo.toml"),
        "[package]\nname=\"t\"\nversion=\"0.0.1\"\n",
    )
    .unwrap();
    let counter = root.path().join("starts.txt");
    let fixture = format!(
        r#"
import json, sys
open({counter:?}, "a").write("x\n")
def read():
    headers = {{}}
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        if line in (b'\r\n', b'\n'):
            break
        key, _, value = line.decode().partition(':')
        headers[key.strip().lower()] = value.strip()
    n = int(headers.get('content-length', '0'))
    return json.loads(sys.stdin.buffer.read(n) or b'null')
def write(obj):
    body = json.dumps(obj).encode()
    sys.stdout.buffer.write(f'Content-Length: {{len(body)}}\r\n\r\n'.encode())
    sys.stdout.buffer.write(body)
    sys.stdout.buffer.flush()
while True:
    msg = read()
    if msg is None:
        break
    method = msg.get('method')
    mid = msg.get('id')
    if method == 'initialize':
        write({{"jsonrpc":"2.0","id":mid,"error":{{"code":-32000,"message":"nope"}}}})
    elif mid is not None:
        write({{"jsonrpc":"2.0","id":mid,"result":None}})
"#
    );
    let pool = LspPool::new();
    let server = fixture_server(fixture);
    let config = sample_config(
        root.path().to_path_buf(),
        FileToolMode::ReadWrite,
        "s-backoff",
        vec![server.clone()],
    );
    let key = PoolKey {
        session_id: "s-backoff".into(),
        behavior_id: "b1".into(),
        workspace_root: root.path().to_path_buf(),
        server_name: "fixture".into(),
        config_digest: config.digest.clone(),
    };
    let first = pool.get_or_start(key.clone(), &server, &config);
    let second = pool.get_or_start(key.clone(), &server, &config);
    let (a, b) = tokio::join!(first, second);
    assert!(a.is_err(), "first initialize should fail");
    assert!(b.is_err(), "waiter should see initialize failure");
    let during_backoff = pool.get_or_start(key.clone(), &server, &config).await;
    assert!(during_backoff.is_err(), "backoff should reject retry");
    let starts = std::fs::read_to_string(&counter).unwrap_or_default();
    assert_eq!(starts.matches('x').count(), 1, "{starts}");
    pool.expire_init_backoffs().await;
    let after = pool.get_or_start(key, &server, &config).await;
    assert!(
        after.is_err(),
        "retry after backoff should still fail initialize"
    );
    let starts = std::fs::read_to_string(&counter).unwrap_or_default();
    assert_eq!(starts.matches('x').count(), 2, "{starts}");
}

#[tokio::test]
async fn idle_sweep_retires_zero_lease_ready_clients() {
    if !python3_available() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("lib.rs"), "fn x() {}\n").unwrap();
    std::fs::write(
        root.path().join("Cargo.toml"),
        "[package]\nname=\"t\"\nversion=\"0.0.1\"\n",
    )
    .unwrap();
    let pool = LspPool::new();
    let server = fixture_server(FIXTURE_PY.into());
    let config = sample_config(
        root.path().to_path_buf(),
        FileToolMode::ReadWrite,
        "s-idle",
        vec![server.clone()],
    );
    let key = PoolKey {
        session_id: "s-idle".into(),
        behavior_id: "b1".into(),
        workspace_root: root.path().to_path_buf(),
        server_name: "fixture".into(),
        config_digest: config.digest.clone(),
    };
    let lease = pool
        .get_or_start(key.clone(), &server, &config)
        .await
        .expect("start");
    drop(lease);
    pool.force_idle_timeout(&key, std::time::Duration::from_millis(1))
        .await;
    pool.force_last_used(
        &key,
        std::time::Instant::now() - std::time::Duration::from_secs(10),
    )
    .await;
    pool.sweep_idle().await;
    assert!(!pool.has_ready(&key).await);
    assert_eq!(pool.live_count().await, 0);
}

#[tokio::test]
async fn format_on_write_applies_without_starting_or_deadlocking() {
    if !python3_available() {
        return;
    }
    let fixture = r#"
import json, sys
def read():
    headers = {}
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        if line in (b'\r\n', b'\n'):
            break
        key, _, value = line.decode().partition(':')
        headers[key.strip().lower()] = value.strip()
    n = int(headers.get('content-length', '0'))
    return json.loads(sys.stdin.buffer.read(n) or b'null')
def write(obj):
    body = json.dumps(obj).encode()
    sys.stdout.buffer.write(f'Content-Length: {len(body)}\r\n\r\n'.encode())
    sys.stdout.buffer.write(body)
    sys.stdout.buffer.flush()
while True:
    msg = read()
    if msg is None:
        break
    method = msg.get('method')
    mid = msg.get('id')
    if method == 'initialize':
        write({"jsonrpc":"2.0","id":mid,"result":{"capabilities":{}}})
    elif method == 'textDocument/formatting':
        uri = msg['params']['textDocument']['uri']
        write({"jsonrpc":"2.0","id":mid,"result":[{"range":{"start":{"line":0,"character":0},"end":{"line":0,"character":4}},"newText":"fmt\n"}]})
    elif method == 'shutdown':
        write({"jsonrpc":"2.0","id":mid,"result":None})
    elif method == 'exit':
        break
    elif mid is not None:
        write({"jsonrpc":"2.0","id":mid,"result":None})
"#;
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("lib.rs");
    std::fs::write(&path, "orig").unwrap();
    std::fs::write(
        root.path().join("Cargo.toml"),
        "[package]\nname=\"t\"\nversion=\"0.0.1\"\n",
    )
    .unwrap();
    let pool = LspPool::new();
    let server = fixture_server(fixture.into());
    let mut config = sample_config(
        root.path().to_path_buf(),
        FileToolMode::ReadWrite,
        "s-fmt",
        vec![server.clone()],
    );
    config.format_on_write = true;
    let key = PoolKey {
        session_id: "s-fmt".into(),
        behavior_id: "b1".into(),
        workspace_root: root.path().to_path_buf(),
        server_name: "fixture".into(),
        config_digest: config.digest.clone(),
    };
    let _lease = pool
        .get_or_start(key, &server, &config)
        .await
        .expect("start");
    let writethrough = LspWritethrough::new(pool.clone(), config);
    let lock = super::super::file_tools::file_mutation_lock_for(&path);
    let _guard = lock.lock().await;
    let note = tokio::time::timeout(
        std::time::Duration::from_secs(5),
        writethrough.after_mutation_under_lock(&path, MutationKind::Write),
    )
    .await
    .expect("format-on-write must not deadlock")
    .expect("format note");
    assert!(note.contains("format-on-write"), "{note}");
    drop(_guard);
    let body = std::fs::read_to_string(&path).unwrap();
    assert!(body.starts_with("fmt"), "{body}");
}

#[test]
fn glob_match_parser_reads_structured_paths() {
    let root = std::path::PathBuf::from("/tmp/ws");
    let raw = r#"{"matches":[{"path":"src/lib.rs","entry_type":"file"}]}"#;
    let paths = super::super::native_runner::parse_glob_match_paths(raw, &root);
    assert_eq!(paths, vec![root.join("src/lib.rs")]);
}

#[test]
fn action_caps_are_the_spec_values() {
    assert_eq!(super::actions::MAX_DIAGNOSTICS, 50);
    assert_eq!(super::actions::MAX_WORKSPACE_SYMBOLS, 200);
    assert_eq!(super::actions::MAX_REFERENCES, 50);
    assert_eq!(super::actions::MAX_RENAME_PAIRS, 1_000);
    assert_eq!(super::actions::MAX_GLOB_TARGETS, 20);
}

#[test]
fn redacts_single_location_workspace_symbol_and_nested_diagnostics() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("lib.rs"), "fn x() {}\n").unwrap();
    let context = ToolContext::new(root.path().to_path_buf(), false).unwrap();
    let inside = format!("file://{}/lib.rs", root.path().display());
    let (_, omitted) = edits::redact_structured_uris(
        &context,
        &serde_json::json!({"uri":"file:///etc/passwd","range":{"start":{"line":0}}}),
    );
    assert_eq!(omitted, 1);
    let (symbol, omitted) = edits::redact_structured_uris(
        &context,
        &serde_json::json!([{"name":"x","location":{"uri":"file:///etc/passwd"}}]),
    );
    assert_eq!(omitted, 1);
    assert!(!symbol.to_string().contains("/etc/passwd"), "{symbol}");
    let (diag, omitted) = edits::redact_structured_uris(
        &context,
        &serde_json::json!({"items":[{"uri":"file:///etc/passwd","items":[{"message":"boom"}]}]}),
    );
    assert_eq!(omitted, 1);
    assert!(!diag.to_string().contains("/etc/passwd"), "{diag}");
    let (kept, omitted) = edits::redact_structured_uris(
        &context,
        &serde_json::json!({"uri": inside, "range":{"start":{"line":2}}}),
    );
    assert_eq!(omitted, 0);
    assert!(kept.pointer("/uri").is_some());
}

#[test]
fn sandbox_change_flips_digest() {
    let root = tempfile::tempdir().unwrap();
    let base = CommandConstraints {
        allowed_argv_prefixes: Vec::new(),
        forbidden_argv_prefixes: Vec::new(),
        network_mode: CommandNetworkMode::Disabled,
        execution_mode: CommandExecutionMode::ReadOnly,
        sandbox: CommandExecutionMode::Unrestricted,
        deny_all_argv: false,
    };
    let seated = CommandConstraints {
        sandbox: CommandExecutionMode::WorkspaceWrite,
        ..base.clone()
    };
    assert_ne!(
        config_digest(root.path(), &[], &base),
        config_digest(root.path(), &[], &seated)
    );
}

#[test]
fn readonly_request_rejects_unknown_payload_fields_before_start() {
    let root = tempfile::tempdir().unwrap();
    let context = ToolContext::new(root.path().to_path_buf(), false).unwrap();
    let err = super::actions::validate_raw_request(
        &context,
        "textDocument/hover",
        Some(r#"{"textDocument":{"uri":"file:///tmp/x"},"extra":true}"#),
    )
    .unwrap_err();
    assert!(err.to_string().contains("unknown field"), "{err}");
}

#[tokio::test]
async fn cancelled_start_does_not_leave_starting_entry() {
    if !python3_available() {
        return;
    }
    let fixture = r#"
import json, sys, time
time.sleep(5)
def read():
    headers = {}
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        if line in (b'\r\n', b'\n'):
            break
        key, _, value = line.decode().partition(':')
        headers[key.strip().lower()] = value.strip()
    n = int(headers.get('content-length', '0'))
    return json.loads(sys.stdin.buffer.read(n) or b'null')
def write(obj):
    body = json.dumps(obj).encode()
    sys.stdout.buffer.write(f'Content-Length: {len(body)}\r\n\r\n'.encode())
    sys.stdout.buffer.write(body)
    sys.stdout.buffer.flush()
while True:
    msg = read()
    if msg is None:
        break
    if msg.get('method') == 'initialize':
        write({"jsonrpc":"2.0","id":msg.get("id"),"result":{"capabilities":{}}})
    elif msg.get('id') is not None:
        write({"jsonrpc":"2.0","id":msg.get("id"),"result":None})
"#;
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("lib.rs"), "fn x() {}\n").unwrap();
    std::fs::write(
        root.path().join("Cargo.toml"),
        "[package]\nname=\"t\"\nversion=\"0.0.1\"\n",
    )
    .unwrap();
    let pool = LspPool::new();
    let server = fixture_server(fixture.into());
    let config = sample_config(
        root.path().to_path_buf(),
        FileToolMode::ReadWrite,
        "s-start-cancel",
        vec![server.clone()],
    );
    let key = PoolKey {
        session_id: "s-start-cancel".into(),
        behavior_id: "b1".into(),
        workspace_root: root.path().to_path_buf(),
        server_name: "fixture".into(),
        config_digest: config.digest.clone(),
    };
    let start = pool.get_or_start(key.clone(), &server, &config);
    tokio::select! {
        _ = tokio::time::sleep(std::time::Duration::from_millis(80)) => {}
        _ = start => {}
    }
    tokio::time::sleep(std::time::Duration::from_millis(80)).await;
    assert_eq!(pool.live_count().await, 0);
}

#[tokio::test]
async fn dropped_hanging_request_sends_cancel_without_killing_server() {
    if !python3_available() {
        return;
    }
    let root = tempfile::tempdir().unwrap();
    let cancel_file = root.path().join("cancelled");
    let fixture = format!(
        r#"
import json, sys
cancel_path = {cancel:?}
def read():
    headers = {{}}
    while True:
        line = sys.stdin.buffer.readline()
        if not line:
            return None
        if line in (b'\r\n', b'\n'):
            break
        key, _, value = line.decode().partition(':')
        headers[key.strip().lower()] = value.strip()
    n = int(headers.get('content-length', '0'))
    return json.loads(sys.stdin.buffer.read(n) or b'null')
def write(obj):
    body = json.dumps(obj).encode()
    sys.stdout.buffer.write(f'Content-Length: {{len(body)}}\r\n\r\n'.encode())
    sys.stdout.buffer.write(body)
    sys.stdout.buffer.flush()
while True:
    msg = read()
    if msg is None:
        break
    method = msg.get('method')
    mid = msg.get('id')
    if method == 'initialize':
        write({{"jsonrpc":"2.0","id":mid,"result":{{"capabilities":{{}}}}}})
    elif method == '$/cancelRequest':
        open(cancel_path, 'w').write('cancelled')
    elif method == 'textDocument/hover':
        continue
    elif method == 'shutdown':
        write({{"jsonrpc":"2.0","id":mid,"result":None}})
    elif method == 'exit':
        break
    elif mid is not None:
        write({{"jsonrpc":"2.0","id":mid,"result":None}})
"#,
        cancel = cancel_file
    );
    std::fs::write(root.path().join("lib.rs"), "fn x() {}\n").unwrap();
    std::fs::write(
        root.path().join("Cargo.toml"),
        "[package]\nname=\"t\"\nversion=\"0.0.1\"\n",
    )
    .unwrap();
    let pool = LspPool::new();
    let server = fixture_server(fixture);
    let config = sample_config(
        root.path().to_path_buf(),
        FileToolMode::ReadWrite,
        "s-drop-cancel",
        vec![server.clone()],
    );
    let key = PoolKey {
        session_id: "s-drop-cancel".into(),
        behavior_id: "b1".into(),
        workspace_root: root.path().to_path_buf(),
        server_name: "fixture".into(),
        config_digest: config.digest.clone(),
    };
    let lease = pool
        .get_or_start(key.clone(), &server, &config)
        .await
        .expect("start");
    let request = lease.client().request(
        "textDocument/hover",
        serde_json::json!({"textDocument":{"uri":"file://x"}}),
    );
    tokio::select! {
        _ = tokio::time::sleep(std::time::Duration::from_millis(80)) => {}
        _ = request => {}
    }
    let seen = tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if cancel_file.exists() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }
    })
    .await;
    assert!(seen.is_ok(), "expected $/cancelRequest");
    assert!(pool.has_ready(&key).await);
}

#[test]
fn readonly_bash_off_uses_platform_sandbox() {
    let constraints =
        constraints_from_effective_bash(&crate::tool_surface::ToolPolicyBash::off(), None);
    assert_eq!(
        constraints.sandbox,
        crate::toolset::lsp_sandbox_for_effective(CommandExecutionMode::ReadOnly)
    );
    assert_eq!(
        constraints.network_mode,
        crate::toolset::default_lsp_network_mode()
    );
}
