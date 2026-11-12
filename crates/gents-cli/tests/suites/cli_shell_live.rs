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
const PROMPT: &str = "Give one short greeting in plain text. Do not call any tools.";

fn open_shell(home: &Path) -> Result<Session> {
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
        args: ["shell", "--home", home].map(String::from).to_vec(),
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
    expect_text(session, text, false, within)
}

fn wait_gone(session: &Session, text: &str, within: Duration) -> Result<()> {
    expect_text(session, text, true, within)
}

fn expect_text(session: &Session, text: &str, not: bool, within: Duration) -> Result<()> {
    let options = LocatorExpectOptions {
        not,
        timeout_ms: Some(within.as_millis() as u64),
    };
    let gone = if not { " to go" } else { "" };
    session
        .get_by_text(text)
        .expect_with(options)
        .with_context(|| format!("waiting for {text:?}{gone}; screen:\n{}", screen(session)))
}

/// The rows of the conversation as this binary lays them out on a `COLS` by `ROWS` terminal.
fn conversation_rows(session: &Session) -> Result<Vec<String>> {
    let wanted = App::connected(KeySet::Regular, COLS, ROWS, std::sync::mpsc::channel().0).wanted();
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

/// A foreground runtime on an allocated port, on a fresh home.
struct Live {
    home: PathBuf,
    node_did: String,
    graphql: String,
    _serve: ServeProcess,
    _tempdir: tempfile::TempDir,
}

async fn start_live(questions: bool) -> Result<Live> {
    let target = gents::support::live_inference::live_target().await;
    start_on(
        target.endpoint().to_owned(),
        target.model().to_owned(),
        questions,
    )
    .await
}

async fn start_on(model_endpoint: String, model_name: String, questions: bool) -> Result<Live> {
    let (tempdir, home, node_did, mut serve, port) = tokio::task::spawn_blocking(move || {
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
        if questions {
            let tools_id = init
                .pointer("/init/tools_id")
                .and_then(serde_json::Value::as_str)
                .context("init did not return tools_id")?;
            configure_questions(&home, &node_did, tools_id)?;
        }
        let port = allocate_port()?;
        let (serve, _) = spawn_server_with_ready_json(
            &home,
            port,
            &["--home", home_arg, "--p2p-transport", "none"],
            &[],
        )?;
        Ok::<_, anyhow::Error>((tempdir, home, node_did, serve, port))
    })
    .await??;
    let graphql = graphql_url(port);
    serve
        .capturing(wait_for_runtime_ready(
            &graphql,
            &node_did,
            Duration::from_secs(30),
        ))
        .await?;
    Ok(Live {
        home,
        node_did,
        graphql,
        _serve: serve,
        _tempdir: tempdir,
    })
}

async fn start_shell(live: &Live, prompt: String) -> Result<Session> {
    let home = live.home.clone();
    tokio::task::spawn_blocking(move || {
        let shell = open_shell(&home)?;
        wait_for(&shell, "runtime ready", SHELL_UP)?;
        shell.execute(Operation::Write {
            data: format!("{prompt}\r"),
        })?;
        Ok(shell)
    })
    .await?
}

fn runtime_access(live: &Live) -> gents_store::config_client::ConfigAccess {
    gents_store::config_client::ConfigAccess::Graphql(crate::support::graphql::served_endpoint(
        &live.graphql,
    ))
}

async fn request_reply(live: &Live, prompt: &str) -> Result<String> {
    use gents::support::live_inference::{
        terminal_assistant_answer, wait_for_request_terminal, wait_for_rows,
    };
    let access = runtime_access(live);
    let query = format!(
        r#"{{ AgentRequest(filter: {{ node_did: {{ _eq: "{}" }}, content: {{ _eq: "{}" }} }}) {{ request_id }} }}"#,
        escape_graphql_string(&live.node_did),
        escape_graphql_string(prompt)
    );
    let rows = wait_for_rows(&access, "AgentRequest", &query, REPLY, |data| {
        data["AgentRequest"]
            .as_array()
            .is_some_and(|rows| !rows.is_empty())
    })
    .await;
    let request = rows["AgentRequest"][0]["request_id"]
        .as_str()
        .context("request id missing")?;
    ensure!(
        wait_for_request_terminal(&access, request, REPLY).await == "completed",
        "shell request did not complete"
    );
    let answer = terminal_assistant_answer(&access, request).await;
    ensure!(
        !answer.trim().is_empty(),
        "shell request has no assistant reply"
    );
    Ok(answer)
}

fn quit_cleanly(shell: &Session) -> Result<()> {
    shell.execute(Operation::Write {
        data: "\x11".to_string(),
    })?;
    shell.execute(Operation::WaitExit {
        timeout_ms: Some(KEYS.as_millis() as u64),
    })?;
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "select GENTS_LIVE_TARGET and run the live CLI target"]
async fn the_shell_streams_a_live_reply_and_quits_cleanly() -> Result<()> {
    let live = start_live(false).await?;
    let shell = start_shell(&live, PROMPT.to_owned()).await?;
    let answer = request_reply(&live, PROMPT).await?;
    tokio::task::spawn_blocking(move || {
        let visible = answer
            .split_whitespace()
            .take(5)
            .collect::<Vec<_>>()
            .join(" ");
        wait_for(&shell, &visible, REPLY)?;
        let rows = conversation_rows(&shell)?;
        let rendered = rows
            .join(" ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let expected = answer.split_whitespace().collect::<Vec<_>>().join(" ");
        ensure!(
            rendered.contains(&expected),
            "assistant reply is absent from the conversation: {rendered}"
        );
        quit_cleanly(&shell)
    })
    .await?
}

fn configure_questions(home: &Path, node_did: &str, previous_tools: &str) -> Result<()> {
    use gents_documents::mailbox_write_decl::{
        canonical_mailbox_write_decl, MailboxAction, MailboxKind, MailboxNotificationPolicy,
    };
    use gents_documents::{DatastoreToolSurfaceDocument, DatastoreTools, SurfaceToolDecl, Tools};
    let mut declaration = canonical_mailbox_write_decl();
    declaration.notification = Some(MailboxNotificationPolicy {
        kind: MailboxKind::Ask,
        action: MailboxAction::StartRequest,
        ..Default::default()
    });
    let tools = Tools {
        node_did: node_did.into(),
        tools_id: "shell-question-tools".into(),
        datastore: Some(DatastoreTools {
            datastore_tool_surface_ids: Some(vec!["shell-questions".into()]),
            ..Default::default()
        }),
        ..Default::default()
    };
    let surface = DatastoreToolSurfaceDocument {
        surface_id: "shell-questions".into(),
        node_did: node_did.into(),
        display_name: None,
        enabled: true,
        entries: Some(vec![SurfaceToolDecl::Create(declaration)]),
        created_at: None,
        tags: vec![],
    };
    let directory = home.join("question-config");
    let root = directory
        .to_str()
        .context("configuration root is not UTF-8")?;
    let home_arg = home.to_str().context("home is not UTF-8")?;
    run_cli_text(
        home,
        &["config", "export", "--home", home_arg, "--root", root],
    )?;
    let path = directory.join("pack_config.json");
    let mut config = read_json_file(&path)?;
    let contexts = config["contexts"]
        .as_array_mut()
        .context("exported contexts missing")?;
    let context = contexts
        .iter_mut()
        .find(|context| context["tools_id"] == previous_tools)
        .context("default Agent context missing from configuration export")?;
    context["tools_id"] = serde_json::json!("shell-question-tools");
    context["system_prompt"] = serde_json::json!("Follow the user's requested tool call exactly. After it succeeds, acknowledge briefly without calling it again. When notified of the user's answer, acknowledge it without tools.");
    config["tools"]
        .as_array_mut()
        .context("exported Tools missing")?
        .push(serde_json::to_value(tools)?);
    let surface = serde_json::to_value(surface)?;
    match config
        .get_mut("datastore_tool_surfaces")
        .and_then(serde_json::Value::as_array_mut)
    {
        Some(surfaces) => surfaces.push(surface),
        None => config["datastore_tool_surfaces"] = serde_json::json!([surface]),
    }
    write_json_file(&path, &config)?;
    run_cli_text(
        home,
        &["config", "apply", "--home", home_arg, "--root", root],
    )?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "select GENTS_LIVE_TARGET and run the live CLI target"]
async fn the_shell_answers_a_question_and_the_item_is_acted() -> Result<()> {
    use gents::support::live_inference::wait_for_rows;
    use gents_protocol::mailbox_question::{
        MailboxQuestion, MailboxQuestionOption, MAILBOX_QUESTION_VERSION,
    };
    let live = start_live(true).await?;
    let question = MailboxQuestion {
        version: MAILBOX_QUESTION_VERSION,
        prompt: "Which channel ships?".into(),
        options: [("stable", "Stable"), ("beta", "Beta")]
            .into_iter()
            .map(|(id, label)| MailboxQuestionOption {
                id: id.into(),
                label: label.into(),
                description: None,
                prompt: None,
            })
            .collect(),
        multi_select: false,
        allow_free_text: false,
        optional: Some(false),
        recommended_order: None,
    };
    let arguments =
        serde_json::json!({"title":"Release channel", "payload":serde_json::to_string(&question)?});
    let prompt = format!("Call file_mailbox_item with exactly these arguments: {arguments}. Then acknowledge briefly and do not call the tool again. When a decision arrives, acknowledge it without calling tools.");
    let shell = start_shell(&live, prompt.clone()).await?;
    request_reply(&live, &prompt).await?;
    let access = runtime_access(&live);
    let rows = wait_for_rows(
        &access,
        "MailboxItem",
        "{ MailboxItem { _docID title status } }",
        REPLY,
        |data| {
            data["MailboxItem"]
                .as_array()
                .is_some_and(|rows| rows.iter().any(|row| row["title"] == "Release channel"))
        },
    )
    .await;
    let item = rows["MailboxItem"]
        .as_array()
        .context("mailbox rows missing")?
        .iter()
        .find(|row| row["title"] == "Release channel")
        .and_then(|row| row["_docID"].as_str())
        .context("question document missing")?
        .to_owned();
    let question_shell = shell.clone();
    tokio::task::spawn_blocking(move || -> Result<()> {
        wait_for(&question_shell, "! Release channel", KEYS)?;
        wait_for(&question_shell, "Which channel ships?", KEYS)?;
        wait_for(&question_shell, "type 1-2", KEYS)?;
        question_shell.execute(Operation::Write { data: "1\r".into() })?;
        Ok(())
    })
    .await??;
    let query = format!(
        r#"{{ MailboxItem(filter: {{ _docID: {{ _eq: "{}" }} }}) {{ status }} }}"#,
        escape_graphql_string(&item)
    );
    wait_for_rows(&access, "MailboxItem", &query, REPLY, |data| {
        data["MailboxItem"][0]["status"] == "acted"
    })
    .await;
    tokio::task::spawn_blocking(move || {
        wait_for(&shell, "Decision on", KEYS)?;
        wait_gone(&shell, "! Release channel", KEYS)?;
        quit_cleanly(&shell)
    })
    .await?
}

fn press(shell: &Session, keys: &[&str]) -> Result<()> {
    for key in keys {
        shell.execute(Operation::Write {
            data: key.to_string(),
        })?;
    }
    Ok(())
}

/// A home whose model does not answer opens on Setup, where a provider added by hand makes
/// its agent ready, and the open shell then sends to it. The model is a local stand-in.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "drives a PTY and a foreground runtime; needs no live model"]
async fn the_shell_opens_on_setup_without_a_model_and_wakes_when_one_is_added() -> Result<()> {
    const DOWN: &str = "\x1b[B";
    const REPLY_TEXT: &str = "pong from the stand-in";
    let live = start_on(
        "http://127.0.0.1:9/v1".to_owned(),
        "no-model".to_owned(),
        false,
    )
    .await?;
    let home = live.home.clone();
    tokio::task::spawn_blocking(move || {
        let model = MockChatEndpoint::start("stand-in", REPLY_TEXT)?;
        let shell = open_shell(&home)?;
        wait_for(&shell, "Add a provider", SHELL_UP)?;
        wait_for(&shell, "cannot be reached, so it cannot answer", KEYS)?;
        wait_for(&shell, "no agent ready", KEYS)?;
        wait_for(&shell, "Local", KEYS)?;

        // Local is the fourth provider; its one method asks for a key, then the endpoint.
        press(&shell, &[DOWN, DOWN, DOWN, "\r"])?;
        wait_for(&shell, "Local: Endpoint + optional key", KEYS)?;
        press(&shell, &["\t", "\x15", model.endpoint(), "\r"])?;
        wait_for(&shell, "Choose a model", KEYS)?;
        wait_for(&shell, "stand-in", KEYS)?;
        press(&shell, &["\r"])?;
        wait_for(&shell, "enter save", KEYS)?;
        press(&shell, &["\r"])?;

        wait_for(&shell, "runtime ready", SHELL_UP)?;
        wait_gone(&shell, "Add a provider", KEYS)?;
        shell.execute(Operation::Write {
            data: format!("{PROMPT}\r"),
        })?;
        wait_for(&shell, REPLY_TEXT, REPLY)?;

        // A fresh home has the regular keys, where ctrl-q quits.
        press(&shell, &["\x11"])?;
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
        Ok(())
    })
    .await?
}
