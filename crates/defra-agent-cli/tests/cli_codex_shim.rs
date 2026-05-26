mod support;
use support::*;

use std::fs;
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use codex_app_server_protocol as codex;
use futures_util::{SinkExt, StreamExt};
use serde::de::DeserializeOwned;
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};
use uuid::Uuid;

type ShimWebSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_shim_protocol_turn_streams_defra_response() -> Result<()> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let expected_reply = format!("codex-shim-ok-{}", Uuid::new_v4().simple());
    let model_name = format!("mock-codex-shim-model-{}", Uuid::new_v4().simple());
    let mock_endpoint = MockChatEndpoint::start(&model_name, &expected_reply)?;

    let server_port = allocate_port()?;
    let graphql = graphql_url(server_port);
    let agent_name = format!("cli-codex-shim-{}", Uuid::new_v4().simple());
    let init = run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            &model_name,
            mock_endpoint.endpoint(),
        ],
    )?;
    let agent_did = agent_did_from_init(&init)?;
    let mut serve = spawn_server(&home_dir, server_port)?;
    wait_for_port(server_port, &mut serve)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    let shim_port = allocate_port()?;
    let shim_port_string = shim_port.to_string();
    let mut shim = ServeProcess::new(spawn_cli(
        &home_dir,
        &[
            "codex-shim",
            "--port",
            &shim_port_string,
            "--poll-ms",
            "100",
            "--timeout-secs",
            "60",
        ],
    )?);
    wait_for_port(shim_port, &mut shim)?;

    let (mut ws, _) = connect_async(format!("ws://127.0.0.1:{shim_port}/"))
        .await
        .context("connecting to codex-shim websocket")?;

    send_client_request(
        &mut ws,
        codex::ClientRequest::Initialize {
            request_id: request_id(1),
            params: codex::InitializeParams {
                client_info: codex::ClientInfo {
                    name: "defra-agent-test".to_string(),
                    title: None,
                    version: env!("CARGO_PKG_VERSION").to_string(),
                },
                capabilities: None,
            },
        },
    )
    .await?;
    let initialize: codex::InitializeResponse = read_typed_response(&mut ws, request_id(1)).await?;
    assert!(
        initialize.user_agent.starts_with("defra-agent-codex-shim/"),
        "unexpected initialize response: {initialize:?}"
    );

    send_client_notification(&mut ws, codex::ClientNotification::Initialized).await?;

    send_client_request(
        &mut ws,
        codex::ClientRequest::ConfigRead {
            request_id: request_id(2),
            params: codex::ConfigReadParams {
                include_layers: false,
                cwd: None,
            },
        },
    )
    .await?;
    let config: codex::ConfigReadResponse = read_typed_response(&mut ws, request_id(2)).await?;
    assert_eq!(config.config.model.as_deref(), Some("defra-default"));

    send_client_request(
        &mut ws,
        codex::ClientRequest::ThreadStart {
            request_id: request_id(3),
            params: codex::ThreadStartParams {
                cwd: Some(home_dir.display().to_string()),
                ..Default::default()
            },
        },
    )
    .await?;
    let thread_start: codex::ThreadStartResponse =
        read_typed_response(&mut ws, request_id(3)).await?;
    let thread_id = thread_start.thread.id.clone();
    Uuid::parse_str(&thread_id)
        .with_context(|| format!("Codex TUI requires UUID thread ids, got {thread_id}"))?;

    let prompt = format!("Reply with exactly {}.", Uuid::new_v4().simple());
    send_client_request(
        &mut ws,
        codex::ClientRequest::TurnStart {
            request_id: request_id(4),
            params: codex::TurnStartParams {
                thread_id: thread_id.clone(),
                input: vec![codex::UserInput::Text {
                    text: prompt.clone(),
                    text_elements: Vec::new(),
                }],
                ..Default::default()
            },
        },
    )
    .await?;
    let turn_start: codex::TurnStartResponse = read_typed_response(&mut ws, request_id(4)).await?;
    assert_eq!(turn_start.turn.status, codex::TurnStatus::InProgress);

    let (final_text, completed_turn) = read_turn_to_completion(&mut ws).await?;
    assert_eq!(completed_turn.status, codex::TurnStatus::Completed);
    assert!(
        final_text.contains(&expected_reply),
        "expected streamed Codex text to contain {expected_reply}, got:\n{final_text}"
    );

    let (_request_id, session_id, _behavior_id) =
        wait_for_request(&graphql, &agent_did, &prompt).await?;
    assert_eq!(session_id, thread_id);
    let captured_requests = mock_endpoint.captured_chat_requests();
    assert!(
        captured_requests
            .iter()
            .any(|request| request_contains_role_text(request, "user", &prompt)),
        "mock endpoint did not receive the Codex prompt; captured={captured_requests:?}"
    );

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the configured real OpenAI-compatible backend"]
async fn codex_shim_live_protocol_uses_real_backend() -> Result<()> {
    let prompt_token = "PONGLIVE";
    let smoke = start_live_codex_shim().await?;
    let (mut ws, _) = connect_async(format!("ws://127.0.0.1:{}/", smoke.shim_port))
        .await
        .context("connecting to live codex-shim websocket")?;

    initialize_config_and_thread(&mut ws, &smoke.home_dir).await?;
    let thread_id = start_thread(&mut ws, &smoke.home_dir).await?;
    let prompt = format!("Reply with exactly this token and no extra words: {prompt_token}");
    send_turn(&mut ws, &thread_id, &prompt).await?;
    let (final_text, completed_turn) = read_turn_to_completion(&mut ws).await?;

    assert_eq!(completed_turn.status, codex::TurnStatus::Completed);
    assert!(
        final_text.contains(prompt_token),
        "expected live Codex protocol stream to contain {prompt_token}, got:\n{final_text}"
    );
    let (_request_id, session_id, _behavior_id) =
        wait_for_request(&smoke.graphql, &smoke.agent_did, &prompt).await?;
    assert_eq!(session_id, thread_id);
    assert_shim_trace_methods(
        &smoke.shim_trace,
        &["initialize", "config/read", "thread/start", "turn/start"],
    )?;

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires the configured real OpenAI-compatible backend"]
async fn codex_shim_live_protocol_supports_multiturn_memory() -> Result<()> {
    let memory_token = "LIME7";
    let smoke = start_live_codex_shim().await?;
    let (mut ws, _) = connect_async(format!("ws://127.0.0.1:{}/", smoke.shim_port))
        .await
        .context("connecting to live codex-shim websocket")?;

    initialize_config_and_thread(&mut ws, &smoke.home_dir).await?;
    let thread_id = start_thread(&mut ws, &smoke.home_dir).await?;

    let first_prompt = multiturn_first_prompt(memory_token);
    send_turn(&mut ws, &thread_id, &first_prompt).await?;
    let (_first_text, first_turn) = read_turn_to_completion(&mut ws).await?;
    assert_eq!(first_turn.status, codex::TurnStatus::Completed);
    let (_request_id, session_id, _behavior_id) =
        wait_for_request(&smoke.graphql, &smoke.agent_did, &first_prompt).await?;
    assert_eq!(session_id, thread_id);

    let second_prompt = "What project codeword did I give earlier in this conversation? Reply with exactly the codeword and no extra words.";
    send_turn(&mut ws, &thread_id, second_prompt).await?;
    let (second_text, second_turn) = read_turn_to_completion(&mut ws).await?;

    assert_eq!(second_turn.status, codex::TurnStatus::Completed);
    assert!(
        second_text.contains(memory_token),
        "expected second live Codex protocol turn to remember {memory_token}, got:\n{second_text}"
    );
    let (_request_id, session_id, _behavior_id) =
        wait_for_request(&smoke.graphql, &smoke.agent_did, second_prompt).await?;
    assert_eq!(session_id, thread_id);
    assert_shim_trace_methods(
        &smoke.shim_trace,
        &["initialize", "config/read", "thread/start"],
    )?;
    assert_shim_trace_method_count_at_least(&smoke.shim_trace, "turn/start", 2)?;

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires stock codex CLI, expect, and the configured real OpenAI-compatible backend"]
async fn stock_codex_remote_pty_smoke_uses_real_backend() -> Result<()> {
    require_command("codex")?;
    require_command("expect")?;
    let prompt_token = "PONGPTY";
    let smoke = start_live_codex_shim().await?;

    let transcript = smoke.tempdir.path().join("codex-pty.log");
    let expect_script = smoke.tempdir.path().join("codex-pty-smoke.expect");
    write_expect_smoke(
        &expect_script,
        &transcript,
        &smoke.codex_home,
        smoke.shim_port,
        prompt_token,
    )?;

    let output = Command::new("expect")
        .arg(&expect_script)
        .output()
        .context("running codex --remote PTY smoke through expect")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    let transcript = fs::read_to_string(&transcript).unwrap_or_default();
    if !output.status.success() {
        let (server_stdout, server_stderr) = smoke._server.captured_output()?;
        let (shim_stdout, shim_stderr) = smoke._shim.captured_output()?;
        let shim_trace = fs::read_to_string(&smoke.shim_trace).unwrap_or_default();
        bail!(
            "codex --remote PTY smoke failed\nstdout:\n{stdout}\nstderr:\n{stderr}\ntranscript:\n{transcript}\nserver stdout:\n{server_stdout}\nserver stderr:\n{server_stderr}\nshim stdout:\n{shim_stdout}\nshim stderr:\n{shim_stderr}\nshim trace:\n{shim_trace}"
        );
    }
    let token_search_text = terminal_token_search_text(&transcript);
    assert!(
        token_occurrences(&token_search_text, prompt_token) >= 2,
        "expected PTY transcript to contain an echoed prompt and assistant response for {prompt_token}\nstdout:\n{stdout}\nstderr:\n{stderr}\ntranscript:\n{transcript}"
    );
    let prompt = smoke_prompt(prompt_token);
    let (_request_id, _session_id, _behavior_id) =
        wait_for_request(&smoke.graphql, &smoke.agent_did, &prompt).await?;
    assert_shim_trace_methods(
        &smoke.shim_trace,
        &["initialize", "thread/start", "turn/start"],
    )?;

    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires tmux, stock codex CLI, and the configured real OpenAI-compatible backend"]
async fn stock_codex_remote_tmux_smoke_uses_real_backend() -> Result<()> {
    require_command("codex")?;
    if which("tmux").is_none() {
        eprintln!("skipping tmux smoke: tmux is not installed");
        return Ok(());
    }
    let prompt_token = "PONGTMUX";
    let smoke = start_live_codex_shim().await?;
    let session = format!("defra-codex-smoke-{}", Uuid::new_v4().simple());
    let command = format!(
        "CODEX_HOME={} codex --no-alt-screen --dangerously-bypass-approvals-and-sandbox --remote ws://127.0.0.1:{} {}",
        shell_quote_path(&smoke.codex_home),
        smoke.shim_port,
        shell_quote(&format!(
            "Reply with exactly this token and no extra words: {prompt_token}"
        )),
    );

    let new_status = Command::new("tmux")
        .args(["new-session", "-d", "-s", &session, &command])
        .status()
        .context("starting tmux codex smoke session")?;
    if !new_status.success() {
        bail!("tmux new-session failed");
    }
    let transcript =
        wait_for_tmux_token_occurrences(&session, prompt_token, 2, Duration::from_secs(180))?;
    let _ = Command::new("tmux")
        .args(["kill-session", "-t", &session])
        .status();
    let token_search_text = terminal_token_search_text(&transcript);
    assert!(
        token_occurrences(&token_search_text, prompt_token) >= 2,
        "expected tmux transcript to contain an echoed prompt and assistant response for {prompt_token}, got:\n{transcript}"
    );
    let prompt = smoke_prompt(prompt_token);
    let (_request_id, _session_id, _behavior_id) =
        wait_for_request(&smoke.graphql, &smoke.agent_did, &prompt).await?;
    assert_shim_trace_methods(
        &smoke.shim_trace,
        &["initialize", "thread/start", "turn/start"],
    )?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires tmux, stock codex CLI, and the configured real OpenAI-compatible backend"]
async fn stock_codex_remote_tmux_multiturn_uses_real_backend() -> Result<()> {
    require_command("codex")?;
    if which("tmux").is_none() {
        eprintln!("skipping tmux multi-turn smoke: tmux is not installed");
        return Ok(());
    }
    let memory_token = "LIME7";
    let transformed_token = "MINT7";
    let first_prompt = multiturn_first_prompt(memory_token);
    let second_prompt = multiturn_second_prompt();
    let smoke = start_live_codex_shim().await?;
    let session = format!("defra-codex-multiturn-{}", Uuid::new_v4().simple());
    let command = format!(
        "CODEX_HOME={} codex --no-alt-screen --dangerously-bypass-approvals-and-sandbox --remote ws://127.0.0.1:{} {}",
        shell_quote_path(&smoke.codex_home),
        smoke.shim_port,
        shell_quote(&first_prompt),
    );

    let new_status = Command::new("tmux")
        .args(["new-session", "-d", "-s", &session, &command])
        .status()
        .context("starting tmux codex multi-turn smoke session")?;
    if !new_status.success() {
        bail!("tmux new-session failed");
    }

    let result: Result<()> = async {
        wait_for_tmux_token_occurrences(&session, "READY", 2, Duration::from_secs(180))?;
        let literal_status = Command::new("tmux")
            .args(["send-keys", "-t", &session, "-l", second_prompt])
            .status()
            .context("sending second prompt to tmux codex session")?;
        if !literal_status.success() {
            bail!("tmux send-keys second prompt failed");
        }
        std::thread::sleep(Duration::from_millis(1500));
        let enter_status = Command::new("tmux")
            .args(["send-keys", "-t", &session, "Enter"])
            .status()
            .context("submitting second prompt in tmux codex session")?;
        if !enter_status.success() {
            bail!("tmux send-keys Enter failed");
        }

        let transcript = wait_for_tmux_token_occurrences(
            &session,
            transformed_token,
            1,
            Duration::from_secs(180),
        )?;
        let token_search_text = terminal_token_search_text(&transcript);
        assert!(
            token_occurrences(&token_search_text, transformed_token) >= 1,
            "expected tmux transcript to contain transformed multi-turn response {transformed_token}, got:\n{transcript}"
        );
        let (_request_id, first_session_id, _behavior_id) =
            wait_for_request(&smoke.graphql, &smoke.agent_did, &first_prompt).await?;
        let (_request_id, second_session_id, _behavior_id) =
            wait_for_request(&smoke.graphql, &smoke.agent_did, second_prompt).await?;
        assert_eq!(first_session_id, second_session_id);
        assert_shim_trace_methods(&smoke.shim_trace, &["initialize", "thread/start"])?;
        assert_shim_trace_method_count_at_least(&smoke.shim_trace, "turn/start", 2)?;
        Ok(())
    }
    .await;
    let _ = Command::new("tmux")
        .args(["kill-session", "-t", &session])
        .status();
    result?;
    Ok(())
}

struct LiveCodexShim {
    tempdir: tempfile::TempDir,
    home_dir: std::path::PathBuf,
    codex_home: std::path::PathBuf,
    graphql: String,
    agent_did: String,
    shim_port: u16,
    shim_trace: std::path::PathBuf,
    _server: ServeProcess,
    _shim: ServeProcess,
}

async fn start_live_codex_shim() -> Result<LiveCodexShim> {
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home_dir = tempdir.path().join("home");
    fs::create_dir_all(&home_dir)?;

    let server_port = allocate_port()?;
    let graphql = graphql_url(server_port);
    let agent_name = format!("cli-codex-live-{}", Uuid::new_v4().simple());
    let init = run_init_json(
        &home_dir,
        &[
            "--agent-name",
            &agent_name,
            "--model-name",
            DEFAULT_MODEL_NAME,
            DEFAULT_MODEL_ENDPOINT,
        ],
    )?;
    let agent_did = agent_did_from_init(&init)?;
    let mut server = spawn_server(&home_dir, server_port)?;
    wait_for_port(server_port, &mut server)?;
    wait_for_runtime_ready(&graphql, &agent_did, Duration::from_secs(30)).await?;

    let shim_port = allocate_port()?;
    let shim_port_string = shim_port.to_string();
    let shim_trace = tempdir.path().join("codex-shim.trace");
    let mut shim = spawn_logged_cli(
        &home_dir,
        &[
            "codex-shim",
            "--port",
            &shim_port_string,
            "--model",
            DEFAULT_MODEL_NAME,
            "--poll-ms",
            "250",
            "--timeout-secs",
            "180",
        ],
        "error,defra_agent_cli::commands::codex_shim=info",
        &[(
            "DEFRA_CODEX_SHIM_TRACE",
            shim_trace.to_string_lossy().as_ref(),
        )],
    )?;
    wait_for_port(shim_port, &mut shim)?;

    Ok(LiveCodexShim {
        codex_home: home_dir.join(".defra-agent").join("codex-ui"),
        tempdir,
        home_dir,
        graphql,
        agent_did,
        shim_port,
        shim_trace,
        _server: server,
        _shim: shim,
    })
}

async fn initialize_config_and_thread(
    ws: &mut ShimWebSocket,
    _home_dir: &std::path::Path,
) -> Result<()> {
    send_client_request(
        ws,
        codex::ClientRequest::Initialize {
            request_id: request_id(101),
            params: codex::InitializeParams {
                client_info: codex::ClientInfo {
                    name: "defra-agent-live-test".to_string(),
                    title: None,
                    version: env!("CARGO_PKG_VERSION").to_string(),
                },
                capabilities: None,
            },
        },
    )
    .await?;
    let _: codex::InitializeResponse = read_typed_response(ws, request_id(101)).await?;
    send_client_notification(ws, codex::ClientNotification::Initialized).await?;

    send_client_request(
        ws,
        codex::ClientRequest::ConfigRead {
            request_id: request_id(102),
            params: codex::ConfigReadParams {
                include_layers: false,
                cwd: None,
            },
        },
    )
    .await?;
    let _: codex::ConfigReadResponse = read_typed_response(ws, request_id(102)).await?;
    Ok(())
}

async fn start_thread(ws: &mut ShimWebSocket, home_dir: &std::path::Path) -> Result<String> {
    send_client_request(
        ws,
        codex::ClientRequest::ThreadStart {
            request_id: request_id(103),
            params: codex::ThreadStartParams {
                cwd: Some(home_dir.display().to_string()),
                ..Default::default()
            },
        },
    )
    .await?;
    let thread_start: codex::ThreadStartResponse = read_typed_response(ws, request_id(103)).await?;
    Ok(thread_start.thread.id)
}

async fn send_turn(ws: &mut ShimWebSocket, thread_id: &str, prompt: &str) -> Result<()> {
    send_client_request(
        ws,
        codex::ClientRequest::TurnStart {
            request_id: request_id(104),
            params: codex::TurnStartParams {
                thread_id: thread_id.to_string(),
                input: vec![codex::UserInput::Text {
                    text: prompt.to_string(),
                    text_elements: Vec::new(),
                }],
                ..Default::default()
            },
        },
    )
    .await?;
    let _: codex::TurnStartResponse = read_typed_response(ws, request_id(104)).await?;
    Ok(())
}

fn require_command(name: &str) -> Result<()> {
    if which(name).is_some() {
        Ok(())
    } else {
        bail!("{name} is required for this smoke test")
    }
}

fn spawn_logged_cli(
    home_dir: &std::path::Path,
    args: &[&str],
    rust_log: &str,
    envs: &[(&str, &str)],
) -> Result<ServeProcess> {
    let stdout_log = tempfile::NamedTempFile::new().context("creating defra-agent stdout log")?;
    let stderr_log = tempfile::NamedTempFile::new().context("creating defra-agent stderr log")?;
    let stdout = stdout_log
        .reopen()
        .context("opening defra-agent stdout log")?;
    let stderr = stderr_log
        .reopen()
        .context("opening defra-agent stderr log")?;
    let mut command = Command::new(cli_bin());
    command
        .env("HOME", home_dir)
        .env("RUST_LOG", rust_log)
        .current_dir(home_dir)
        .args(args)
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    for (name, value) in envs {
        command.env(name, value);
    }
    let child = command
        .spawn()
        .with_context(|| format!("spawning defra-agent {}", args.join(" ")))?;
    Ok(ServeProcess::with_logs(child, stdout_log, stderr_log))
}

fn which(name: &str) -> Option<std::path::PathBuf> {
    std::env::var_os("PATH")?
        .to_string_lossy()
        .split(':')
        .map(std::path::Path::new)
        .map(|dir| dir.join(name))
        .find(|candidate| candidate.exists())
}

fn write_expect_smoke(
    script: &std::path::Path,
    transcript: &std::path::Path,
    codex_home: &std::path::Path,
    shim_port: u16,
    prompt_token: &str,
) -> Result<()> {
    let prompt = smoke_prompt(prompt_token);
    let token_match_regex = tcl_regex_terminal_tolerant_literal(prompt_token);
    let contents = format!(
        r#"set timeout 120
set env(CODEX_HOME) {{{codex_home}}}
set env(TERM) xterm-256color
stty rows 40 columns 120
log_user 0
spawn codex --no-alt-screen --dangerously-bypass-approvals-and-sandbox --remote ws://127.0.0.1:{shim_port}/ {{{prompt}}}
log_file -a {{{transcript}}}
set match_count 0
expect {{
  -ex "\033\[6n" {{
    send "\033\[24;1R"
    exp_continue
  }}
  -ex "\033\[?u" {{
    send "\033\[?0u"
    exp_continue
  }}
  -ex "\033\[c" {{
    send "\033\[?1;2c"
    exp_continue
  }}
  -ex "\033]10;?\033\\" {{
    send "\033]10;rgb:ffff/ffff/ffff\033\\"
    exp_continue
  }}
  -ex "\033]11;?\033\\" {{
    send "\033]11;rgb:0000/0000/0000\033\\"
    exp_continue
  }}
  -re {{{token_match_regex}}} {{
    incr match_count
    if {{$match_count >= 2}} {{
      after 2000
      send "\003"
      expect {{
        eof {{ exit 0 }}
        timeout {{ exit 0 }}
      }}
    }}
    exp_continue
  }}
  timeout {{
    send "\003"
    expect {{
      eof {{ exit 0 }}
      timeout {{ exit 0 }}
    }}
  }}
  eof {{ exit 2 }}
}}
"#,
        transcript = tcl_brace(transcript),
        codex_home = tcl_brace(codex_home),
        prompt = tcl_brace_str(&prompt),
        token_match_regex = tcl_brace_str(&token_match_regex),
    );
    fs::write(script, contents).with_context(|| format!("writing {}", script.display()))
}

fn smoke_prompt(prompt_token: &str) -> String {
    format!("Reply with exactly this token and no extra words: {prompt_token}")
}

fn multiturn_first_prompt(memory_token: &str) -> String {
    format!(
        "The project codeword for this conversation is {memory_token}. Reply with exactly READY and no extra words."
    )
}

fn multiturn_second_prompt() -> &'static str {
    "Take the project codeword I gave earlier, replace LIME with MINT, keep the digit, and reply with exactly the transformed codeword and no extra words."
}

fn assert_shim_trace_methods(path: &std::path::Path, methods: &[&str]) -> Result<()> {
    let trace = fs::read_to_string(path)
        .with_context(|| format!("reading shim trace {}", path.display()))?;
    for method in methods {
        assert!(
            trace.contains(method),
            "expected shim trace to contain {method}, got:\n{trace}"
        );
    }
    Ok(())
}

fn assert_shim_trace_method_count_at_least(
    path: &std::path::Path,
    method: &str,
    minimum: usize,
) -> Result<()> {
    let trace = fs::read_to_string(path)
        .with_context(|| format!("reading shim trace {}", path.display()))?;
    let count = trace.matches(method).count();
    assert!(
        count >= minimum,
        "expected shim trace to contain {method} at least {minimum} times, got {count}:\n{trace}"
    );
    Ok(())
}

fn wait_for_tmux_token_occurrences(
    session: &str,
    needle: &str,
    required_count: usize,
    timeout: Duration,
) -> Result<String> {
    let deadline = std::time::Instant::now() + timeout;
    let mut last = String::new();
    loop {
        let output = Command::new("tmux")
            .args(["capture-pane", "-pt", session])
            .output()
            .context("capturing tmux pane")?;
        if output.status.success() {
            last = String::from_utf8_lossy(&output.stdout).into_owned();
            if token_occurrences(&terminal_token_search_text(&last), needle) >= required_count {
                return Ok(last);
            }
        }
        if std::time::Instant::now() >= deadline {
            bail!(
                "timed out waiting for {required_count} occurrences of {needle} in tmux pane; last transcript:\n{last}"
            );
        }
        std::thread::sleep(Duration::from_secs(1));
    }
}

fn shell_quote_path(path: &std::path::Path) -> String {
    shell_quote(&path.display().to_string())
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

fn tcl_brace(path: &std::path::Path) -> String {
    tcl_brace_str(&path.display().to_string())
}

fn tcl_brace_str(value: &str) -> String {
    value.replace('\\', r"\\").replace('}', r"\}")
}

fn tcl_regex_terminal_tolerant_literal(value: &str) -> String {
    let mut regex = String::from("(?s)");
    for (index, ch) in value.chars().enumerate() {
        if index > 0 {
            regex.push_str(".*");
        }
        if matches!(
            ch,
            '.' | '\\'
                | '+'
                | '*'
                | '?'
                | '['
                | '^'
                | ']'
                | '$'
                | '('
                | ')'
                | '{'
                | '}'
                | '='
                | '!'
                | '<'
                | '>'
                | '|'
                | ':'
                | '-'
        ) {
            regex.push('\\');
        }
        regex.push(ch);
    }
    regex
}

fn terminal_token_search_text(value: &str) -> String {
    terminal_visible_text(value)
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
        .collect()
}

fn token_occurrences(haystack: &str, needle: &str) -> usize {
    haystack.match_indices(needle).count()
}

fn terminal_visible_text(value: &str) -> String {
    let mut visible = String::new();
    let mut chars = value.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\u{1b}' {
            skip_escape_sequence(&mut chars);
        } else if ch == '\r' || ch == '\n' {
            visible.push('\n');
        } else if !ch.is_control() {
            visible.push(ch);
        }
    }
    visible
}

fn skip_escape_sequence<I>(chars: &mut std::iter::Peekable<I>)
where
    I: Iterator<Item = char>,
{
    match chars.peek().copied() {
        Some('[') => {
            chars.next();
            for ch in chars.by_ref() {
                if ('@'..='~').contains(&ch) {
                    break;
                }
            }
        }
        Some(']') => {
            chars.next();
            let mut saw_escape = false;
            for ch in chars.by_ref() {
                if ch == '\u{7}' || (saw_escape && ch == '\\') {
                    break;
                }
                saw_escape = ch == '\u{1b}';
            }
        }
        Some(_) => {
            chars.next();
        }
        None => {}
    }
}

fn request_id(value: i64) -> codex::RequestId {
    codex::RequestId::Integer(value)
}

async fn send_client_request(ws: &mut ShimWebSocket, request: codex::ClientRequest) -> Result<()> {
    let value = serde_json::to_value(request).context("serializing Codex client request")?;
    let request: codex::JSONRPCRequest =
        serde_json::from_value(value).context("building JSON-RPC request")?;
    write_jsonrpc(ws, codex::JSONRPCMessage::Request(request)).await
}

async fn send_client_notification(
    ws: &mut ShimWebSocket,
    notification: codex::ClientNotification,
) -> Result<()> {
    let value =
        serde_json::to_value(notification).context("serializing Codex client notification")?;
    let notification: codex::JSONRPCNotification =
        serde_json::from_value(value).context("building JSON-RPC notification")?;
    write_jsonrpc(ws, codex::JSONRPCMessage::Notification(notification)).await
}

async fn write_jsonrpc(ws: &mut ShimWebSocket, message: codex::JSONRPCMessage) -> Result<()> {
    let text = serde_json::to_string(&message).context("encoding JSON-RPC message")?;
    ws.send(WsMessage::Text(text.into()))
        .await
        .context("sending JSON-RPC websocket message")
}

async fn read_typed_response<T>(ws: &mut ShimWebSocket, expected_id: codex::RequestId) -> Result<T>
where
    T: DeserializeOwned,
{
    loop {
        match read_jsonrpc(ws).await? {
            codex::JSONRPCMessage::Response(response) if response.id == expected_id => {
                return serde_json::from_value(response.result)
                    .context("decoding typed Codex response");
            }
            codex::JSONRPCMessage::Error(error) if error.id == expected_id => {
                bail!(
                    "Codex shim returned error for request {}: {}",
                    expected_id,
                    error.error.message
                );
            }
            codex::JSONRPCMessage::Notification(_) => {}
            other => {
                bail!("unexpected JSON-RPC message while waiting for {expected_id}: {other:?}")
            }
        }
    }
}

async fn read_turn_to_completion(ws: &mut ShimWebSocket) -> Result<(String, codex::Turn)> {
    let mut text = String::new();
    loop {
        match read_jsonrpc(ws).await? {
            codex::JSONRPCMessage::Notification(notification) => {
                match server_notification_from_jsonrpc(notification)? {
                    codex::ServerNotification::AgentMessageDelta(delta) => {
                        text.push_str(&delta.delta);
                    }
                    codex::ServerNotification::TurnCompleted(completed) => {
                        return Ok((text, completed.turn));
                    }
                    _ => {}
                }
            }
            codex::JSONRPCMessage::Error(error) => {
                bail!("Codex shim emitted JSON-RPC error: {}", error.error.message);
            }
            codex::JSONRPCMessage::Request(request) => {
                bail!("Codex shim sent unexpected server request: {request:?}");
            }
            codex::JSONRPCMessage::Response(_) => {}
        }
    }
}

async fn read_jsonrpc(ws: &mut ShimWebSocket) -> Result<codex::JSONRPCMessage> {
    loop {
        let frame = tokio::time::timeout(Duration::from_secs(60), ws.next())
            .await
            .context("timed out waiting for Codex shim websocket message")?
            .ok_or_else(|| anyhow!("Codex shim websocket closed"))?
            .context("reading Codex shim websocket message")?;
        let text = match frame {
            WsMessage::Text(text) => text,
            WsMessage::Binary(bytes) => String::from_utf8(bytes.to_vec())
                .context("decoding binary websocket payload as UTF-8")?
                .into(),
            WsMessage::Ping(_) | WsMessage::Pong(_) => continue,
            WsMessage::Close(close) => bail!("Codex shim websocket closed: {close:?}"),
            WsMessage::Frame(_) => bail!("unexpected raw websocket frame"),
        };
        return serde_json::from_str(&text)
            .with_context(|| format!("decoding JSON-RPC message: {text}"));
    }
}

fn server_notification_from_jsonrpc(
    notification: codex::JSONRPCNotification,
) -> Result<codex::ServerNotification> {
    serde_json::from_value(serde_json::to_value(notification)?)
        .context("decoding Codex server notification")
}
