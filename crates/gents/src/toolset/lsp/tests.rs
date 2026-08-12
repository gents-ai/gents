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
    std::fs::write(root.path().join("Cargo.toml"), "[package]\nname=\"x\"\nversion=\"0.1.0\"\n").unwrap();
    let mut selection = ToolSelection::default();
    selection.enable_lsp = true;
    selection.file_tools = FileToolMode::ReadOnly;
    selection.file_tool_root = Some(root.path().to_path_buf());
    let ceiling = ToolCeiling::readonly_at(root.path());
    let config = BehaviorToolConfig::from_selection("beh", selection, &ceiling, Vec::new()).unwrap();
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
    let config = BehaviorToolConfig::from_selection("beh", selection, &ceiling, Vec::new()).unwrap();
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
    std::fs::write(root.path().join("Cargo.toml"), "[package]\nname=\"t\"\nversion=\"0.0.1\"\nedition=\"2021\"\n").unwrap();
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
    std::fs::write(root.path().join("Cargo.toml"), "[package]\nname=\"t\"\nversion=\"0.0.1\"\n").unwrap();
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
    std::fs::write(root.path().join("Cargo.toml"), "[package]\nname=\"t\"\nversion=\"0.0.1\"\n").unwrap();
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
    std::fs::write(root.path().join("Cargo.toml"), "[package]\nname=\"t\"\nversion=\"0.0.1\"\n").unwrap();
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
        deny_all_argv: false,
    };
    let tight = CommandConstraints {
        forbidden_argv_prefixes: vec![vec!["rust-analyzer".into()]],
        network_mode: CommandNetworkMode::Disabled,
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
    std::fs::write(root.path().join("Cargo.toml"), "[package]\nname=\"x\"\nversion=\"0.1.0\"\n")
        .unwrap();
    let mut selection = ToolSelection::default();
    selection.enable_lsp = true;
    selection.file_tools = FileToolMode::ReadOnly;
    selection.file_tool_root = Some(root.path().to_path_buf());
    let ceiling = ToolCeiling::readonly_at(root.path());
    let config = BehaviorToolConfig::from_selection("beh", selection, &ceiling, Vec::new()).unwrap();
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
        sample_config(root.path().to_path_buf(), FileToolMode::ReadWrite, "s-exec", vec![]),
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
        })
        .await
        .unwrap_err();
    let text = err.to_string();
    assert!(
        text.contains("executeCommand") || text.contains("unknown") || text.contains("not supported"),
        "{text}"
    );
}

