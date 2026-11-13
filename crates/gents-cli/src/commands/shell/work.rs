use std::path::{Path, PathBuf};

use chrono::{DateTime, Local};
use gents_documents::mailbox_write_decl::{MailboxAction, MailboxKind};
use gents_protocol::job::{
    JobChildState, JobPauseReason, JobPhaseState, JobReadError, JobState, JobView,
};
use gents_protocol::mailbox_question::{MailboxQuestion, MailboxQuestionAnswer};
use gents_protocol::operations::mailbox::MailboxReply;
use gents_session::session::mailbox::MailboxItem;
use gents_shell::source::{Answer, BackgroundCall, Job, JobEnd, Prompt, PromptKind};
use gents_store::config_client::{ConfigAccess, GraphqlEndpoint};
use gents_toolcalls::background::job::view::{list_jobs, JobFilter, StateFilter};

use crate::commands::background::{load_background_tool_calls, BackgroundToolCallRow};

/// This home's GraphQL, read as the home's node, which signs its mailbox replies.
#[derive(Clone)]
pub(super) struct Work {
    pub(super) endpoint: GraphqlEndpoint,
    pub(super) home: PathBuf,
}

impl Work {
    pub(super) fn access(&self) -> ConfigAccess {
        ConfigAccess::Graphql(self.endpoint.clone())
    }
}

/// Signs as this CLI's User, as `gents automation mailbox` does. Never
/// `resolve_config_access`: with no runtime answering, it opens and claims the embedded store.
pub(super) fn work_for(home: &Path) -> Result<Work, String> {
    let endpoint = gents_service::home_state::resolve_graphql_endpoint(None, Some(home))
        .await
        .map_err(|error| format!("{error:#}"))?;
    Ok(Work {
        endpoint,
        home: home.to_path_buf(),
    })
}

/// Every job of each of `sessions`, ended ones too, a session's in the order the node lists them.
pub(super) async fn jobs(
    access: &ConfigAccess,
    sessions: &[String],
) -> Result<Vec<JobView>, JobReadError> {
    let mut jobs = Vec::new();
    for session in sessions {
        // ponytail: every finished run of each open session; add a limit if sessions keep hundreds
        let filter = JobFilter {
            session_id: Some(session.clone()),
            state: StateFilter::States(vec![
                JobState::Running,
                JobState::Paused,
                JobState::AwaitingUser,
                JobState::Completed,
                JobState::Failed,
                JobState::Cancelled,
                JobState::TimedOut,
            ]),
            plugin: None,
        };
        jobs.extend(list_jobs(access, &filter).await?);
    }
    Ok(jobs)
}

/// The pending and running background calls, as the desktop's Background view lists them.
pub(super) async fn background(
    access: &ConfigAccess,
) -> anyhow::Result<Vec<BackgroundToolCallRow>> {
    load_background_tool_calls(access, None, None, &["pending", "running"]).await
}

pub(super) fn job(view: &JobView) -> Job {
    let name = match view.pin.plugin_ref.rsplit_once('/') {
        Some((_, name)) => name,
        None => &view.pin.script_id,
    };
    let progress = view.progress.as_ref();
    // A dynamic run's progress title is its script id; its handle reads better.
    let title = progress
        .map(|progress| progress.title.as_str())
        .filter(|title| !title.is_empty() && *title != view.pin.script_id)
        .unwrap_or(&view.handle);
    let ended = match view.state {
        JobState::Completed => Some(("finished", JobEnd::Done)),
        JobState::Failed => Some(("failed", JobEnd::Failed)),
        JobState::Cancelled => Some(("cancelled", JobEnd::Failed)),
        JobState::TimedOut => Some(("timed out", JobEnd::Failed)),
        JobState::Running | JobState::Paused | JobState::AwaitingUser => None,
    };
    let waiting = ended.is_none()
        && (view.state == JobState::AwaitingUser
            || progress
                .is_some_and(|progress| progress.pause_reason == Some(JobPauseReason::User)));
    let phases = progress.map_or(&[][..], |progress| &progress.phases);
    let status = if let Some((status, _)) = ended {
        status.to_string()
    } else if waiting {
        "waiting on you".to_string()
    } else if view.state == JobState::Paused {
        "paused".to_string()
    } else if phases.is_empty() && view.children.is_empty() {
        "running".to_string()
    } else if phases.is_empty() {
        let done = view
            .children
            .iter()
            .filter(|child| child.state == JobChildState::Completed)
            .count();
        format!("{done} of {} agents done", view.children.len())
    } else {
        let done = phases
            .iter()
            .filter(|phase| phase.state == JobPhaseState::Done)
            .count();
        let step = phases
            .iter()
            .position(|phase| phase.state == JobPhaseState::Active)
            .unwrap_or(done)
            .min(phases.len() - 1);
        format!(
            "{} · step {} of {}",
            phases[step].title,
            step + 1,
            phases.len()
        )
    };
    let marks = phases.iter().map(|phase| {
        let mark = match phase.state {
            JobPhaseState::Done => '✓',
            JobPhaseState::Active => '●',
            JobPhaseState::Pending => '○',
            JobPhaseState::Failed => '✗',
        };
        format!("{mark} {}", phase.title)
    });
    let children = progress
        .into_iter()
        .flat_map(|progress| &progress.active)
        .map(|child| format!("◆ {}", child.title));
    // A run that declares no phases has its agents as its steps, in slot order.
    let agents = view
        .children
        .iter()
        .filter(|_| phases.is_empty())
        .map(|child| {
            let mark = match child.state {
                JobChildState::Completed => '✓',
                JobChildState::Failed | JobChildState::Interrupted => '✗',
                JobChildState::Live => '●',
                JobChildState::Other => '○',
            };
            format!("{mark} {}", child.title)
        });
    Job {
        address: view.address.clone(),
        session: view.session_id.clone(),
        name: name.to_string(),
        title: title.to_string(),
        status,
        waiting,
        ended: ended.map(|(_, end)| end),
        detail: marks.chain(children).chain(agents).collect(),
    }
}

/// Only pending and running calls are read, so none is done. The start time, unlike an
/// elapsed time, stays true between reads.
pub(super) fn background_call(row: &BackgroundToolCallRow) -> BackgroundCall {
    let started = row
        .started_at
        .as_deref()
        .and_then(|stamp| DateTime::parse_from_rfc3339(stamp).ok());
    let status = match started {
        Some(started) => format!("running since {}", clock(started.with_timezone(&Local))),
        None => "pending".to_string(),
    };
    BackgroundCall {
        session: row.session_id.clone().unwrap_or_default(),
        name: row.tool_name.clone().unwrap_or_default(),
        status,
        done: false,
    }
}

pub(super) fn clock(time: DateTime<Local>) -> String {
    time.format("%H:%M").to_string()
}

/// The question an ask that starts a request carries, read as the desktop reads one.
fn question(item: &MailboxItem) -> Option<MailboxQuestion> {
    let asks = item.kind == MailboxKind::Ask.as_str()
        && item.parsed_action() == Some(MailboxAction::StartRequest);
    let payload = item.payload.as_deref().filter(|_| asks)?;
    MailboxQuestion::from_payload(payload).ok()
}

/// Only an optional version 2 or later question takes an explicit skip.
fn skippable(question: &MailboxQuestion) -> bool {
    question.version >= 2 && question.optional == Some(true)
}

/// An ask or a gate as the prompt that answers it.
pub(super) fn prompt(item: &MailboxItem) -> Option<Prompt> {
    let kind = if item.kind == MailboxKind::Ask.as_str() {
        PromptKind::Ask
    } else if item.kind == MailboxKind::Gate.as_str() {
        PromptKind::Gate
    } else {
        return None;
    };
    let asked = question(item);
    let summary = match (&asked, &item.summary) {
        (Some(question), Some(summary)) => Some(format!("{}\n{summary}", question.prompt)),
        (Some(question), None) => Some(question.prompt.clone()),
        (None, summary) => summary.clone(),
    };
    let answer = match item.parsed_action()? {
        MailboxAction::Ack => Answer::Ack,
        MailboxAction::WriteDocument => Answer::Document {
            collection: item.expected_collection.clone().unwrap_or_default(),
            key: item.item_key.clone(),
        },
        MailboxAction::StartRequest => match asked {
            Some(question) => Answer::Choose {
                options: question
                    .options
                    .iter()
                    .map(|option| option.label.clone())
                    .collect(),
                free_text: question.allow_free_text,
                skip: skippable(&question),
            },
            None => Answer::Reply,
        },
    };
    Some(Prompt {
        item: item.doc_id.clone(),
        kind,
        title: item.title.clone(),
        summary,
        answer,
        dismissable: !gents_session::operations::mailbox::needs_answer(
            &item.kind,
            &item.action,
            item.payload.as_deref(),
        ),
    })
}

/// `session`'s prompts asked of `principal`, oldest first, from items listed newest first.
pub(super) fn prompts(items: &[MailboxItem], principal: &str, session: &str) -> Vec<Prompt> {
    let asked = |item: &&MailboxItem| {
        item.session_id.as_deref() == Some(session) && item.target_node_did == principal
    };
    items
        .iter()
        .rev()
        .filter(asked)
        .filter_map(prompt)
        .collect()
}

/// What the user typed, as the item takes it: a question by option numbers, by `skip` where it
/// may be skipped, or in the user's own words where it allows them.
pub(super) fn reply_from_text(text: &str, item: &MailboxItem) -> Result<MailboxReply, String> {
    let Some(question) = question(item) else {
        return Ok(MailboxReply::Text(text.to_string()));
    };
    if skippable(&question) && text.trim().eq_ignore_ascii_case("skip") {
        return Ok(MailboxReply::Answer(MailboxQuestionAnswer {
            skip: Some(true),
            ..Default::default()
        }));
    }
    let count = question.options.len();
    let picked: Option<Vec<String>> = text
        .split(|c: char| c == ',' || c.is_whitespace())
        .filter(|token| !token.is_empty())
        .map(|token| {
            let at = token
                .parse::<usize>()
                .ok()
                .filter(|at| (1..=count).contains(at))?;
            Some(question.options[at - 1].id.clone())
        })
        .collect();
    let answer = match picked {
        Some(option_ids) if !option_ids.is_empty() => MailboxQuestionAnswer {
            option_ids,
            ..Default::default()
        },
        _ if question.allow_free_text => MailboxQuestionAnswer {
            free_text: Some(text.to_string()),
            ..Default::default()
        },
        _ => return Err(format!("type 1-{count}")),
    };
    Ok(MailboxReply::Answer(answer))
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;
    use gents_protocol::mailbox_question::MailboxQuestionAnswer;
    use gents_shell::source::{Answer, PromptKind};
    use serde_json::{json, Value};

    use super::*;

    /// A job row as `list_jobs` returns it.
    fn view(state: &str, progress: Value) -> JobView {
        serde_json::from_value(json!({
            "address": "s1:call-1",
            "row_doc_id": "bae-1",
            "session_id": "s1",
            "tool_call_id": "call-1",
            "scope": { "session_id": "s1", "request_id": "r1", "node_did": "did:key:z6Mk" },
            "handle": "change-unit-7",
            "state": state,
            "pin": {
                "script_id": "change_unit",
                "origin": "installed",
                "engine_digest": "e1",
                "plugin_ref": "gents/change-unit",
                "agent_budget": 4,
                "cwd": "/srv",
                "chain_root_doc_id": "bae-1",
            },
            "progress": progress,
            "children": [],
            "created_at": 0,
            "adopted_at": 0,
        }))
        .expect("a job view")
    }

    fn progress(
        state: &str,
        pause_reason: Value,
        phases: &[(&str, &str)],
        active: &[&str],
    ) -> Value {
        let phases: Vec<_> = phases
            .iter()
            .map(|(title, state)| json!({ "title": title, "state": state }))
            .collect();
        let active: Vec<_> = active
            .iter()
            .map(|title| json!({ "slot": title, "title": title, "session_id": "c1" }))
            .collect();
        json!({
            "title": "cli ux",
            "state": state,
            "pause_reason": pause_reason,
            "phases": phases,
            "active": active,
            "spawned": 1,
            "budget": { "total": 4, "spent": 1, "remaining": 3 },
            "last_event": "",
            "observed_terminal_children": 0,
        })
    }

    const PHASES: &[(&str, &str)] = &[
        ("plan", "done"),
        ("edit", "done"),
        ("build", "active"),
        ("test", "pending"),
        ("review", "pending"),
    ];

    #[test]
    fn a_running_job_shows_its_active_phase_as_a_step() {
        let shown = job(&view(
            "running",
            progress("running", Value::Null, PHASES, &[]),
        ));

        assert_eq!(shown.address, "s1:call-1");
        assert_eq!(shown.session, "s1");
        assert_eq!(shown.name, "change-unit");
        assert_eq!(shown.title, "cli ux");
        assert_eq!(shown.status, "build · step 3 of 5");
        assert!(!shown.waiting);

        let between = [("plan", "done"), ("edit", "done"), ("test", "pending")];
        let next = job(&view(
            "running",
            progress("running", Value::Null, &between, &[]),
        ));
        assert_eq!(next.status, "test · step 3 of 3");
        let finished = [("plan", "done"), ("edit", "done")];
        let last = job(&view(
            "running",
            progress("running", Value::Null, &finished, &[]),
        ));
        assert_eq!(
            last.status, "edit · step 2 of 2",
            "the step is capped at the last"
        );
    }

    #[test]
    fn a_job_waiting_on_the_user_says_so() {
        let awaiting = job(&view(
            "awaiting_user",
            progress("awaiting_user", Value::Null, PHASES, &[]),
        ));
        let paused_for_you = job(&view(
            "paused",
            progress("paused", json!("user"), PHASES, &[]),
        ));
        let paused = job(&view(
            "paused",
            progress("paused", json!("back_off"), PHASES, &[]),
        ));

        for waiting in [&awaiting, &paused_for_you] {
            assert_eq!(waiting.status, "waiting on you");
            assert!(waiting.waiting);
        }
        assert_eq!(paused.status, "paused");
        assert!(!paused.waiting);
    }

    #[test]
    fn a_job_with_no_phases_shows_its_state() {
        let mut bare = view("running", Value::Null);
        bare.pin.plugin_ref = String::new();

        let shown = job(&bare);

        assert_eq!(shown.status, "running");
        assert_eq!(
            shown.name, "change_unit",
            "the script names a job with no plugin"
        );
        assert_eq!(
            shown.title, "change-unit-7",
            "the handle titles a job with no progress"
        );
        assert!(shown.detail.is_empty());
    }

    #[test]
    fn a_jobs_detail_lists_phases_then_active_children() {
        let phases = [
            ("plan", "done"),
            ("build", "active"),
            ("test", "pending"),
            ("lint", "failed"),
        ];
        let shown = job(&view(
            "running",
            progress("running", Value::Null, &phases, &["scout", "fixer"]),
        ));

        assert_eq!(
            shown.detail,
            [
                "✓ plan",
                "● build",
                "○ test",
                "✗ lint",
                "◆ scout",
                "◆ fixer"
            ]
        );
    }

    #[test]
    fn a_background_call_shows_when_it_started() {
        let row = |started_at: Option<String>| BackgroundToolCallRow {
            request_id: Some("r1".to_string()),
            tool_call_id: Some("call-1".to_string()),
            tool_name: Some("bash".to_string()),
            status: None,
            lifecycle_state: Some("running".to_string()),
            await_mode: Some("background".to_string()),
            started_at,
            session_id: None,
        };
        let started = Local.with_ymd_and_hms(2026, 11, 5, 9, 7, 30).unwrap();

        let running = background_call(&row(Some(started.to_utc().to_rfc3339())));
        let pending = background_call(&row(None));

        assert_eq!(running.status, "running since 09:07");
        assert_eq!(
            (pending.name.as_str(), pending.status.as_str()),
            ("bash", "pending")
        );
        assert!(!running.done && !pending.done);
        assert_eq!(clock(started), "09:07");
    }

    /// A stale pause for the user leaves an ended run ended, and its steps stay listed.
    #[test]
    fn a_run_without_phases_lists_its_agents_as_steps() {
        let child = |slot: &str, title: &str, state: &str| {
            serde_json::from_value(json!({
                "slot": slot, "attempt": 0, "session_id": "c", "request_id": "r",
                "state": state, "title": title,
                "usage": { "input": 0, "output": 0, "total": 0 },
                "duration_ms": 0, "target": "wf-read-only",
            }))
            .expect("a child row")
        };
        let mut progress = progress("running", Value::Null, &[], &[]);
        progress["title"] = json!("change_unit");
        let mut run = view("running", progress);
        run.children = vec![
            child("econ", "Zcash: economic lens", "completed"),
            child("social", "Zcash: social lens", "live"),
            child("crypto", "Zcash: cryptographic lens", "failed"),
        ];

        let shown = job(&run);

        assert_eq!(
            shown.title, "change-unit-7",
            "the handle, not the script id"
        );
        assert_eq!(shown.status, "1 of 3 agents done");
        let steps = [
            "✓ Zcash: economic lens",
            "● Zcash: social lens",
            "✗ Zcash: cryptographic lens",
        ];
        assert_eq!(shown.detail, steps);

        run.children.clear();
        assert_eq!(job(&run).status, "running");
        assert!(job(&run).detail.is_empty());
    }

    #[test]
    fn an_ended_job_says_how_it_ended() {
        let ends = [
            ("completed", "finished", JobEnd::Done),
            ("failed", "failed", JobEnd::Failed),
            ("cancelled", "cancelled", JobEnd::Failed),
            ("timed_out", "timed out", JobEnd::Failed),
        ];
        for (state, status, end) in ends {
            let shown = job(&view(
                state,
                progress(state, json!("user"), PHASES, &["scout"]),
            ));

            assert_eq!(shown.status, status, "{state}");
            assert_eq!(shown.ended, Some(end), "{state}");
            assert!(!shown.waiting, "{state}");
            let steps = [
                "✓ plan",
                "✓ edit",
                "● build",
                "○ test",
                "○ review",
                "◆ scout",
            ];
            assert_eq!(shown.detail, steps, "{state}");
        }
        let running = job(&view(
            "running",
            progress("running", Value::Null, PHASES, &[]),
        ));
        assert_eq!(running.ended, None);
    }

    #[test]
    fn a_background_call_keeps_its_session() {
        let row = |session: Value| -> BackgroundToolCallRow {
            let mut row = json!({
                "request_id": "r1",
                "tool_call_id": "call-1",
                "tool_name": "bash",
                "lifecycle_state": "running",
                "await_mode": "background",
            });
            if !session.is_null() {
                row["session_id"] = session;
            }
            serde_json::from_value(row).expect("a background row")
        };

        assert_eq!(background_call(&row(json!("s1"))).session, "s1");
        assert_eq!(
            background_call(&row(Value::Null)).session,
            "",
            "a row from before the field"
        );
    }

    /// An open item of `kind` and `action` on session `s1`, asked of this node.
    fn item(id: &str, kind: &str, action: &str, payload: Option<Value>) -> MailboxItem {
        serde_json::from_value(json!({
            "_docID": id,
            "item_key": format!("key:{id}"),
            "requester_did": "did:key:z",
            "node_did": "did:key:z",
            "status": "open",
            "kind": kind,
            "action": action,
            "title": format!("{id} title"),
            "summary": null,
            "payload": payload.map(|payload| payload.to_string()),
            "source_kind": "runtime",
            "source_id": "request-doc",
            "session_id": "s1",
            "request_id": "r1",
            "target_node_did": "did:key:z",
            "target_agent_did": "a",
            "expected_collection": "Review",
            "created_at": "2026-11-05T12:00:00Z",
        }))
        .expect("a mailbox item")
    }

    fn question(version: u32, optional: bool, free_text: bool) -> Value {
        json!({
            "version": version,
            "prompt": "Which channel ships?",
            "options": [{ "id": "stable", "label": "Stable" }, { "id": "beta", "label": "Beta" }],
            "allow_free_text": free_text,
            "optional": optional,
        })
    }

    fn choose(free_text: bool, skip: bool) -> Answer {
        Answer::Choose {
            options: vec!["Stable".to_string(), "Beta".to_string()],
            free_text,
            skip,
        }
    }

    fn answer(option_ids: &[&str], free_text: Option<&str>, skip: Option<bool>) -> MailboxReply {
        MailboxReply::Answer(MailboxQuestionAnswer {
            option_ids: option_ids.iter().map(|id| id.to_string()).collect(),
            free_text: free_text.map(str::to_string),
            skip,
            question_hash: None,
        })
    }

    #[test]
    fn mailbox_items_become_prompts_by_action() {
        let approval = Prompt {
            item: "i1".to_string(),
            kind: PromptKind::Gate,
            title: "i1 title".to_string(),
            summary: None,
            answer: Answer::Ack,
            dismissable: true,
        };
        assert_eq!(prompt(&item("i1", "gate", "ack", None)), Some(approval));
        let shown = |item: &MailboxItem| prompt(item).map(|prompt| (prompt.kind, prompt.answer));
        let document = Answer::Document {
            collection: "Review".to_string(),
            key: "key:i2".to_string(),
        };
        assert_eq!(
            shown(&item("i2", "ask", "write_document", None)),
            Some((PromptKind::Ask, document))
        );
        assert_eq!(
            shown(&item("i3", "ask", "start_request", None)),
            Some((PromptKind::Ask, Answer::Reply))
        );
        let optional = item("i4", "ask", "start_request", Some(question(3, true, true)));
        assert_eq!(
            shown(&optional),
            Some((PromptKind::Ask, choose(true, true)))
        );
        let summary = |item: &MailboxItem| prompt(item).and_then(|prompt| prompt.summary);
        assert_eq!(
            summary(&optional).as_deref(),
            Some("Which channel ships?"),
            "the question is the summary when the item has none"
        );
        let summarized = MailboxItem {
            summary: Some("which one ships".to_string()),
            ..optional.clone()
        };
        assert_eq!(
            summary(&summarized).as_deref(),
            Some("Which channel ships?\nwhich one ships"),
            "the question comes before the item's summary"
        );
        let old = item(
            "i5",
            "ask",
            "start_request",
            Some(question(1, false, false)),
        );
        assert_eq!(shown(&old), Some((PromptKind::Ask, choose(false, false))));
        let gated = item(
            "i6",
            "gate",
            "start_request",
            Some(question(3, false, false)),
        );
        assert_eq!(
            shown(&gated),
            Some((PromptKind::Gate, Answer::Reply)),
            "only an ask holds a question"
        );
        for kind in ["finished", "failed", "flag"] {
            assert_eq!(prompt(&item("i7", kind, "ack", None)), None, "{kind}");
        }
    }

    #[test]
    fn a_modern_question_is_not_dismissable_and_an_old_one_is() {
        for (version, dismissable) in [(1, true), (2, false), (3, false)] {
            let asked = item(
                "q1",
                "ask",
                "start_request",
                Some(question(version, false, false)),
            );
            let shown = prompt(&asked).expect("a question");
            assert_eq!(shown.dismissable, dismissable, "version {version}");
        }
    }

    #[test]
    fn text_answers_a_question_by_number_words_or_skip() {
        let asked = |optional, free_text| {
            item(
                "q1",
                "ask",
                "start_request",
                Some(question(3, optional, free_text)),
            )
        };
        let single = asked(false, false);
        assert_eq!(
            reply_from_text("2", &single),
            Ok(answer(&["beta"], None, None))
        );
        assert_eq!(
            reply_from_text("SKIP", &asked(true, false)),
            Ok(answer(&[], None, Some(true)))
        );
        let words = asked(false, true);
        assert_eq!(
            reply_from_text("skip", &words),
            Ok(answer(&[], Some("skip"), None)),
            "skip is words where it is not allowed"
        );
        assert_eq!(
            reply_from_text("ship beta", &words),
            Ok(answer(&[], Some("ship beta"), None))
        );
        assert_eq!(
            reply_from_text("3", &words),
            Ok(answer(&[], Some("3"), None))
        );
        for text in ["3", "ship beta", "skip"] {
            assert_eq!(
                reply_from_text(text, &single),
                Err("type 1-2".to_string()),
                "{text}"
            );
        }

        let two = reply_from_text(" 1, 2 ", &single).expect("two numbers");
        assert_eq!(two, answer(&["stable", "beta"], None, None));
        let MailboxReply::Answer(two) = two else {
            unreachable!()
        };
        let two = gents_session::operations::mailbox::answer_observing((&single).into(), two)
            .expect("the question as read");
        let refused = gents_session::operations::mailbox::prepare_reply(
            (&single).into(),
            MailboxReply::Answer(two),
            &Default::default(),
        )
        .expect_err("a single choice");
        assert_eq!(
            refused.to_string(),
            "answer selects more than one option of a single-select question"
        );

        let plain = item("r1", "ask", "start_request", None);
        assert_eq!(
            reply_from_text("looks good", &plain),
            Ok(MailboxReply::Text("looks good".to_string()))
        );
    }
}
