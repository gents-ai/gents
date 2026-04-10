use std::io;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use crossterm::event::{self, Event, KeyCode, KeyEvent, KeyModifiers};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::{Constraint, Direction, Layout};
use ratatui::style::{Modifier, Style};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Terminal;
use rig::completion::message::{
    AssistantContent, Message, ReasoningContent, Text, ToolResultContent, UserContent,
};
use rig::one_or_many::OneOrMany;
use serde::Deserialize;
use serde_json::Value;

pub(crate) async fn run(args: crate::TuiArgs) -> Result<()> {
    let home_dir = crate::resolve_home_dir(args.home.as_deref());
    let runtime_state = crate::read_runtime_state(&home_dir)?;
    let init_config = crate::read_init_config(&home_dir)?;
    let graphql = args
        .graphql
        .clone()
        .or_else(|| runtime_state.as_ref().map(|state| state.graphql.clone()))
        .unwrap_or_else(|| {
            format!(
                "http://127.0.0.1:{}/api/v0/graphql",
                crate::DEFAULT_HTTP_PORT
            )
        });
    let agent_name = args
        .agent_name
        .clone()
        .or_else(|| runtime_state.as_ref().map(|state| state.agent_name.clone()))
        .or_else(|| init_config.as_ref().map(|config| config.agent_name.clone()))
        .unwrap_or_else(|| crate::DEFAULT_AGENT_NAME.to_string());
    let agent_did = args
        .agent_did
        .clone()
        .or_else(|| runtime_state.as_ref().map(|state| state.agent_did.clone()))
        .or_else(|| init_config.as_ref().map(|config| config.agent_did.clone()))
        .unwrap_or_else(|| format!("did:defra-agent:{agent_name}"));
    let session_id = args
        .session_id
        .clone()
        .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());

    enable_raw_mode().context("enabling raw mode for TUI")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen).context("entering alternate screen")?;
    let _guard = TerminalGuard;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend).context("creating terminal backend")?;

    let mut app = App::new(
        graphql,
        agent_did,
        agent_name,
        session_id,
        args.behavior_id,
        Duration::from_millis(args.poll_ms),
        args.timeout_secs,
    );
    app.refresh().await?;

    loop {
        terminal.draw(|frame| draw_ui(frame, &app))?;
        if app.should_quit {
            break;
        }

        if event::poll(Duration::from_millis(50)).context("polling terminal events")? {
            match event::read().context("reading terminal event")? {
                Event::Key(key) => {
                    if let Err(error) = app.handle_key(key).await {
                        app.last_error = Some(error.to_string());
                    }
                }
                Event::Resize(_, _) => {}
                _ => {}
            }
        }

        if app.last_refresh_at.elapsed() >= app.poll_interval {
            if let Err(error) = app.refresh().await {
                app.last_error = Some(error.to_string());
            }
        }
    }

    Ok(())
}

struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let mut stdout = io::stdout();
        let _ = execute!(stdout, LeaveAlternateScreen);
    }
}

struct App {
    graphql: String,
    agent_did: String,
    agent_name: String,
    session_id: String,
    behavior_id: Option<String>,
    poll_interval: Duration,
    timeout_secs: u64,
    input: String,
    transcript: String,
    tool_text: String,
    reasoning_text: String,
    runtime_text: String,
    latest_response: Option<ResponseRow>,
    active_request: Option<crate::SubmittedRequest>,
    last_error: Option<String>,
    last_refresh_at: Instant,
    should_quit: bool,
    spinner_index: usize,
}

impl App {
    fn new(
        graphql: String,
        agent_did: String,
        agent_name: String,
        session_id: String,
        behavior_id: Option<String>,
        poll_interval: Duration,
        timeout_secs: u64,
    ) -> Self {
        Self {
            graphql,
            agent_did,
            agent_name,
            session_id,
            behavior_id,
            poll_interval,
            timeout_secs,
            input: String::new(),
            transcript: String::new(),
            tool_text: String::new(),
            reasoning_text: String::new(),
            runtime_text: String::new(),
            latest_response: None,
            active_request: None,
            last_error: None,
            last_refresh_at: Instant::now(),
            should_quit: false,
            spinner_index: 0,
        }
    }

    async fn handle_key(&mut self, key: KeyEvent) -> Result<()> {
        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.should_quit = true;
            }
            KeyCode::Char('q') if key.modifiers.is_empty() && self.input.trim().is_empty() => {
                self.should_quit = true;
            }
            KeyCode::Esc => {
                if self.input.is_empty() {
                    self.should_quit = true;
                } else {
                    self.input.clear();
                }
            }
            KeyCode::Enter if key.modifiers.contains(KeyModifiers::SHIFT) => {
                self.input.push('\n');
            }
            KeyCode::Enter => {
                self.submit_input().await?;
            }
            KeyCode::Backspace => {
                self.input.pop();
            }
            KeyCode::Tab => self.input.push('\t'),
            KeyCode::Char(ch) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.input.push(ch);
            }
            _ => {}
        }
        Ok(())
    }

    async fn submit_input(&mut self) -> Result<()> {
        let content = self.input.trim().to_string();
        if content.is_empty() {
            return Ok(());
        }
        if self.active_request.is_some() {
            self.last_error = Some("wait for the current turn to finish".to_string());
            return Ok(());
        }

        let submitted = crate::create_agent_request(
            &self.graphql,
            &self.agent_did,
            &content,
            Some(&self.session_id),
            self.behavior_id.as_deref(),
        )
        .await?;
        self.active_request = Some(submitted);
        self.input.clear();
        self.last_error = None;
        self.refresh().await?;
        Ok(())
    }

    async fn refresh(&mut self) -> Result<()> {
        let snapshot = load_snapshot(&self.graphql, &self.agent_did, &self.session_id).await?;
        self.transcript = render_transcript(&snapshot.messages, snapshot.response.as_ref());
        self.tool_text = render_tools(&snapshot.tools);
        self.reasoning_text = snapshot
            .response
            .as_ref()
            .map(|response| response.reasoning.trim().to_string())
            .filter(|text| !text.is_empty())
            .unwrap_or_else(|| "No persisted reasoning yet.".to_string());
        self.runtime_text =
            render_runtime(snapshot.runtime.as_ref(), &self.agent_name, &self.agent_did);
        self.latest_response = snapshot.response;
        if self
            .active_request
            .as_ref()
            .zip(self.latest_response.as_ref())
            .is_some_and(|(request, response)| {
                request.request_id == response.request_id
                    && matches!(response.status.as_str(), "complete" | "error")
            })
        {
            self.active_request = None;
        }
        self.last_refresh_at = Instant::now();
        self.spinner_index = self.spinner_index.wrapping_add(1);
        Ok(())
    }

    fn header_text(&self) -> String {
        let spinner = ['-', '\\', '|', '/'][self.spinner_index % 4];
        let request_status = match self.latest_response.as_ref() {
            Some(response) => response.status.as_str(),
            None => "idle",
        };
        let turn_state = if self.active_request.is_some() {
            format!("{spinner} {request_status}")
        } else {
            request_status.to_string()
        };
        format!(
            "agent={}  session={}  behavior={}  turn={}  timeout={}s",
            self.agent_name,
            truncate_middle(&self.session_id, 16),
            self.behavior_id.as_deref().unwrap_or("default"),
            turn_state,
            self.timeout_secs
        )
    }

    fn footer_text(&self) -> String {
        match self.last_error.as_deref() {
            Some(error) => format!(
                "Enter send | Shift+Enter newline | Esc clear/quit | Ctrl-C quit | error: {}",
                truncate_tail(error, 120)
            ),
            None => "Enter send | Shift+Enter newline | Esc clear/quit | Ctrl-C quit".to_string(),
        }
    }
}

struct Snapshot {
    messages: Vec<MessageRow>,
    tools: Vec<ToolRow>,
    response: Option<ResponseRow>,
    runtime: Option<RuntimeRow>,
}

#[derive(Debug, Clone, Deserialize)]
struct MessageRow {
    sequence: u64,
    role: String,
    content: String,
}

#[derive(Debug, Clone, Deserialize)]
struct ToolRow {
    tool_name: String,
    status: String,
    args: String,
    result: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct ResponseRow {
    request_id: String,
    status: String,
    content: String,
    #[serde(default)]
    reasoning: String,
    #[serde(default)]
    error_message: String,
}

#[derive(Debug, Clone, Deserialize)]
struct RuntimeRow {
    process_state: Option<String>,
    reconcile_phase: Option<String>,
    active_generation: Option<i64>,
    runnable_behavior_count: Option<i64>,
    unavailable_behavior_count: Option<i64>,
    last_reconcile_result: Option<String>,
}

async fn load_snapshot(graphql: &str, agent_did: &str, session_id: &str) -> Result<Snapshot> {
    let query = format!(
        r#"{{
            AgentMessage(
                filter: {{ session_id: {{ _eq: "{session_id}" }} }},
                order: {{ sequence: ASC }}
            ) {{
                sequence
                role
                content
            }}
            AgentToolCall(
                filter: {{ session_id: {{ _eq: "{session_id}" }} }},
                order: {{ started_at: ASC }}
            ) {{
                tool_name
                status
                args
                result
            }}
            AgentResponse(
                filter: {{ session_id: {{ _eq: "{session_id}" }} }},
                order: {{ created_at: DESC }},
                limit: 1
            ) {{
                request_id
                status
                content
                reasoning
                error_message
            }}
            AgentRuntime(
                filter: {{ agent_did: {{ _eq: "{agent_did}" }} }},
                limit: 1
            ) {{
                process_state
                reconcile_phase
                active_generation
                runnable_behavior_count
                unavailable_behavior_count
                last_reconcile_result
            }}
        }}"#,
        session_id = crate::escape_graphql_string(session_id),
        agent_did = crate::escape_graphql_string(agent_did),
    );

    let value = crate::post_graphql(graphql, &query).await?;
    Ok(Snapshot {
        messages: parse_rows(&value, "/data/AgentMessage")?,
        tools: parse_rows(&value, "/data/AgentToolCall")?,
        response: parse_rows::<ResponseRow>(&value, "/data/AgentResponse")?
            .into_iter()
            .next(),
        runtime: parse_rows::<RuntimeRow>(&value, "/data/AgentRuntime")?
            .into_iter()
            .next(),
    })
}

fn parse_rows<T: for<'de> Deserialize<'de>>(value: &Value, pointer: &str) -> Result<Vec<T>> {
    match value.pointer(pointer) {
        Some(rows) => Ok(serde_json::from_value(rows.clone())?),
        None => Ok(Vec::new()),
    }
}

fn draw_ui(frame: &mut ratatui::Frame, app: &App) {
    let area = frame.area();
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(12),
            Constraint::Length(6),
            Constraint::Length(2),
        ])
        .split(area);

    let body = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(68), Constraint::Percentage(32)])
        .split(layout[1]);
    let side = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(7),
            Constraint::Min(8),
            Constraint::Min(8),
        ])
        .split(body[1]);

    let header = Paragraph::new(app.header_text())
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("defra-agent tui"),
        )
        .wrap(Wrap { trim: false });
    frame.render_widget(header, layout[0]);

    let transcript = Paragraph::new(app.transcript.as_str())
        .block(Block::default().borders(Borders::ALL).title("Chat"))
        .wrap(Wrap { trim: false })
        .scroll((scroll_offset(&app.transcript, body[0].height), 0));
    frame.render_widget(transcript, body[0]);

    let runtime = Paragraph::new(app.runtime_text.as_str())
        .block(Block::default().borders(Borders::ALL).title("Runtime"))
        .wrap(Wrap { trim: false });
    frame.render_widget(runtime, side[0]);

    let tools = Paragraph::new(app.tool_text.as_str())
        .block(Block::default().borders(Borders::ALL).title("Tools"))
        .wrap(Wrap { trim: false })
        .scroll((scroll_offset(&app.tool_text, side[1].height), 0));
    frame.render_widget(tools, side[1]);

    let thinking_title = if app.latest_response.as_ref().is_some_and(|response| {
        response.status == "streaming" && !response.reasoning.trim().is_empty()
    }) {
        "Thinking (live)"
    } else {
        "Thinking"
    };
    let thinking = Paragraph::new(app.reasoning_text.as_str())
        .block(Block::default().borders(Borders::ALL).title(thinking_title))
        .wrap(Wrap { trim: false })
        .scroll((scroll_offset(&app.reasoning_text, side[2].height), 0));
    frame.render_widget(thinking, side[2]);

    let composer = Paragraph::new(app.input.as_str())
        .block(
            Block::default()
                .borders(Borders::ALL)
                .title("Compose")
                .title_style(Style::default().add_modifier(Modifier::BOLD)),
        )
        .wrap(Wrap { trim: false })
        .scroll((scroll_offset(&app.input, layout[2].height), 0));
    frame.render_widget(composer, layout[2]);

    let footer = Paragraph::new(app.footer_text())
        .block(Block::default().borders(Borders::ALL))
        .wrap(Wrap { trim: false });
    frame.render_widget(footer, layout[3]);
}

fn scroll_offset(text: &str, height: u16) -> u16 {
    let visible = height.saturating_sub(2) as usize;
    let lines = text.lines().count();
    lines.saturating_sub(visible) as u16
}

fn render_transcript(messages: &[MessageRow], response: Option<&ResponseRow>) -> String {
    let mut rendered = String::new();
    for row in messages {
        let message = decode_message(&row.role, &row.content);
        let body = render_message_body(&message);
        if body.trim().is_empty() {
            continue;
        }
        if !rendered.is_empty() {
            rendered.push_str("\n\n");
        }
        rendered.push_str(&format!("#{} {}", row.sequence, role_label(&row.role)));
        rendered.push('\n');
        rendered.push_str(&body);
    }

    if let Some(response) = response {
        if response.status == "streaming" && !response.content.trim().is_empty() {
            if !rendered.is_empty() {
                rendered.push_str("\n\n");
            }
            rendered.push_str("Assistant (draft)\n");
            rendered.push_str(response.content.trim());
        } else if response.status == "error" && !response.error_message.trim().is_empty() {
            if !rendered.is_empty() {
                rendered.push_str("\n\n");
            }
            rendered.push_str("Assistant (error)\n");
            rendered.push_str(response.error_message.trim());
        }
    }

    if rendered.is_empty() {
        "No conversation yet.\n\nType a message below and press Enter.".to_string()
    } else {
        rendered
    }
}

fn render_tools(tools: &[ToolRow]) -> String {
    if tools.is_empty() {
        return "No tool calls yet.".to_string();
    }

    let mut lines = Vec::new();
    for tool in tools.iter().rev().take(12).rev() {
        let mut line = format!(
            "[{}] {} {}",
            tool.status,
            tool.tool_name,
            format_tool_args_preview(&tool.args)
        );
        if matches!(tool.status.as_str(), "completed" | "error") {
            let result = tool.result.as_deref().unwrap_or("");
            if !result.trim().is_empty() {
                line.push_str(" => ");
                line.push_str(&truncate_tail(result.trim(), 140));
            }
        }
        lines.push(line);
    }
    lines.join("\n\n")
}

fn render_runtime(runtime: Option<&RuntimeRow>, agent_name: &str, agent_did: &str) -> String {
    let Some(runtime) = runtime else {
        return format!("agent: {agent_name}\ndid: {agent_did}\nruntime: unavailable");
    };

    format!(
        "agent: {agent_name}\ndid: {}\nstate: {}\nreconcile: {}\ngeneration: {}\nrunnable: {}\nunavailable: {}\nlast result: {}",
        truncate_middle(agent_did, 24),
        runtime.process_state.as_deref().unwrap_or("unknown"),
        runtime.reconcile_phase.as_deref().unwrap_or("unknown"),
        runtime
            .active_generation
            .map(|value| value.to_string())
            .unwrap_or_else(|| "n/a".to_string()),
        runtime
            .runnable_behavior_count
            .map(|value| value.to_string())
            .unwrap_or_else(|| "n/a".to_string()),
        runtime
            .unavailable_behavior_count
            .map(|value| value.to_string())
            .unwrap_or_else(|| "n/a".to_string()),
        runtime
            .last_reconcile_result
            .as_deref()
            .unwrap_or("n/a"),
    )
}

fn decode_message(role: &str, content: &str) -> Message {
    if let Ok(message) = serde_json::from_str::<Message>(content) {
        return message;
    }

    if role == "assistant" {
        if let Ok(content) = serde_json::from_str::<OneOrMany<AssistantContent>>(content) {
            return Message::Assistant { id: None, content };
        }
    }

    if role == "user" {
        if let Ok(content) = serde_json::from_str::<OneOrMany<UserContent>>(content) {
            return Message::User { content };
        }
    }

    match role {
        "assistant" => Message::Assistant {
            id: None,
            content: OneOrMany::one(AssistantContent::Text(Text {
                text: content.to_string(),
            })),
        },
        _ => Message::User {
            content: OneOrMany::one(UserContent::Text(Text {
                text: content.to_string(),
            })),
        },
    }
}

fn render_message_body(message: &Message) -> String {
    match message {
        Message::User { content } => content
            .iter()
            .map(|item| match item {
                UserContent::Text(text) => text.text.clone(),
                UserContent::ToolResult(tool_result) => tool_result
                    .content
                    .iter()
                    .filter_map(|content| match content {
                        ToolResultContent::Text(text) => Some(format!(
                            "[tool result] {}",
                            truncate_tail(text.text.trim(), 160)
                        )),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
                _ => "[non-text user content]".to_string(),
            })
            .filter(|item| !item.trim().is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        Message::Assistant { content, .. } => {
            let mut lines = Vec::new();
            for item in content.iter() {
                match item {
                    AssistantContent::Text(text) => {
                        if !text.text.trim().is_empty() {
                            lines.push(text.text.trim().to_string());
                        }
                    }
                    AssistantContent::ToolCall(tool_call) => {
                        lines.push(format!(
                            "[tool] {} {}",
                            tool_call.function.name,
                            truncate_tail(&tool_call.function.arguments.to_string(), 120)
                        ));
                    }
                    AssistantContent::Reasoning(reasoning) => {
                        let summary = render_reasoning_summary(reasoning);
                        if !summary.is_empty() {
                            lines.push(format!("[thinking] {}", truncate_tail(&summary, 120)));
                        }
                    }
                    _ => {}
                }
            }
            lines.join("\n")
        }
    }
}

fn render_reasoning_summary(reasoning: &rig::completion::message::Reasoning) -> String {
    let mut out = String::new();
    for item in &reasoning.content {
        let piece = match item {
            ReasoningContent::Text { text, .. } | ReasoningContent::Summary(text) => text.as_str(),
            ReasoningContent::Encrypted(_) => "[encrypted reasoning]",
            ReasoningContent::Redacted { .. } => "[redacted reasoning]",
            _ => "[opaque reasoning]",
        };
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(piece);
    }
    out
}

fn role_label(role: &str) -> &'static str {
    match role {
        "assistant" => "Assistant",
        _ => "You",
    }
}

fn truncate_tail(text: &str, max_chars: usize) -> String {
    let chars = text.chars().collect::<Vec<_>>();
    if chars.len() <= max_chars {
        return text.to_string();
    }
    let head = chars[..max_chars.saturating_sub(3)]
        .iter()
        .collect::<String>();
    format!("{head}...")
}

fn truncate_middle(text: &str, max_chars: usize) -> String {
    let chars = text.chars().collect::<Vec<_>>();
    if chars.len() <= max_chars {
        return text.to_string();
    }
    let keep = max_chars.saturating_sub(3);
    let front = keep / 2;
    let back = keep.saturating_sub(front);
    format!(
        "{}...{}",
        chars[..front].iter().collect::<String>(),
        chars[chars.len().saturating_sub(back)..]
            .iter()
            .collect::<String>()
    )
}

fn format_tool_args_preview(args: &str) -> String {
    if args.trim().is_empty() {
        return "()".to_string();
    }
    format!("({})", truncate_tail(args.trim(), 72))
}
