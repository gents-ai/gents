use chrono::{Local, NaiveDate, TimeZone};
use gents_codex_protocol as codex;
use gents_shell::app::ChatLine;
use gents_shell::sample::Tone;
use gents_shell::source::{Agent, Day, Session, SessionState, Update};

use crate::commands::codex_shim::RuntimeIdentity;

pub(super) fn session(thread: &codex::Thread, agent: &str, today: NaiveDate) -> Session {
    let name = [thread.name.as_deref(), thread.preview.lines().next()]
        .into_iter()
        .flatten()
        .map(clean)
        .find(|name| !name.is_empty())
        .unwrap_or_else(|| "untitled".to_string());
    let updated = Local.timestamp_opt(thread.updated_at, 0).single();
    let day = match updated.map(|at| at.date_naive()) {
        Some(date) if date == today => Day::Today,
        Some(date) if Some(date) == today.pred_opt() => Day::Yesterday,
        _ => Day::Older,
    };
    Session {
        id: thread.id.clone(),
        agent: agent.to_string(),
        tab: name.clone(),
        name,
        day,
        state: state(&thread.status),
    }
}

fn state(status: &codex::ThreadStatus) -> SessionState {
    match status {
        codex::ThreadStatus::Active { .. } => SessionState::Active,
        codex::ThreadStatus::Idle | codex::ThreadStatus::NotLoaded => SessionState::Idle,
        codex::ThreadStatus::SystemError => SessionState::Error,
    }
}

/// The bound agent first, marked default.
pub(super) fn agents(identity: &RuntimeIdentity) -> Vec<Agent> {
    let bound = &identity.bound_agent;
    let rest = identity
        .runnable_agents
        .iter()
        .flatten()
        .filter(|id| *id != bound);
    std::iter::once(bound)
        .chain(rest)
        .map(|id| {
            let name = id.rsplit_once(':').map_or(id.as_str(), |(_, name)| name);
            let text = if id == bound {
                format!(" {name:<12} default")
            } else {
                format!(" {name}")
            };
            Agent {
                id: id.clone(),
                name: name.to_string(),
                text,
                tone: Tone::Plain,
            }
        })
        .collect()
}

/// Every line carries an item id. A line under the item (its output, one changed path) has
/// its own, because a line with an id the tab has seen replaces that line.
pub(super) fn lines(item: &codex::ThreadItem) -> Vec<ChatLine> {
    let id = item.id();
    let line = |text: String, tone| shown(text, tone, id.to_string());
    match item {
        codex::ThreadItem::UserMessage { content, .. } => {
            let text: Vec<_> = content
                .iter()
                .filter_map(|input| match input {
                    codex::UserInput::Text { text, .. } => Some(clean(text)),
                    _ => None,
                })
                .collect();
            vec![line(text.join("\n"), Tone::You)]
        }
        codex::ThreadItem::AgentMessage { text, .. } => vec![line(clean(text), Tone::Reply)],
        codex::ThreadItem::Reasoning {
            summary, content, ..
        } => {
            let text: Vec<_> = summary
                .iter()
                .chain(content)
                .map(|part| clean(part))
                .collect();
            vec![line(text.join("\n"), Tone::Label)]
        }
        codex::ThreadItem::CommandExecution {
            command,
            status,
            aggregated_output,
            ..
        } => {
            let (mark, tone) = match status {
                codex::CommandExecutionStatus::InProgress => RUNNING,
                codex::CommandExecutionStatus::Completed => DONE,
                codex::CommandExecutionStatus::Failed | codex::CommandExecutionStatus::Declined => {
                    FAILED
                }
            };
            let mut lines = vec![line(format!("  {mark} run   {}", clean(command)), tone)];
            let output = aggregated_output.as_deref().map(clean).unwrap_or_default();
            let output = output.trim_end();
            if !output.is_empty() {
                lines.push(shown(
                    output.to_string(),
                    Tone::Label,
                    format!("{id}/output"),
                ));
            }
            lines
        }
        codex::ThreadItem::FileChange {
            changes, status, ..
        } => {
            let (mark, tone) = match status {
                codex::PatchApplyStatus::InProgress => RUNNING,
                codex::PatchApplyStatus::Completed => DONE,
                codex::PatchApplyStatus::Failed | codex::PatchApplyStatus::Declined => FAILED,
            };
            changes
                .iter()
                .enumerate()
                .map(|(n, change)| {
                    let text = format!("  {mark} edit  {}", clean(&change.path));
                    shown(text, tone, format!("{id}/{n}"))
                })
                .collect()
        }
        codex::ThreadItem::McpToolCall {
            server,
            tool,
            status,
            ..
        } => {
            let (mark, tone) = match status {
                codex::McpToolCallStatus::InProgress => RUNNING,
                codex::McpToolCallStatus::Completed => DONE,
                codex::McpToolCallStatus::Failed => FAILED,
            };
            vec![line(
                format!("  {mark} {} {}", clean(server), clean(tool)),
                tone,
            )]
        }
        codex::ThreadItem::CollabAgentToolCall { prompt, .. } => {
            let first = prompt.as_deref().and_then(|prompt| prompt.lines().next());
            let text = format!("  ◆ {}", clean(first.unwrap_or_default()));
            vec![line(text, Tone::Label)]
        }
        codex::ThreadItem::ContextCompaction { .. } => {
            vec![line(
                "  ✓ compacted the conversation".to_string(),
                Tone::Label,
            )]
        }
        _ => Vec::new(),
    }
}

const RUNNING: (char, Tone) = ('…', Tone::Warn);
const DONE: (char, Tone) = ('✓', Tone::Label);
/// Failed or declined.
const FAILED: (char, Tone) = ('✗', Tone::Warn);

fn shown(text: String, tone: Tone, item: String) -> ChatLine {
    ChatLine {
        text,
        tone,
        item: Some(item),
    }
}

pub(super) fn updates(note: codex::ServerNotification) -> Vec<Update> {
    use codex::ServerNotification as Note;
    // ponytail: each delta is cleaned on its own, so an escape split across two deltas shows
    // until the completed item replaces the line; carry escape state per item if that shows.
    let delta = |session, item, text: &str, tone| Update::Delta {
        session,
        item,
        text: clean(text),
        tone,
    };
    match note {
        Note::TurnStarted(started) => vec![Update::Turn {
            session: started.thread_id,
            turn: started.turn.id,
        }],
        Note::TurnCompleted(completed) => {
            let failed = completed
                .turn
                .error
                .map(|error| Update::Notice(clean(&error.message)));
            let done = Update::TurnEnded {
                session: completed.thread_id,
                turn: completed.turn.id,
            };
            std::iter::once(done).chain(failed).collect()
        }
        Note::ThreadStatusChanged(changed) => vec![Update::SessionState {
            session: changed.thread_id,
            state: state(&changed.status),
        }],
        Note::ItemStarted(started) => items(started.thread_id, &started.item),
        Note::ItemCompleted(completed) => items(completed.thread_id, &completed.item),
        Note::AgentMessageDelta(reply) => {
            vec![delta(
                reply.thread_id,
                reply.item_id,
                &reply.delta,
                Tone::Reply,
            )]
        }
        Note::ReasoningTextDelta(reasoning) => vec![delta(
            reasoning.thread_id,
            reasoning.item_id,
            &reasoning.delta,
            Tone::Label,
        )],
        Note::CommandExecutionOutputDelta(output) => vec![delta(
            output.thread_id,
            format!("{}/output", output.item_id),
            &output.delta,
            Tone::Label,
        )],
        _ => Vec::new(),
    }
}

fn items(session: String, item: &codex::ThreadItem) -> Vec<Update> {
    lines(item)
        .into_iter()
        .map(|line| Update::Item {
            session: session.clone(),
            line,
        })
        .collect()
}

/// The shell draws text as given, so a carriage return or a terminal escape would move its
/// cursor or restyle the screen. Drops both: CSI and OSC sequences whole, any other escape
/// with the character after it.
fn clean(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '\r' => {}
            '\u{1b}' => match chars.next() {
                Some('[') => {
                    let _ = chars.by_ref().find(|c| ('@'..='~').contains(c));
                }
                Some(']') => {
                    let mut escaped = false;
                    for c in chars.by_ref() {
                        if c == '\u{7}' || (escaped && c == '\\') {
                            break;
                        }
                        escaped = c == '\u{1b}';
                    }
                }
                _ => {}
            },
            c => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use chrono::{Local, NaiveDate, TimeZone};
    use gents_codex_protocol as codex;
    use gents_shell::sample::Tone;
    use gents_shell::source::{Agent, Day, SessionState, Update};
    use serde_json::{json, Value};

    use super::{agents, lines, session, updates};
    use crate::commands::codex::tests::identity;
    use crate::commands::shell::tests::line;

    fn today() -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 11, 5).unwrap()
    }

    fn noon(date: NaiveDate) -> i64 {
        let noon = date.and_hms_opt(12, 0, 0).unwrap();
        Local.from_local_datetime(&noon).unwrap().timestamp()
    }

    fn thread(name: Option<&str>, preview: &str, updated_at: i64) -> codex::Thread {
        let mut thread: codex::Thread =
            serde_json::from_value(crate::commands::shell::tests::thread("t1", updated_at))
                .unwrap();
        thread.name = name.map(str::to_string);
        thread.preview = preview.to_string();
        thread
    }

    #[test]
    fn a_thread_becomes_a_session_row() {
        let named = session(
            &thread(Some("fix login"), "ignored", noon(today())),
            "a",
            today(),
        );
        assert_eq!(named.id, "t1");
        assert_eq!(named.agent, "a");
        assert_eq!(
            (named.name.as_str(), named.tab.as_str()),
            ("fix login", "fix login")
        );
        assert_eq!(named.day, Day::Today);
        assert_eq!(named.state, SessionState::Idle);

        let yesterday = today().pred_opt().unwrap();
        let previewed = session(
            &thread(None, "first line\nsecond", noon(yesterday)),
            "a",
            today(),
        );
        assert_eq!(
            (previewed.name.as_str(), previewed.tab.as_str()),
            ("first line", "first line")
        );
        assert_eq!(previewed.day, Day::Yesterday);

        let long_ago = today() - chrono::Days::new(3);
        let untitled = session(&thread(Some(""), "", noon(long_ago)), "a", today());
        assert_eq!(
            (untitled.name.as_str(), untitled.tab.as_str()),
            ("untitled", "untitled")
        );
        assert_eq!(untitled.day, Day::Older);

        for (status, state) in [
            (
                codex::ThreadStatus::Active {
                    active_flags: Vec::new(),
                },
                SessionState::Active,
            ),
            (codex::ThreadStatus::Idle, SessionState::Idle),
            (codex::ThreadStatus::NotLoaded, SessionState::Idle),
            (codex::ThreadStatus::SystemError, SessionState::Error),
        ] {
            let mut thread = thread(None, "x", noon(today()));
            thread.status = status;
            assert_eq!(session(&thread, "a", today()).state, state);
        }
    }

    fn item(value: Value) -> codex::ThreadItem {
        serde_json::from_value(value).unwrap()
    }

    fn command(status: &str, output: Option<&str>) -> codex::ThreadItem {
        item(json!({
            "type": "commandExecution",
            "id": "c1",
            "command": "cargo test",
            "cwd": "/srv/root",
            "status": status,
            "commandActions": [],
            "aggregatedOutput": output,
        }))
    }

    #[test]
    fn each_shown_item_maps_to_its_rows() {
        use Tone::{Label, Reply, Warn, You};
        let cases = [
            (
                item(
                    json!({ "type": "userMessage", "id": "u1", "content": [{ "type": "text", "text": "hello" }] }),
                ),
                vec![line("hello", You, "u1")],
            ),
            (
                item(
                    json!({ "type": "agentMessage", "id": "r1", "text": "hi\r\n\u{1b}[1mthere\u{1b}[0m" }),
                ),
                vec![line("hi\nthere", Reply, "r1")],
            ),
            (
                item(json!({ "type": "reasoning", "id": "g1", "content": ["weighing it"] })),
                vec![line("weighing it", Label, "g1")],
            ),
            (
                command("inProgress", None),
                vec![line("  … run   cargo test", Warn, "c1")],
            ),
            (
                command("completed", Some("\u{1b}[32mok\u{1b}[0m\r\n")),
                vec![
                    line("  ✓ run   cargo test", Label, "c1"),
                    line("ok", Label, "c1/output"),
                ],
            ),
            (
                command("failed", Some("")),
                vec![line("  ✗ run   cargo test", Warn, "c1")],
            ),
            (
                item(json!({
                    "type": "collabAgentToolCall",
                    "id": "s1",
                    "tool": "spawnAgent",
                    "status": "inProgress",
                    "senderThreadId": "t1",
                    "receiverThreadIds": [],
                    "prompt": "review the diff\nthen report",
                    "agentsStates": {},
                })),
                vec![line("  ◆ review the diff", Label, "s1")],
            ),
            (
                item(json!({ "type": "contextCompaction", "id": "k1" })),
                vec![line("  ✓ compacted the conversation", Label, "k1")],
            ),
            (
                item(json!({ "type": "webSearch", "id": "w1", "query": "rust" })),
                Vec::new(),
            ),
        ];
        for (item, expected) in cases {
            assert_eq!(lines(&item), expected, "{item:?}");
        }

        for (status, mark, tone) in [
            ("inProgress", '…', Warn),
            ("completed", '✓', Label),
            ("failed", '✗', Warn),
            ("declined", '✗', Warn),
        ] {
            let edit = item(json!({
                "type": "fileChange",
                "id": "f1",
                "status": status,
                "changes": [
                    { "path": "src/a.rs", "kind": { "type": "update" }, "diff": "" },
                    { "path": "src/b.rs", "kind": { "type": "add" }, "diff": "" },
                ],
            }));
            let expected = vec![
                line(&format!("  {mark} edit  src/a.rs"), tone, "f1/0"),
                line(&format!("  {mark} edit  src/b.rs"), tone, "f1/1"),
            ];
            assert_eq!(lines(&edit), expected, "{status}");
        }

        for (status, mark, tone) in [
            ("inProgress", '…', Warn),
            ("completed", '✓', Label),
            ("failed", '✗', Warn),
        ] {
            let call = item(json!({
                "type": "mcpToolCall",
                "id": "m1",
                "server": "\u{1b}[1mgithub\u{1b}[0m",
                "tool": "search\r",
                "status": status,
                "arguments": {},
            }));
            let expected = vec![line(&format!("  {mark} github search"), tone, "m1")];
            assert_eq!(lines(&call), expected, "{status}");
        }
    }

    fn note(method: &str, params: Value) -> codex::ServerNotification {
        serde_json::from_value(json!({ "method": method, "params": params })).unwrap()
    }

    fn turn(status: &str, error: Value) -> Value {
        json!({ "id": "t1", "items": [], "status": status, "error": error })
    }

    #[test]
    fn notifications_map_to_updates() {
        let s1 = || "s1".to_string();
        let delta = |item: &str, text: &str, tone| Update::Delta {
            session: s1(),
            item: item.to_string(),
            text: text.to_string(),
            tone,
        };
        let item_params = |item: Value| json!({ "item": item, "threadId": "s1", "turnId": "t1", "startedAtMs": 0, "completedAtMs": 0 });
        let file_change = json!({
            "type": "fileChange",
            "id": "f1",
            "status": "completed",
            "changes": [
                { "path": "a.rs", "kind": { "type": "add" }, "diff": "" },
                { "path": "b.rs", "kind": { "type": "add" }, "diff": "" },
            ],
        });
        let cases = [
            (
                note(
                    "turn/started",
                    json!({ "threadId": "s1", "turn": turn("inProgress", Value::Null) }),
                ),
                vec![Update::Turn {
                    session: s1(),
                    turn: "t1".to_string(),
                }],
            ),
            (
                note(
                    "turn/completed",
                    json!({ "threadId": "s1", "turn": turn("completed", Value::Null) }),
                ),
                vec![Update::TurnEnded {
                    session: s1(),
                    turn: "t1".to_string(),
                }],
            ),
            (
                note(
                    "turn/completed",
                    json!({ "threadId": "s1", "turn": turn("failed", json!({ "message": "the model went away" })) }),
                ),
                vec![
                    Update::TurnEnded {
                        session: s1(),
                        turn: "t1".to_string(),
                    },
                    Update::Notice("the model went away".to_string()),
                ],
            ),
            (
                note(
                    "thread/status/changed",
                    json!({ "threadId": "s1", "status": { "type": "systemError" } }),
                ),
                vec![Update::SessionState {
                    session: s1(),
                    state: SessionState::Error,
                }],
            ),
            (
                note(
                    "item/started",
                    item_params(json!({ "type": "agentMessage", "id": "r1", "text": "" })),
                ),
                vec![Update::Item {
                    session: s1(),
                    line: line("", Tone::Reply, "r1"),
                }],
            ),
            (
                note("item/completed", item_params(file_change)),
                vec![
                    Update::Item {
                        session: s1(),
                        line: line("  ✓ edit  a.rs", Tone::Label, "f1/0"),
                    },
                    Update::Item {
                        session: s1(),
                        line: line("  ✓ edit  b.rs", Tone::Label, "f1/1"),
                    },
                ],
            ),
            (
                note(
                    "item/agentMessage/delta",
                    json!({ "threadId": "s1", "turnId": "t1", "itemId": "r1", "delta": "hi" }),
                ),
                vec![delta("r1", "hi", Tone::Reply)],
            ),
            (
                note(
                    "item/reasoning/textDelta",
                    json!({ "threadId": "s1", "turnId": "t1", "itemId": "g1", "delta": "hm", "contentIndex": 0 }),
                ),
                vec![delta("g1", "hm", Tone::Label)],
            ),
            (
                note(
                    "item/commandExecution/outputDelta",
                    json!({ "threadId": "s1", "turnId": "t1", "itemId": "c1", "delta": "\u{1b}[31mok\r\n" }),
                ),
                vec![delta("c1/output", "ok\n", Tone::Label)],
            ),
            (
                note("thread/archived", json!({ "threadId": "s1" })),
                Vec::new(),
            ),
        ];
        for (notification, expected) in cases {
            let method = notification.to_string();
            assert_eq!(updates(notification), expected, "{method}");
        }
    }

    #[test]
    fn runtime_agents_put_the_bound_one_first() {
        let mut runtime = identity(Some(&["did:key:z:reviewer", "did:key:z:default"]));
        runtime.bound_agent = "did:key:z:default".to_string();

        let agent = |id: &str, name: &str, text: &str| Agent {
            id: id.to_string(),
            name: name.to_string(),
            text: text.to_string(),
            tone: Tone::Plain,
        };
        assert_eq!(
            agents(&runtime),
            [
                agent("did:key:z:default", "default", " default      default"),
                agent("did:key:z:reviewer", "reviewer", " reviewer"),
            ]
        );
        let pinned: Vec<_> = agents(&identity(None))
            .into_iter()
            .map(|agent| agent.id)
            .collect();
        assert_eq!(pinned, ["a"]);
    }
}
