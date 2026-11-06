//! `gents shell` in a real PTY against a foreground runtime on allocated ports and a live model.
//! The shell redraws only changed cells, so a streamed reply can reach the PTY split across
//! frames, and color escapes hold digits; replies are read from the emulated screen instead.

use crate::support::*;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, ensure, Context, Result};
use gents_shell::app::App;
use gents_shell::keys::KeySet;
use gents_shell::layout::{conversation_area, layout, Screen};
use tui_test::{
    AutomaticRecording, AutomaticRecordingMode, ExecutionContext, LocatorExpectOptions,
    OpenOptions, Operation, OperationResult, RunOptions, Session, Timeouts, TraceMode,
    TraceOptions,
};

const ROWS: u16 = 40;
const COLS: u16 = 120;
const SHELL_UP: Duration = Duration::from_secs(60);
const REPLY: Duration = Duration::from_secs(120);
const KEYS: Duration = Duration::from_secs(20);
const PROMPT: &str = "What is 12 times 12? Reply with only the number.";
const ANSWER: &str = "144";

fn open_shell(home: &Path, shim_port: u16) -> Result<Session> {
    let home = home.to_str().context("home is not UTF-8")?;
    let traces = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("cli_shell_live");
    let context = ExecutionContext {
        trace: Some(TraceOptions {
            mode: TraceMode::OnFailure,
            directory: traces.clone(),
        }),
        ..ExecutionContext::default()
    };
    let wait = Some(KEYS.as_millis() as u64);
    let defaults = OpenOptions::default();
    let options = RunOptions {
        backend: defaults.backend,
        program: cli_bin().to_string(),
        args: [
            "shell",
            "--home",
            home,
            "--codex-remote",
            &format!("ws://127.0.0.1:{shim_port}/"),
        ]
        .map(String::from)
        .to_vec(),
        profile: defaults.profile,
        cols: COLS,
        rows: ROWS,
        cwd: Some(home.to_string()),
        env: [("HOME", home), ("RUST_LOG", "error")]
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .to_vec(),
        wait_ready: Some(false),
        restart: false,
        timeouts: Timeouts {
            text: wait,
            idle: wait,
            exit: wait,
            ..Timeouts::default()
        },
        recording: AutomaticRecording {
            mode: AutomaticRecordingMode::OnFailure,
            directory: Some(traces),
        },
        style: defaults.style,
    };
    let session = Session::new("gents-shell-live").with_execution_context(context);
    session.run(options).context("starting gents shell")?;
    Ok(session)
}

fn screen(session: &Session) -> String {
    match session.execute(Operation::Text { full: false }) {
        Ok(OperationResult::Text(text)) => text,
        other => format!("<no screen: {other:?}>"),
    }
}

fn wait_for(session: &Session, text: &str, within: Duration) -> Result<()> {
    let options = LocatorExpectOptions {
        not: false,
        timeout_ms: Some(within.as_millis() as u64),
    };
    session
        .get_by_text(text)
        .expect_with(options)
        .with_context(|| format!("waiting for {text:?}; screen:\n{}", screen(session)))
}

/// The rows of the conversation as this binary lays them out on a `COLS` by `ROWS` terminal.
fn conversation_rows(session: &Session) -> Result<Vec<String>> {
    let wanted = App::connected(KeySet::Vim, COLS, ROWS, std::sync::mpsc::channel().0).wanted();
    let Screen::Shell(panes) = layout(COLS, ROWS, wanted) else {
        bail!("{COLS}x{ROWS} is too small for the shell");
    };
    let area = conversation_area(panes.agent);
    let cells = match session.execute(Operation::Cells {
        x: area.x,
        y: area.y,
        w: area.width,
        h: area.height,
    })? {
        OperationResult::Cells(cells) => cells,
        other => bail!("Cells returned {other:?}"),
    };
    Ok(cells
        .chunk_by(|left, right| left.y == right.y)
        .map(|row| row.iter().map(|cell| cell.char.as_str()).collect())
        .collect())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires a live model at GENTS_CLI_E2E_MODEL_ENDPOINT"]
async fn the_shell_streams_a_live_reply_and_quits_cleanly() -> Result<()> {
    let model_endpoint = std::env::var("GENTS_CLI_E2E_MODEL_ENDPOINT")
        .unwrap_or_else(|_| DEFAULT_MODEL_ENDPOINT.to_string());
    let model_name = std::env::var("GENTS_CLI_E2E_MODEL_NAME")
        .unwrap_or_else(|_| DEFAULT_MODEL_NAME.to_string());
    let tempdir = tempfile::tempdir().context("creating tempdir")?;
    let home = tempdir.path().join("home");
    fs::create_dir_all(&home)?;
    let home_arg = home.to_str().context("home is not UTF-8")?;
    let init = run_init_json(
        &home,
        &[
            "--home",
            home_arg,
            "--node-name",
            "shell-live",
            "--model-name",
            &model_name,
            "--inference-url",
            &model_endpoint,
        ],
    )?;
    let node_did = node_did_from_init(&init)?;

    let port = allocate_port()?;
    let shim_port = allocate_port()?;
    let mut serve = spawn_server_with_env(
        &home,
        port,
        &[
            "--home",
            home_arg,
            "--codex-shim-port",
            &shim_port.to_string(),
            "--p2p-transport",
            "none",
        ],
        &[],
    )?;
    wait_for_port(port, &mut serve)?;
    serve
        .capturing(wait_for_runtime_ready(
            &graphql_url(port),
            &node_did,
            Duration::from_secs(30),
        ))
        .await?;
    wait_for_port(shim_port, &mut serve)?;

    let shell = open_shell(&home, shim_port)?;
    wait_for(&shell, "runtime ready", SHELL_UP)?;
    shell.execute(Operation::Write {
        data: format!("{PROMPT}\r"),
    })?;
    wait_for(&shell, ANSWER, REPLY)?;
    let rows = conversation_rows(&shell)?;
    ensure!(
        rows.iter().any(|row| row.contains(ANSWER)),
        "{ANSWER} is on screen but not in the conversation:\n{}",
        rows.join("\n")
    );

    shell.execute(Operation::Write {
        data: "\x1b".to_string(),
    })?;
    wait_for(&shell, "NORMAL", KEYS)?;
    shell.execute(Operation::Write {
        data: ":q\r".to_string(),
    })?;
    shell.execute(Operation::WaitExit { timeout_ms: None })?;
    let state = match shell.execute(Operation::State)? {
        OperationResult::State(state) => state,
        other => bail!("State returned {other:?}"),
    };
    ensure!(
        state.exited == Some(0),
        "exit {:?}:\n{}",
        state.exited,
        state.text
    );
    ensure!(
        !state.modes["alternate_screen"],
        "the shell left the alternate screen on:\n{}",
        state.text
    );
    Ok(())
}
