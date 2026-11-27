use base64::Engine as _;
use chrono::{DateTime, Local, NaiveDate};
use gents_observe::client_inference::AgentView;
use gents_observe::client_projection::canonical_output::{
    project_canonical_message, terminal_tool_output, CanonicalMessageProjection,
};
use gents_observe::client_projection::query::SessionTranscriptQueryPage;
use gents_observe::client_projection::store::{session_summary_entries, ClientStore};
use gents_protocol::background_completion::is_background_completion_notification_message_key;
use gents_protocol::client_protocol::ClientTurnState;
use gents_protocol::file_changes::{FileImage, ToolFileChanges};
use gents_protocol::message::{AssistantContent, Message, ToolResultContent, UserContent};
use gents_protocol::output::{
    MessageBlock, MessagePublication, MessageRole, PayloadPresentation, TranscriptMessage,
};
use gents_protocol::row::{AgentRequestRow, AgentToolCallRow, CompactionEntryRow};
use gents_protocol::tool_call::ToolCallState;
use gents_protocol::tool_names::{
    AGENT_MESSAGE_TOOL_NAME, AGENT_NEW_TOOL_NAME, SPAWN_PROCESS_TOOL_NAME,
};
use gents_protocol::transcript::present_message;
use gents_shell::app::transcript::{elapsed, ChatLine};
use gents_shell::sample::{Opens, RunState, Running, Tone};
use gents_shell::source::{Agent, Change, ChangeKind, Day, Session, SessionState};
use gents_tools::rendering::{command_streams, completion_metadata, CompletionMetadata};
use rapidhash::{HashMapExt, RapidHashMap};
use serde_json::Value;

use gents_session::session::caused::CausedSession;

/// The node's root sessions, newest activity first. A session an agent started is listed
/// beside its parent instead, and an archived one not at all.
pub(super) fn sessions(store: &ClientStore, node: &str, today: NaiveDate) -> Vec<Session> {
    let entries = session_summary_entries(&store.sessions, &store.requests, node);
    let roots = entries.into_iter().filter(|entry| {
        let started = entry.session.provenance.as_ref();
        entry.session.archived_at.is_none() && started.is_none_or(|by| by.started_by.is_none())
    });
    roots
        .map(|entry| {
            let session = entry.session;
            let observation = session.observation.as_ref();
            let preview = observation.and_then(|seen| seen.preview.as_deref());
            let name = session_title(session.title.as_ref().map(|title| &title.text), preview);
            let active = observation.map(|seen| seen.last_activity_at.as_str());
            Session {
                id: session.session_id.clone(),
                agent: session.agent_did.clone(),
                tab: name.clone(),
                name,
                day: day(active.unwrap_or(&session.created_at), today),
                state: match entry.turn_state {
                    Some(ClientTurnState::Running | ClientTurnState::WaitingForClaim) => {
                        SessionState::Active
                    }
                    Some(ClientTurnState::Failed) => SessionState::Error,
                    _ => SessionState::Idle,
                },
            }
        })
        .collect()
}

/// The session's title, or the first line of `preview`.
fn session_title(title: Option<&String>, preview: Option<&str>) -> String {
    let previewed = preview.and_then(|text| text.lines().next());
    [title.map(String::as_str), previewed]
        .into_iter()
        .flatten()
        .map(clean)
        .find(|name| !name.trim().is_empty())
        .unwrap_or_else(|| "untitled".to_string())
}

fn day(at: &str, today: NaiveDate) -> Day {
    let date = DateTime::parse_from_rfc3339(at).map(|at| at.with_timezone(&Local).date_naive());
    match date {
        Ok(date) if date == today => Day::Today,
        Ok(date) if Some(date) == today.pred_opt() => Day::Yesterday,
        _ => Day::Older,
    }
}

/// The enabled agents the runtime has made ready, its default first and marked so.
pub(super) fn agents(views: &[AgentView], ready: &[String]) -> Vec<Agent> {
    let runnable = views
        .iter()
        .filter(|view| view.enabled && ready.contains(&view.agent_did));
    let (default, rest): (Vec<_>, Vec<_>) = runnable.partition(|view| view.is_default);
    default
        .iter()
        .chain(&rest)
        .map(|view| {
            let name = agent_name(view);
            let text = match view.is_default {
                true => format!(" {name:<12} default"),
                false => format!(" {name}"),
            };
            Agent {
                id: view.agent_did.clone(),
                name,
                text,
                tone: Tone::Plain,
            }
        })
        .collect()
}

pub(super) fn agent_name(view: &AgentView) -> String {
    match clean(&view.display_name).trim() {
        "" => did_tail(&view.agent_did).to_string(),
        name => name.to_string(),
    }
}

fn did_tail(did: &str) -> &str {
    did.rsplit_once(':').map_or(did, |(_, tail)| tail)
}

/// A subagent's running row, named by its agent as the agent picker names agents.
pub(super) fn subagent(child: &CausedSession, agents: &[AgentView]) -> Running {
    let agent = agents.iter().find(|view| view.agent_did == child.agent_did);
    let name = agent.map_or_else(|| did_tail(&child.agent_did).to_string(), agent_name);
    use gents_protocol::request_lifecycle::RequestLifecycleState as Request;
    // A request parked on its backend resumes when the model answers again, so it has not ended.
    let (status, state) = match child.latest.lifecycle_state {
        Some(Request::WaitingForBackend | Request::ResumingBackend) => {
            ("waiting for its model", RunState::Working)
        }
        Some(Request::Failed | Request::Dead) => ("error", RunState::Failed),
        Some(state) if !state.is_terminal() => ("working", RunState::Working),
        _ => ("finished", RunState::Done),
    };
    Running {
        name,
        job: session_title(None, child.first.content.as_deref()),
        status: status.to_string(),
        state,
        opens: Opens::Agent(child.scope.session_id.clone()),
    }
}

/// An opened session's lines, and the sequence its next message takes.
#[derive(Debug, Default)]
pub(super) struct Transcript {
    pub(super) lines: Vec<ChatLine>,
    pub(super) next: u32,
}

/// The native file tools, the only ones whose file versions the runtime records.
const FILE_TOOLS: [&str; 2] = ["write_file", "edit_file"];

/// Each file edit of the page that has finished, as `(tool call, request)`: its recorded
/// changes can be read.
pub(super) fn finished_edits(
    page: &SessionTranscriptQueryPage,
    session: &str,
) -> Vec<(String, String)> {
    let view = page.store.transcript(session);
    let edits = view.tool_calls.into_iter().filter(|tool| {
        FILE_TOOLS.contains(&tool.tool_name.as_deref().unwrap_or_default())
            && state(tool) == Some(ToolCallState::Completed)
    });
    let ids = edits.filter_map(|tool| Some((tool.doc_id.clone()?, tool.request_id.clone()?)));
    ids.collect()
}

/// A recorded edit as the shell shows its changes, in turn `turn`.
pub(super) fn changes(recorded: &ToolFileChanges, turn: &str) -> Vec<Change> {
    let shown = recorded.changes.iter().map(|change| {
        let (before, after) = (text(&change.before), text(&change.after));
        let (kind, diff) = match (&change.before, &change.after) {
            (FileImage::Missing, _) => (ChangeKind::Add, after.unwrap_or_default()),
            (_, FileImage::Missing) => (ChangeKind::Delete, before.unwrap_or_default()),
            _ => match before.zip(after) {
                Some((before, after)) => (ChangeKind::Update, hunk(&before, &after)),
                None => (ChangeKind::Update, String::new()),
            },
        };
        Change::new(turn.to_string(), clean(&change.path), kind, clean(&diff))
    });
    shown.collect()
}

fn text(image: &FileImage) -> Option<String> {
    let FileImage::File { data, .. } = image else {
        return None;
    };
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .ok()?;
    String::from_utf8(bytes).ok()
}

/// One hunk from the first changed line to the last.
// ponytail: one hunk around every change; a line diff if edits far apart read poorly.
fn hunk(before: &str, after: &str) -> String {
    let (old, new): (Vec<_>, Vec<_>) = (before.lines().collect(), after.lines().collect());
    let head = old.iter().zip(&new).take_while(|(a, b)| a == b).count();
    let (old_rest, new_rest) = (&old[head..], &new[head..]);
    let same = |(a, b): &(&&str, &&str)| a == b;
    let tail = old_rest
        .iter()
        .rev()
        .zip(new_rest.iter().rev())
        .take_while(same)
        .count();
    let removed = &old_rest[..old_rest.len() - tail];
    let added = &new_rest[..new_rest.len() - tail];
    let range = |count: usize| match count {
        0 => format!("{head},0"),
        1 => (head + 1).to_string(),
        _ => format!("{},{count}", head + 1),
    };
    let mut diff = format!("@@ -{} +{} @@\n", range(removed.len()), range(added.len()));
    for line in removed {
        diff.push_str(&format!("-{line}\n"));
    }
    for line in added {
        diff.push_str(&format!("+{line}\n"));
    }
    diff
}

fn state(tool: &AgentToolCallRow) -> Option<ToolCallState> {
    let state = tool.lifecycle_state.as_deref().map(str::trim);
    state.and_then(ToolCallState::from_persisted)
}

/// Every line carries an item id: `m{sequence}` for a message's text, so a running turn's live
/// text, shown under the sequence its message will take, is replaced by it. A line under an
/// item (its output, one changed path) has its own, because a line with an id the tab has seen
/// replaces that line.
pub(super) fn transcript(
    page: &SessionTranscriptQueryPage,
    compactions: &[CompactionEntryRow],
    session: &str,
    edits: &RapidHashMap<String, Option<Vec<Change>>>,
) -> Transcript {
    let view = page.store.transcript(session);
    let dependencies = &page.canonical_dependencies;
    let segments = view.output_segments.iter().map(|row| (*row).clone());
    let segments: Vec<_> = segments
        .chain(dependencies.output_segments.iter().cloned())
        .collect();
    let denied = &dependencies.denied_segment_doc_ids;
    let denials = &dependencies.dependency_denials;
    let tools: RapidHashMap<&str, &AgentToolCallRow> = view
        .tool_calls
        .iter()
        .filter_map(|tool| Some((tool.doc_id.as_deref()?, *tool)))
        .collect();
    let mut messages = view.messages;
    messages.sort_by_key(|row| row.message.sequence);
    // A compaction shows after the last message it compacted.
    let through =
        |entry: &&CompactionEntryRow| entry.compacted_through_sequence.unwrap_or(i64::MAX);
    let mut compacted = compactions.iter().collect::<Vec<_>>();
    compacted.sort_by_key(through);
    let mut compacted = compacted.into_iter().peekable();
    let mut lines = Vec::new();
    let mut names = RapidHashMap::new();
    let mut subjects: RapidHashMap<String, String> = RapidHashMap::new();
    let mut delivered = Vec::new();
    let requests: RapidHashMap<&str, &AgentRequestRow> = page
        .requests
        .iter()
        .filter_map(|request| Some((request.doc_id.as_deref()?, request)))
        .collect();
    let worked_line = |turn: Option<&str>| {
        let request = turn.and_then(|done| requests.get(done)).copied();
        request.and_then(worked)
    };
    let mut turn = None;
    for row in &messages {
        let request = row.message.request_doc_id.as_deref();
        if request != turn {
            lines.extend(worked_line(turn));
            turn = request;
        }
        let sequence = i64::from(row.message.sequence);
        while let Some(entry) = compacted.next_if(|entry| through(entry) < sequence) {
            lines.push(compaction(entry));
        }
        if gents_protocol::output::rows::is_runtime_context_key(&row.message.message_key) {
            continue;
        }
        let CanonicalMessageProjection::Ready(message) =
            project_canonical_message(row, &segments, denied, denials)
        else {
            continue;
        };
        let id = format!("m{}", row.message.sequence);
        let shown = present_message(&message);
        let reasoning = shown.reasoning_markdown.as_deref().map(clean);
        let body = clean(&shown.body_markdown);
        match row.message.role {
            MessageRole::User if shown.has_tool_results => {
                for block in &row.message.blocks {
                    let MessageBlock::ToolResult {
                        tool_call_doc_id,
                        id: call,
                        ..
                    } = block
                    else {
                        continue;
                    };
                    delivered.push(tool_call_doc_id.clone());
                    let command = tools.get(tool_call_doc_id.as_str()).copied();
                    let named = names.get(tool_call_doc_id).map(String::as_str);
                    if is_command(named.unwrap_or_default(), command) {
                        let result = tool_result(&message, call);
                        lines.extend(output(tool_call_doc_id, &result));
                    }
                }
            }
            MessageRole::User
                if is_background_completion_notification_message_key(&row.message.message_key) =>
            {
                let Some((call, metadata)) = completion(&row.message) else {
                    continue;
                };
                let tool = tools.get(call.as_str()).copied();
                let parent = tool.and_then(|tool| tool.spawned_by_tool_call_doc_id.as_deref());
                let subject = subjects.get(&call).or_else(|| subjects.get(parent?));
                let subject = subject.unwrap_or(&metadata.tool_name);
                lines.push(notice(&metadata, subject, id.clone()));
                if let Some(tool) = tool {
                    if let Ok(Some(result)) = terminal_tool_output(tool, &segments, denied, denials)
                    {
                        lines.extend(output(&id, &result));
                    }
                }
                delivered.push(call);
            }
            MessageRole::User if shown.has_visible_body() => {
                lines.push(shown_line(body, Tone::You, id));
            }
            MessageRole::Assistant => {
                if let Some(reasoning) = reasoning.filter(|text| !text.trim().is_empty()) {
                    lines.push(shown_line(
                        reasoning,
                        Tone::Label,
                        format!("{id}/reasoning"),
                    ));
                }
                if shown.has_visible_body() {
                    lines.push(shown_line(body, Tone::Reply, id));
                }
                for block in &row.message.blocks {
                    let MessageBlock::ToolCall {
                        tool_call_doc_id,
                        id: call,
                        name,
                        ..
                    } = block
                    else {
                        continue;
                    };
                    names.insert(tool_call_doc_id.clone(), name.clone());
                    let tool = tools.get(tool_call_doc_id.as_str()).copied();
                    let arguments = tool_arguments(&message, call);
                    if let Some(subject) = subject(name, tool, &arguments) {
                        subjects.insert(tool_call_doc_id.clone(), subject);
                    }
                    let changed = edits.get(tool_call_doc_id).and_then(Option::as_deref);
                    let made = tool_lines(tool_call_doc_id, name, tool, &arguments, changed);
                    lines.extend(made);
                }
            }
            _ => {}
        }
    }
    // A spawned or background run has no result message: its own output is its result.
    for tool in view.tool_calls.iter().copied() {
        let Some(id) = tool.doc_id.as_deref() else {
            continue;
        };
        if delivered.iter().any(|done| done == id) || !names.contains_key(id) {
            continue;
        }
        if !is_command(&names[id], Some(tool)) {
            continue;
        }
        if let Ok(Some(result)) = terminal_tool_output(tool, &segments, denied, denials) {
            lines.extend(output(id, &result));
        }
    }
    lines.extend(worked_line(turn));
    lines.extend(compacted.map(compaction));
    let next = messages.iter().map(|row| row.message.sequence + 1).max();
    Transcript {
        lines,
        next: next.unwrap_or_default(),
    }
}

/// How long a finished request worked: from its claim to its end.
fn worked(request: &AgentRequestRow) -> Option<ChatLine> {
    let claimed = DateTime::parse_from_rfc3339(request.claimed_at.as_deref()?).ok()?;
    let ended = DateTime::parse_from_rfc3339(request.terminalized_at.as_deref()?).ok()?;
    let spent = (ended - claimed).to_std().ok()?;
    let id = format!("{}/worked", request.doc_id.as_deref()?);
    Some(shown_line(
        format!("  worked for {}", elapsed(spent)),
        Tone::Label,
        id,
    ))
}

fn compaction(entry: &CompactionEntryRow) -> ChatLine {
    let text = "  ✓ compacted the conversation".to_string();
    shown_line(text, Tone::Label, entry.compaction_key.clone())
}

fn tool_arguments(message: &Message, call: &str) -> Value {
    let Message::Assistant { content, .. } = message else {
        return Value::Null;
    };
    let called = content.iter().find_map(|item| match item {
        AssistantContent::ToolCall(tool) if tool.id == call => Some(&tool.function.arguments),
        _ => None,
    });
    called.cloned().unwrap_or(Value::Null)
}

fn tool_result(message: &Message, call: &str) -> String {
    let Message::User { content } = message else {
        return String::new();
    };
    let result = content.iter().find_map(|item| match item {
        UserContent::ToolResult(result) if result.id == call => Some(result),
        _ => None,
    });
    let texts = result.into_iter().flat_map(|result| &result.content);
    let texts = texts.filter_map(|item| match item {
        ToolResultContent::Text(text) => Some(text.text.as_str()),
        ToolResultContent::Image(_) => None,
    });
    texts.collect::<Vec<_>>().join("\n")
}

/// Commands, and the read-only file tools, which the shell shows as commands.
fn is_command(name: &str, tool: Option<&AgentToolCallRow>) -> bool {
    let background = tool.and_then(|tool| tool.await_mode.as_deref()) == Some("background");
    background
        || matches!(
            name,
            "bash" | "bash_unrestricted" | "read_file" | "list_files" | "glob" | "grep"
        )
}

fn tool_lines(
    id: &str,
    name: &str,
    tool: Option<&AgentToolCallRow>,
    arguments: &Value,
    changed: Option<&[Change]>,
) -> Vec<ChatLine> {
    let (mark, tone) = match tool.map(state) {
        None | Some(Some(ToolCallState::Pending | ToolCallState::Running)) => RUNNING,
        Some(Some(ToolCallState::Completed)) => DONE,
        Some(_) => FAILED,
    };
    let line = |text: String, tone| shown_line(text, tone, id.to_string());
    if name == AGENT_NEW_TOOL_NAME || name == AGENT_MESSAGE_TOOL_NAME {
        let first = first_prompt_line(name, arguments);
        return vec![line(format!("  ◆ {first}"), Tone::Label)];
    }
    if name == SPAWN_PROCESS_TOOL_NAME {
        if let Some(spawned) = subject(name, tool, arguments) {
            return vec![line(format!("  {mark} spawn {spawned}"), tone)];
        }
    }
    if is_command(name, tool) {
        return vec![line(
            format!("  {mark} run   {}", command(name, arguments)),
            tone,
        )];
    }
    if FILE_TOOLS.contains(&name) {
        let edited = |n: usize, path: String| ChatLine {
            text: format!("  {mark} edit  {path}"),
            tone,
            item: Some(format!("{id}/{n}")),
            change: None,
            under: None,
        };
        return match changed {
            Some(changes) => changes
                .iter()
                .enumerate()
                .map(|(n, change)| ChatLine {
                    change: Some(change.clone()),
                    ..edited(n, change.path.clone())
                })
                .collect(),
            None => {
                let path = clean(arguments["path"].as_str().unwrap_or_default());
                vec![edited(0, path)]
            }
        };
    }
    let picked = |selected: Option<&str>, fallback: &str| {
        let selected = selected.map(str::trim).filter(|value| !value.is_empty());
        clean(selected.unwrap_or(fallback))
    };
    let service = picked(
        tool.and_then(|tool| tool.selected_service_id.as_deref()),
        "gents",
    );
    let called = picked(
        tool.and_then(|tool| tool.selected_tool_name.as_deref()),
        name,
    );
    vec![line(format!("  {mark} {service} {called}"), tone)]
}

/// What a background call's completion names it by: an agent call's first prompt line, a
/// spawned process's command, or a command's own. A spawned process's completion is its
/// child's, which names it through this call.
fn subject(name: &str, tool: Option<&AgentToolCallRow>, arguments: &Value) -> Option<String> {
    if name == AGENT_NEW_TOOL_NAME || name == AGENT_MESSAGE_TOOL_NAME {
        return Some(first_prompt_line(name, arguments));
    }
    if name == SPAWN_PROCESS_TOOL_NAME {
        let spawned = arguments["tool_name"].as_str()?;
        return Some(command(spawned, &arguments["args"]));
    }
    is_command(name, tool).then(|| command(name, arguments))
}

fn first_prompt_line(name: &str, arguments: &Value) -> String {
    let field = if name == AGENT_NEW_TOOL_NAME {
        "prompt"
    } else {
        "message"
    };
    let prompt = arguments[field].as_str().unwrap_or_default();
    clean(prompt.lines().next().unwrap_or_default())
}

/// A command as it was run: its argv joined as a shell would read it.
fn command(name: &str, arguments: &Value) -> String {
    let program = arguments["command"]
        .as_str()
        .map(str::trim)
        .unwrap_or_default();
    if !program.is_empty() {
        let args = arguments["args"].as_array().into_iter().flatten();
        let argv = std::iter::once(program).chain(args.filter_map(Value::as_str));
        let joined = shlex::try_join(argv).unwrap_or_else(|_| program.to_string());
        return clean(&joined);
    }
    match arguments {
        Value::Null => format!("spawn_process {name}"),
        Value::Object(fields) if fields.is_empty() => format!("spawn_process {name}"),
        _ => clean(&format!("{name} {arguments}")),
    }
}

/// A command's output line, opening under its command's line `id`; none for no output. Its
/// first line is the runner's metadata when it starts `gents_exec:`, and of the streams after
/// it the line shows stdout, or stderr when stdout is empty.
fn output(id: &str, result: &str) -> Option<ChatLine> {
    let shown = match result.strip_prefix("gents_exec:") {
        Some(rest) => {
            let (header, body) = rest.split_once('\n').unwrap_or((rest, ""));
            let meta = serde_json::from_str::<Value>(header).ok();
            let streams = command_streams(body, meta.as_ref().and_then(Value::as_object));
            match streams.stdout.trim().is_empty() {
                true => streams.stderr,
                false => streams.stdout,
            }
        }
        None => result,
    };
    let shown = clean(shown);
    let shown = shown.trim_end();
    (!shown.is_empty()).then(|| ChatLine {
        under: Some(id.to_string()),
        ..shown_line(capped(shown), Tone::Label, format!("{id}/output"))
    })
}

/// The call a completion notification delivers to, and what its envelope says.
fn completion(message: &TranscriptMessage) -> Option<(String, CompletionMetadata)> {
    let MessagePublication::ToolDelivery { tool_call_doc_id } = &message.publication else {
        return None;
    };
    let metadata = message.blocks.iter().find_map(|block| match block {
        MessageBlock::Text { text } => match &text.presentation {
            PayloadPresentation::Composed { parts } => completion_metadata(parts),
            _ => None,
        },
        _ => None,
    })?;
    Some((tool_call_doc_id.clone(), metadata))
}

/// A background call's end, in its run line's columns.
fn notice(metadata: &CompletionMetadata, subject: &str, id: String) -> ChatLine {
    let ((mark, tone), verb) = match (metadata.status.as_str(), metadata.reason.as_deref()) {
        ("completed", _) => (DONE, "done"),
        ("cancelled", _) => (FAILED, "cancelled"),
        ("failed", Some("deadline_exceeded")) => (FAILED, "timed out"),
        _ => (FAILED, "failed"),
    };
    shown_line(format!("  {mark} {verb:<5} {}", clean(subject)), tone, id)
}

/// The running turn's reasoning and reply, under the sequence its message will take.
pub(super) fn live(next: u32, reasoning: &str, reply: &str) -> Vec<ChatLine> {
    let id = format!("m{next}");
    let lines = [
        (clean(reasoning), Tone::Label, format!("{id}/reasoning")),
        (clean(reply), Tone::Reply, id),
    ];
    let lines = lines.into_iter().filter(|(text, ..)| !text.is_empty());
    lines
        .map(|(text, tone, id)| shown_line(text, tone, id))
        .collect()
}

const RUNNING: (char, Tone) = ('…', Tone::Warn);
const DONE: (char, Tone) = ('✓', Tone::Label);
/// Failed, timed out or cancelled.
const FAILED: (char, Tone) = ('✗', Tone::Warn);

fn shown_line(text: String, tone: Tone, item: String) -> ChatLine {
    ChatLine {
        text,
        tone,
        item: Some(item),
        change: None,
        under: None,
    }
}

/// Lines of a command's output the chat keeps, and the characters it keeps of each: a page of
/// output would otherwise fill the tab, and every line the tab holds is wrapped to draw it.
const OUTPUT_LINES: usize = 20;
const OUTPUT_LINE_CHARS: usize = 400;

/// `output`'s first lines, each cut short, then how many more there were.
fn capped(output: &str) -> String {
    let mut all = output.lines();
    let mut shortened = false;
    let mut kept: Vec<String> = all
        .by_ref()
        .take(OUTPUT_LINES)
        .map(|line| match line.char_indices().nth(OUTPUT_LINE_CHARS) {
            Some((at, _)) => {
                shortened = true;
                format!("{}…", &line[..at])
            }
            None => line.to_string(),
        })
        .collect();
    let more = all.count();
    if more == 0 && !shortened {
        return output.to_string();
    }
    if more > 0 {
        let s = if more == 1 { "" } else { "s" };
        kept.push(format!("… {more} more line{s}"));
    }
    kept.join("\n")
}

/// The shell draws text as given, so a carriage return or a terminal escape would move its
/// cursor or restyle the screen. Drops both: CSI and OSC sequences whole, any other escape
/// with the character after it.
pub(super) fn clean(text: &str) -> String {
    gents_desktop_core::ansi::strip_ansi(text).replace('\r', "")
}

#[cfg(test)]
mod tests {
    use chrono::{Local, NaiveDate, TimeZone};
    use gents_observe::client_projection::query::SessionTranscriptQueryPage;
    use gents_observe::client_projection::store::{ClientStore, ClientStoreRows};
    use gents_protocol::file_changes::{
        FileChangeImages, FileImage, FilePermissions, ToolFileChanges, FILE_CHANGES_VERSION,
    };
    use gents_protocol::output::rows::{OutputSegmentRow, TranscriptMessageRow};
    use gents_protocol::output::{
        MessageBlock, MessagePublication, MessageRole, OutputSource, OutputWriter,
        PayloadPresentation, PayloadRef, PresentedPayload, StreamPayload, ToolResultPart,
    };
    use gents_protocol::row::{AgentRequestRow, AgentToolCallRow, CompactionEntryRow};
    use gents_protocol::session::AgentSession;
    use gents_shell::app::transcript::ChatLine;
    use gents_shell::sample::{Opens, RunState, Running, Tone};
    use gents_shell::source::{Change, ChangeKind, Day, SessionState};
    use rapidhash::{HashMapExt, RapidHashMap};
    use serde_json::{json, Value};

    use super::super::testing::{agent, closed, execution, header, presented, text_message, At};
    use super::*;
    use crate::commands::shell::tests::line;

    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 11, 5).unwrap()
    }

    fn noon(date: NaiveDate) -> String {
        let noon = date.and_hms_opt(12, 0, 0).unwrap();
        let at = Local.from_local_datetime(&noon).unwrap();
        at.to_rfc3339()
    }

    fn session(id: &str, created: &str, value: Value) -> AgentSession {
        let mut row = json!({
            "session_id": id, "node_did": "n", "requester_did": "u", "agent_did": "a",
            "created_at": created,
        });
        row.as_object_mut()
            .unwrap()
            .extend(value.as_object().unwrap().clone());
        serde_json::from_value(row).unwrap()
    }

    fn request(id: &str, session: &str, state: &str, content: &str, at: &str) -> AgentRequestRow {
        serde_json::from_value(json!({
            "_docID": format!("bae-{id}"), "request_id": id, "node_did": "n", "requester_did": "u",
            "agent_did": "a", "session_id": session, "lifecycle_state": state, "content": content,
            "created_at": at,
        }))
        .unwrap()
    }

    #[test]
    fn a_session_becomes_a_row_named_by_its_title_or_preview() {
        let yesterday = today().pred_opt().unwrap();
        let long_ago = today() - chrono::Days::new(3);
        let seen = |request: &str, state: &str, preview: &str, at: &str| {
            json!({ "observation": { "last_activity_at": at, "preview": preview,
                "latest_request": { "request_doc_id": format!("bae-{request}"),
                    "request_id": request, "lifecycle_state": state } } })
        };
        let mut titled = seen("r1", "processing", "ignored", &noon(today()));
        titled["title"] = json!({ "text": "fix login", "source": "user" });
        let started = json!({ "provenance": { "started_by": {
            "node_did": "n", "session_id": "s1", "requester_did": "u" } } });
        let archived = json!({ "archived_at": noon(today()) });
        let previewed = seen("r2", "failed", "first line\nsecond", &noon(yesterday));
        let rows = ClientStoreRows {
            sessions: vec![
                session("s1", &noon(long_ago), titled),
                session("s2", &noon(long_ago), previewed),
                session("s3", &noon(long_ago), json!({})),
                session("child", &noon(today()), started),
                session("gone", &noon(today()), archived),
            ],
            requests: vec![
                request("r1", "s1", "processing", "ignored", &noon(today())),
                request("r2", "s2", "failed", "first line\nsecond", &noon(yesterday)),
                request("r4", "child", "completed", "child work", &noon(today())),
            ],
            ..Default::default()
        };
        let store = ClientStore::from_rows(rows);

        let listed = sessions(&store, "n", today());

        let shown: Vec<_> = listed
            .iter()
            .map(|row| (row.id.as_str(), row.name.as_str(), row.day, row.state))
            .collect();
        assert_eq!(
            shown,
            [
                ("s1", "fix login", Day::Today, SessionState::Active),
                ("s2", "first line", Day::Yesterday, SessionState::Error),
                ("s3", "untitled", Day::Older, SessionState::Idle),
            ],
            "newest activity first; an agent's session sits beside its parent, and an archived one is gone"
        );
        assert_eq!(
            (listed[0].agent.as_str(), listed[0].tab.as_str()),
            ("a", "fix login")
        );
    }

    #[test]
    fn the_default_agent_leads_and_only_ready_enabled_agents_are_listed() {
        let mut reviewer = agent("did:key:z:reviewer", "reviewer", true, false);
        reviewer.display_name = String::new();
        let views = [
            reviewer,
            agent("did:key:z:default", "Engineer", true, true),
            agent("did:key:z:off", "off", false, false),
            agent("did:key:z:unready", "unready", true, false),
        ];
        let ready = ["did:key:z:reviewer", "did:key:z:default", "did:key:z:off"].map(String::from);

        let listed = agents(&views, &ready);

        let shown: Vec<_> = listed
            .iter()
            .map(|agent| (agent.id.as_str(), agent.name.as_str(), agent.text.as_str()))
            .collect();
        assert_eq!(
            shown,
            [
                ("did:key:z:default", "Engineer", " Engineer     default"),
                ("did:key:z:reviewer", "reviewer", " reviewer"),
            ]
        );
    }

    #[test]
    fn a_caused_session_becomes_a_subagent_row() {
        use gents_protocol::session::SessionScope;
        use gents_session::session::caused::CausedSession;
        let scope = |session: &str| SessionScope {
            node_did: "n".to_string(),
            session_id: session.to_string(),
            requester_did: Some("u".to_string()),
        };
        let child = |state: &str| CausedSession {
            scope: scope("c1"),
            agent_did: "did:key:z:researcher".to_string(),
            caused_by_scope: scope("s1"),
            depth: 1,
            first: request("r1", "c1", "completed", "look into it\nthen report", ""),
            latest: request("r2", "c1", state, "more", ""),
        };
        let row = |status: &str, state| Running {
            name: "Researcher".to_string(),
            job: "look into it".to_string(),
            status: status.to_string(),
            state,
            opens: Opens::Agent("c1".to_string()),
        };
        let named = [agent("did:key:z:researcher", "Researcher", true, false)];
        for (state, expected) in [
            ("pending", row("working", RunState::Working)),
            ("workspaceBindingPending", row("working", RunState::Working)),
            ("claimed", row("working", RunState::Working)),
            ("processing", row("working", RunState::Working)),
            (
                "waitingForBackend",
                row("waiting for its model", RunState::Working),
            ),
            (
                "resumingBackend",
                row("waiting for its model", RunState::Working),
            ),
            ("completed", row("finished", RunState::Done)),
            ("interrupted", row("finished", RunState::Done)),
            ("superseded", row("finished", RunState::Done)),
            ("failed", row("error", RunState::Failed)),
            ("dead", row("error", RunState::Failed)),
        ] {
            assert_eq!(subagent(&child(state), &named), expected, "{state}");
        }
        let unnamed = subagent(&child("completed"), &[]);
        assert_eq!(
            unnamed.name, "researcher",
            "named by its DID without an agent"
        );
    }

    fn at() -> At {
        At {
            node: "n".to_string(),
            session: "s1".to_string(),
            requester: Some("u".to_string()),
            request: "bae-r1".to_string(),
            generation: "g1".to_string(),
        }
    }

    /// A transcript page of the rows `push`ed into it.
    #[derive(Default)]
    struct Page {
        rows: ClientStoreRows,
        closes: usize,
        requests: Vec<AgentRequestRow>,
    }

    impl Page {
        fn segment(&mut self, segment: gents_protocol::output::OutputSegment) -> String {
            self.closes += 1;
            let doc_id = format!("close-{}", self.closes);
            let row = OutputSegmentRow {
                doc_id: doc_id.clone(),
                segment,
            };
            self.rows.output_segments.push(row);
            doc_id
        }

        fn message(&mut self, message: gents_protocol::output::TranscriptMessage) {
            let doc_id = format!("header-{}", message.sequence);
            let row = TranscriptMessageRow { doc_id, message };
            self.rows.transcript_messages.push(row);
        }

        fn text(&mut self, sequence: u32, role: MessageRole, text: &str) {
            let close = format!("close-{}", self.closes + 1);
            let (segment, message) = text_message(&at(), sequence, role, text, &close);
            self.segment(segment);
            self.message(message);
        }

        /// A call of `name` at `sequence` with `arguments`, in `state`, and its result in the
        /// next message when it has one.
        fn tool(&mut self, sequence: u32, id: &str, name: &str, arguments: Value, state: &str) {
            let payload = StreamPayload::ToolArguments {
                id: id.to_string(),
                call_id: None,
                name: name.to_string(),
            };
            let source = super::super::testing::turn_source(sequence);
            let segment = closed(
                &at(),
                source,
                execution(&at()),
                payload,
                &arguments.to_string(),
            );
            let close = self.segment(segment);
            let block = MessageBlock::ToolCall {
                tool_call_doc_id: id.to_string(),
                id: id.to_string(),
                call_id: None,
                name: name.to_string(),
                arguments: presented(&close).output,
                signature: None,
                additional_params: None,
            };
            let publication = MessagePublication::RequestExecution {
                execution_generation: "g1".to_string(),
            };
            let key = format!("turn:{sequence}");
            let call = header(
                &at(),
                &key,
                sequence,
                MessageRole::Assistant,
                publication,
                vec![block],
            );
            self.message(call);
            let row = json!({
                "_docID": id, "node_did": "n", "request_doc_id": "bae-r1", "tool_call_key": id,
                "session_id": "s1", "request_id": "r1", "requester_did": "u", "tool_name": name,
                "lifecycle_state": state,
            });
            self.rows
                .tool_calls
                .push(serde_json::from_value::<AgentToolCallRow>(row).unwrap());
        }

        fn result(&mut self, sequence: u32, id: &str, output: &str) {
            let source = OutputSource::ToolCall {
                tool_call_doc_id: id.to_string(),
            };
            self.delivery(sequence, id, source, output);
        }

        /// The receipt a background call's caller gets at once, written apart from its output.
        fn receipt(&mut self, sequence: u32, id: &str, text: &str) {
            let key = format!("receipt:{id}");
            self.delivery(sequence, id, OutputSource::Authored { key }, text);
        }

        /// Marks call `id` as run in the background.
        fn background(&mut self, id: &str) {
            let mut tools = self.rows.tool_calls.iter_mut();
            let tool = tools.find(|tool| tool.doc_id.as_deref() == Some(id));
            tool.expect("a call on the page").await_mode = Some("background".to_string());
        }

        /// The process `parent`'s spawn_process call started, as call `id` in `state`.
        fn spawned(&mut self, id: &str, parent: &str, state: &str) {
            let row = json!({
                "_docID": id, "node_did": "n", "request_doc_id": "bae-r1", "tool_call_key": id,
                "session_id": "s1", "request_id": "r1", "requester_did": "u", "tool_name": "bash",
                "lifecycle_state": state, "await_mode": "background",
                "spawned_by_tool_call_doc_id": parent,
            });
            let row = serde_json::from_value::<AgentToolCallRow>(row).unwrap();
            self.rows.tool_calls.push(row);
        }

        fn delivery(&mut self, sequence: u32, id: &str, source: OutputSource, output: &str) {
            let writer = OutputWriter::ToolExecution {
                tool_call_doc_id: id.to_string(),
            };
            let segment = closed(&at(), source, writer, StreamPayload::ToolOutput, output);
            let close = self.segment(segment);
            let block = MessageBlock::ToolResult {
                tool_call_doc_id: id.to_string(),
                id: id.to_string(),
                call_id: None,
                parts: vec![ToolResultPart::Text {
                    text: presented(&close),
                }],
            };
            let publication = MessagePublication::ToolDelivery {
                tool_call_doc_id: id.to_string(),
            };
            let key = format!("delivery:{id}");
            let delivered = header(
                &at(),
                &key,
                sequence,
                MessageRole::User,
                publication,
                vec![block],
            );
            self.message(delivered);
        }

        /// The completion notification the runtime delivers for background call `id`: a
        /// user message whose text composes the envelope around the call's output.
        fn notice(
            &mut self,
            sequence: u32,
            id: &str,
            status: &str,
            reason: Option<&str>,
            output: &str,
        ) {
            use gents_tools::rendering::{
                tool_completion_presentation, NOTIFICATION_SUMMARY_BYTES,
            };
            let source = OutputSource::ToolCall {
                tool_call_doc_id: id.to_string(),
            };
            let writer = OutputWriter::ToolExecution {
                tool_call_doc_id: id.to_string(),
            };
            let segment = closed(&at(), source, writer, StreamPayload::ToolOutput, output);
            let close_doc_id = self.segment(segment);
            let (_, parts) = tool_completion_presentation(
                id,
                "bash",
                status,
                output,
                reason,
                NOTIFICATION_SUMMARY_BYTES,
            );
            let block = MessageBlock::Text {
                text: PresentedPayload {
                    output: PayloadRef {
                        close_doc_id,
                        stream: 0,
                    },
                    presentation: PayloadPresentation::Composed { parts },
                },
            };
            let key =
                gents_protocol::background_completion::background_completion_notification_message_key(id, "tool");
            let publication = MessagePublication::ToolDelivery {
                tool_call_doc_id: id.to_string(),
            };
            let notice = header(
                &at(),
                &key,
                sequence,
                MessageRole::User,
                publication,
                vec![block],
            );
            self.message(notice);
        }

        /// The page's request, claimed at `claimed` and ended at `ended` when it has.
        fn claimed(&mut self, claimed: &str, ended: Option<&str>) {
            let mut row = request("r1", "s1", "completed", "hello", claimed);
            row.claimed_at = Some(claimed.to_string());
            row.terminalized_at = ended.map(str::to_string);
            self.requests.push(row);
        }

        fn read(self, compactions: &[CompactionEntryRow], edits: &Edits) -> Transcript {
            let page = SessionTranscriptQueryPage {
                store: ClientStore::from_rows(self.rows),
                canonical_dependencies: Default::default(),
                query_count: 0,
                queried_rows: 0,
                message_query_limit: 0,
                tool_call_query_limit: 0,
                source_exhausted: true,
                has_newer: false,
                covered: Default::default(),
                requests: self.requests,
                result_messages: Vec::new(),
            };
            transcript(&page, compactions, "s1", edits)
        }
    }

    type Edits = RapidHashMap<String, Option<Vec<Change>>>;

    /// `line`, opening under item `parent`.
    fn under(parent: &str, line: ChatLine) -> ChatLine {
        ChatLine {
            under: Some(parent.to_string()),
            ..line
        }
    }

    #[test]
    fn a_finished_request_ends_with_how_long_it_worked() {
        let mut page = Page::default();
        page.text(0, MessageRole::User, "hello");
        page.text(1, MessageRole::Assistant, "hi");
        let cargo = json!({ "command": "cargo", "args": ["test"] });
        page.tool(2, "t1", "bash", cargo, "completed");
        page.background("t1");
        let source = OutputSource::ToolCall {
            tool_call_doc_id: "t1".to_string(),
        };
        let writer = OutputWriter::ToolExecution {
            tool_call_doc_id: "t1".to_string(),
        };
        let ran = closed(&at(), source, writer, StreamPayload::ToolOutput, "ok");
        page.segment(ran);
        page.claimed("2026-11-26T12:00:00Z", Some("2026-11-26T12:01:23Z"));
        let read = page.read(&[], &Edits::new());
        let worked = line("  worked for 1m 23s", Tone::Label, "bae-r1/worked");
        let output = under("t1", line("ok", Tone::Label, "t1/output"));
        assert_eq!(read.lines[read.lines.len() - 2..], [output, worked]);

        let mut running = Page::default();
        running.text(0, MessageRole::User, "hello");
        running.claimed("2026-11-26T12:00:00Z", None);
        let read = running.read(&[], &Edits::new());
        assert!(read
            .lines
            .iter()
            .all(|shown| !shown.text.contains("worked for")));
    }

    #[test]
    fn a_spawned_process_shows_its_command() {
        let mut page = Page::default();
        let spawn = json!({
            "tool_name": "bash",
            "args": { "command": "cargo", "args": ["test", "-p", "gents"] },
        });
        page.tool(0, "p1", "spawn_process", spawn, "completed");
        let read = page.read(&[], &Edits::new());
        let spawned = line("  ✓ spawn cargo test -p gents", Tone::Label, "p1");
        assert_eq!(read.lines, [spawned]);
    }

    fn compacted(key: &str, through: i64) -> CompactionEntryRow {
        let entry = json!({ "compaction_key": key, "compacted_through_sequence": through });
        serde_json::from_value(entry).unwrap()
    }

    #[test]
    fn each_message_and_tool_call_maps_to_its_rows() {
        use Tone::{Label, Reply, Warn, You};
        let mut page = Page::default();
        page.text(0, MessageRole::User, "hello");
        page.text(1, MessageRole::Assistant, "hi\r\n\u{1b}[1mthere\u{1b}[0m");
        let bash = json!({ "command": "cargo", "args": ["test", "--all targets"] });
        page.tool(2, "t1", "bash", bash, "completed");
        page.result(
            3,
            "t1",
            "gents_exec: {\"exit_code\":0}\n\u{1b}[32mok\u{1b}[0m\r\n",
        );
        page.tool(4, "t2", "search", json!({ "query": "rust" }), "running");
        page.tool(5, "t3", "bash", json!({ "command": "false" }), "failed");
        page.result(6, "t3", "");
        page.tool(
            7,
            "t4",
            AGENT_NEW_TOOL_NAME,
            json!({ "prompt": "review it\nthen report" }),
            "completed",
        );
        let compactions = [compacted("k1", 4), compacted("k2", 99)];

        let read = page.read(&compactions, &Edits::new());

        let expected = vec![
            line("hello", You, "m0"),
            line("hi\nthere", Reply, "m1"),
            line("  ✓ run   cargo test '--all targets'", Label, "t1"),
            under("t1", line("ok", Label, "t1/output")),
            line("  … gents search", Warn, "t2"),
            line("  ✓ compacted the conversation", Label, "k1"),
            line("  ✗ run   false", Warn, "t3"),
            line("  ◆ review it", Label, "t4"),
            line("  ✓ compacted the conversation", Label, "k2"),
        ];
        assert_eq!(read.lines, expected);
        assert_eq!(read.next, 8, "the next message's sequence");
    }

    #[test]
    fn a_background_completion_shows_its_command_and_output() {
        use Tone::{Label, Reply, You};
        let mut page = Page::default();
        page.text(0, MessageRole::User, "run the tests in the background");
        let cargo = json!({ "command": "cargo", "args": ["test", "-p", "gents"] });
        page.tool(1, "t1", "bash", cargo, "completed");
        page.notice(2, "t1", "completed", None, "test result: ok. 412 passed");
        page.text(3, MessageRole::Assistant, "All 412 tests passed.");

        let read = page.read(&[], &Edits::new());

        let expected = vec![
            line("run the tests in the background", You, "m0"),
            line("  ✓ run   cargo test -p gents", Label, "t1"),
            line("  ✓ done  cargo test -p gents", Label, "m2"),
            under(
                "m2",
                line("test result: ok. 412 passed", Label, "m2/output"),
            ),
            line("All 412 tests passed.", Reply, "m3"),
        ];
        assert_eq!(read.lines, expected);
        let raw = read
            .lines
            .iter()
            .any(|shown| shown.text.contains("<tool-completion"));
        assert!(!raw, "the envelope never shows");
    }

    #[test]
    fn a_background_completion_names_how_it_ended() {
        use Tone::{Label, Warn, You};
        let cargo = || json!({ "command": "cargo", "args": ["test", "-p", "gents"] });
        for (status, reason, state, expected) in [
            (
                "cancelled",
                None,
                "cancelled",
                "  ✗ cancelled cargo test -p gents",
            ),
            (
                "failed",
                Some("deadline_exceeded"),
                "failed",
                "  ✗ timed out cargo test -p gents",
            ),
            (
                "failed",
                Some("tool_failed"),
                "failed",
                "  ✗ failed cargo test -p gents",
            ),
        ] {
            let mut page = Page::default();
            page.tool(0, "t1", "bash", cargo(), state);
            page.notice(1, "t1", status, reason, "boom");
            let read = page.read(&[], &Edits::new());
            assert_eq!(
                read.lines[1],
                line(expected, Warn, "m1"),
                "{status} {reason:?}"
            );
        }

        // A call not on the loaded page is named by its tool and has no output line.
        let mut page = Page::default();
        page.notice(0, "older", "completed", None, "ok");
        let read = page.read(&[], &Edits::new());
        assert_eq!(read.lines, vec![line("  ✓ done  bash", Label, "m0")]);

        // Only the message key makes a notice: typed envelope text stays the person's.
        let typed = "<tool-completion tool_call_id=\"x\" tool_name=\"bash\" status=\"completed\">\n  <result>ok</result>\n</tool-completion>";
        let mut page = Page::default();
        page.text(0, MessageRole::User, typed);
        assert_eq!(page.read(&[], &Edits::new()).lines[0].tone, You);
    }

    #[test]
    fn a_receipt_and_the_completion_keep_their_own_lines() {
        use Tone::Label;
        let mut page = Page::default();
        let cargo = json!({ "command": "cargo", "args": ["test", "-p", "gents"] });
        page.tool(0, "t1", "bash", cargo, "completed");
        page.background("t1");
        page.receipt(1, "t1", "continues in the background");
        page.notice(2, "t1", "completed", None, "test result: ok");

        let read = page.read(&[], &Edits::new());

        let expected = vec![
            line("  ✓ run   cargo test -p gents", Label, "t1"),
            under(
                "t1",
                line("continues in the background", Label, "t1/output"),
            ),
            line("  ✓ done  cargo test -p gents", Label, "m2"),
            under("m2", line("test result: ok", Label, "m2/output")),
        ];
        assert_eq!(read.lines, expected);
    }

    #[test]
    fn an_agent_call_completion_names_its_prompt() {
        let mut page = Page::default();
        let prompt = json!({ "prompt": "review it\nthen report" });
        page.tool(0, "t1", AGENT_NEW_TOOL_NAME, prompt, "completed");
        page.background("t1");
        page.notice(1, "t1", "completed", None, "reviewed");
        let read = page.read(&[], &Edits::new());
        assert_eq!(
            read.lines[1],
            line("  ✓ done  review it", Tone::Label, "m1")
        );
    }

    #[test]
    fn a_spawned_process_completion_names_its_command() {
        use Tone::Label;
        let mut page = Page::default();
        let spawn = json!({
            "tool_name": "bash",
            "args": { "command": "cargo", "args": ["test", "-p", "gents"] },
        });
        page.tool(0, "p1", "spawn_process", spawn, "completed");
        page.spawned("c1", "p1", "completed");
        page.notice(1, "c1", "completed", None, "test result: ok");
        let read = page.read(&[], &Edits::new());
        assert_eq!(
            read.lines[1..],
            [
                line("  ✓ done  cargo test -p gents", Label, "m1"),
                under("m1", line("test result: ok", Label, "m1/output")),
            ]
        );
    }

    fn file(path: &str, before: Option<&str>, after: Option<&str>) -> FileChangeImages {
        use base64::Engine as _;
        let image = |text: Option<&str>| match text {
            None => FileImage::Missing,
            Some(text) => FileImage::File {
                content_hash: String::new(),
                data: base64::engine::general_purpose::STANDARD.encode(text),
                permissions: FilePermissions {
                    readonly: false,
                    unix_mode: None,
                },
                links: None,
            },
        };
        FileChangeImages {
            root: "/srv/root".to_string(),
            path: path.to_string(),
            before: image(before),
            after: image(after),
        }
    }

    #[test]
    fn a_recorded_edit_becomes_its_changes_in_its_turn() {
        let recorded = ToolFileChanges {
            version: FILE_CHANGES_VERSION,
            tool_call_doc_id: "t1".to_string(),
            changes: vec![
                file("a.rs", Some("one\ntwo\nthree\n"), Some("one\n2\nthree\n")),
                file("b.rs", None, Some("x\n")),
                file("c.rs", Some("y\n"), None),
                file("d\u{1b}[2J.rs", Some("a\r\n"), Some("a\r\nb\r\n")),
            ],
        };

        let changes = changes(&recorded, "r1");

        let change = |path: &str, kind, diff: &str| {
            Change::new("r1".to_string(), path.to_string(), kind, diff.to_string())
        };
        assert_eq!(
            changes,
            [
                change("a.rs", ChangeKind::Update, "@@ -2 +2 @@\n-two\n+2\n"),
                change("b.rs", ChangeKind::Add, "x\n"),
                change("c.rs", ChangeKind::Delete, "y\n"),
                change("d.rs", ChangeKind::Update, "@@ -1,0 +2 @@\n+b\n"),
            ]
        );
        assert_eq!(changes[0].counts(), (1, 1));
    }

    #[test]
    fn a_file_edit_shows_its_recorded_changes_or_its_path_until_read() {
        let mut page = Page::default();
        page.tool(0, "t1", "edit_file", json!({ "path": "a.rs" }), "completed");
        page.tool(
            1,
            "t2",
            "write_file",
            json!({ "path": "new.rs" }),
            "running",
        );
        let changed = vec![
            Change::new(
                "r1".into(),
                "a.rs".into(),
                ChangeKind::Update,
                "@@ -1 +1 @@\n-a\n+b\n".into(),
            ),
            Change::new("r1".into(), "b.rs".into(), ChangeKind::Add, "x\n".into()),
        ];
        let mut edits = Edits::new();
        edits.insert("t1".to_string(), Some(changed.clone()));

        let read = page.read(&[], &edits);

        let edited = |text: &str, tone, item: &str, change: Option<&Change>| ChatLine {
            change: change.cloned(),
            ..line(text, tone, item)
        };
        assert_eq!(
            read.lines,
            [
                edited("  ✓ edit  a.rs", Tone::Label, "t1/0", Some(&changed[0])),
                edited("  ✓ edit  b.rs", Tone::Label, "t1/1", Some(&changed[1])),
                edited("  … edit  new.rs", Tone::Warn, "t2/0", None),
            ]
        );
    }

    #[test]
    fn command_output_shows_its_stdout_or_else_its_stderr() {
        let shown = |result: &str| output("t1", result).map(|line| line.text);
        let both = "gents_exec: {}\nstdout:\nran tests\nstderr:\nwarning: unused";
        assert_eq!(shown(both).as_deref(), Some("ran tests"));
        let failed = "gents_exec: {}\nstdout:\n(empty)\nstderr:\nerror: no such file";
        assert_eq!(shown(failed).as_deref(), Some("error: no such file"));
        assert_eq!(
            shown("gents_exec: {}\nstdout:\n(empty)\nstderr:\n(empty)"),
            None
        );
    }

    #[test]
    fn long_command_output_shows_its_first_lines_and_how_many_more() {
        let long: String = (1..=25).map(|n| format!("line {n}\n")).collect();
        let first: Vec<String> = (1..=20).map(|n| format!("line {n}")).collect();
        let capped = format!("{}\n… 5 more lines", first.join("\n"));
        assert_eq!(output("t1", &long).unwrap().text, capped);

        let wide = format!("{}\nend\n", "x".repeat(1000));
        let cut = format!("{}…\nend", "x".repeat(400));
        assert_eq!(output("t1", &wide).unwrap().text, cut);
        assert_eq!(output("t1", " \n"), None, "no output, no line");
    }

    #[test]
    fn a_running_turn_shows_under_the_sequence_its_message_takes() {
        assert_eq!(
            live(4, "weighing\r it", "Hel\u{1b}[1mlo"),
            [
                line("weighing it", Tone::Label, "m4/reasoning"),
                line("Hello", Tone::Reply, "m4"),
            ]
        );
        assert_eq!(live(4, "", "Hel"), [line("Hel", Tone::Reply, "m4")]);
    }
}
