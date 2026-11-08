use std::collections::BTreeMap;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Local};
use gents::pack::BindAccess;
use gents::plugin::allowed::AllowedDir;
use gents::plugin::ux::{UxDoor, UxListing, UxProducer};
use gents::Collection;
use gents_desktop_core::inference::AgentView;
use gents_shell::sample::{empty, MenuId, Tone};
use gents_shell::source::{Doc, FormRef, MenuAct, MenuRow};
use serde_json::Value;

use super::menus::row;

/// The node's documents its triggers refer to.
pub(super) struct Sources {
    pub(super) schedules: Vec<Value>,
    pub(super) event_sources: Vec<Value>,
    pub(super) tasks: Vec<Value>,
    pub(super) agents: Vec<AgentView>,
}

const DAYS: [&str; 7] = [
    "Sunday",
    "Monday",
    "Tuesday",
    "Wednesday",
    "Thursday",
    "Friday",
    "Saturday",
];

/// `{prefix}-{milliseconds since the epoch in base 36}`, the desktop's `newId` (`draft.ts:333`).
pub(super) fn new_id(prefix: &str) -> String {
    let since = SystemTime::now().duration_since(UNIX_EPOCH);
    let mut millis = since.unwrap_or_default().as_millis();
    let mut digits = Vec::new();
    loop {
        digits.extend(char::from_digit((millis % 36) as u32, 36));
        millis /= 36;
        if millis == 0 {
            break;
        }
    }
    format!("{prefix}-{}", digits.iter().rev().collect::<String>())
}

/// The prefix of a new `doc`'s id, as the desktop's panels pass it to `newId`.
pub(super) fn prefix(doc: Doc) -> &'static str {
    match doc {
        Doc::Task => "task",
        Doc::Skill => "skill",
        Doc::Trigger => "trig",
        Doc::Schedule => "sched",
        Doc::EventSource => "evsrc",
        Doc::RemoteTool => "mcp",
    }
}

fn collection(doc: Doc) -> Collection {
    match doc {
        Doc::Task => Collection::Task,
        Doc::Skill => Collection::Skill,
        Doc::Trigger => Collection::Trigger,
        Doc::Schedule => Collection::Schedule,
        Doc::EventSource => Collection::EventSource,
        Doc::RemoteTool => Collection::ToolServiceRegistry,
    }
}

/// The desktop's row title: the display name, else a skill's name, else the id. A skill's names
/// are trimmed, blank as none, as the bridge's view (`snapshot/runtime.rs:170-175`).
pub(super) fn title(doc: Doc, document: &Value) -> String {
    let named = match doc {
        Doc::Skill => named(&document["display_name"]).or(named(&document["name"])),
        _ => document["display_name"].as_str(),
    };
    named.map_or_else(|| id(doc, document), str::to_string)
}

/// `value` trimmed, blank as none: the bridge's `normalize_optional` (`types/util.rs:3-8`).
pub(super) fn named(value: &Value) -> Option<&str> {
    value
        .as_str()
        .map(str::trim)
        .filter(|name| !name.is_empty())
}

fn id(doc: Doc, document: &Value) -> String {
    let id = &document[collection(doc).unique_field()];
    id.as_str().unwrap_or_default().to_string()
}

/// `SkillsPanel.tsx:289-331`.
pub(super) fn skills(documents: &[Value]) -> Vec<MenuRow> {
    listed(Doc::Skill, "New skill", documents, |skill| {
        let count = skill["tool_refs"].as_array().map_or(0, Vec::len);
        let tools = if count == 1 { "tool" } else { "tools" };
        let enabled = skill["enabled"] != false;
        let (mark, disabled, tone) = match enabled {
            true => ("●", "", Tone::Plain),
            false => ("○", " · disabled", Tone::Label),
        };
        let title = title(Doc::Skill, skill);
        let text = format!(" {mark} {title}   {count} {tools}{disabled}");
        document_row(Doc::Skill, skill, text, tone, Some(enabled))
    })
}

/// `TriggersPanel.tsx:471-529`. An off trigger shows no badge or note.
pub(super) fn triggers(documents: &[Value], sources: &Sources) -> Vec<MenuRow> {
    listed(Doc::Trigger, "New trigger", documents, |trigger| {
        let enabled = trigger["enabled"] != false;
        let (mark, tone, tail) = match trigger_readiness(trigger, sources) {
            _ if !enabled => ("○", Tone::Label, None),
            Err(reason) => ("●", Tone::Warn, Some(reason)),
            Ok(note) if last_run_failed(trigger) => ("●", Tone::Warn, note),
            Ok(note) => ("●", Tone::Plain, note),
        };
        let title = title(Doc::Trigger, trigger);
        let words = trigger_in_words(trigger, sources);
        let tail = tail.map_or(String::new(), |tail| format!("   {tail}"));
        let text = format!(" {mark} {title}   {words}{tail}");
        document_row(Doc::Trigger, trigger, text, tone, Some(enabled))
    })
}

/// `SchedulesPanel.tsx:34-38,242-274`.
pub(super) fn schedules(documents: &[Value]) -> Vec<MenuRow> {
    listed(Doc::Schedule, "New schedule", documents, |schedule| {
        let cadence = &schedule["cadence"];
        let meta = match cadence["kind"].as_str() {
            Some("cron") => cadence["expression"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
            _ => format!("every {}s", cadence["interval_secs"]),
        };
        plain(Doc::Schedule, schedule, &meta)
    })
}

/// `EventSourcesPanel.tsx:322-354`.
pub(super) fn event_sources(documents: &[Value]) -> Vec<MenuRow> {
    listed(Doc::EventSource, "New event source", documents, |source| {
        let collection = source["source_collection"].as_str().unwrap_or_default();
        plain(Doc::EventSource, source, collection)
    })
}

/// `ToolServicesPanel.tsx:229-247`.
pub(super) fn remote_tools(documents: &[Value]) -> Vec<MenuRow> {
    listed(Doc::RemoteTool, "New remote tools", documents, |service| {
        let hostname = service["hostname"].as_str().unwrap_or_default();
        plain(Doc::RemoteTool, service, hostname)
    })
}

/// `" + {create}"`, the node's own rows, then each pack's under its header, as
/// `ListDetail.tsx:427-455` lists them, or the menu's empty copy (`:487`). A document whose pack
/// tags do not parse is the node's own.
fn listed<F>(doc: Doc, create: &str, documents: &[Value], shown: F) -> Vec<MenuRow>
where
    F: Fn(&Value) -> MenuRow,
{
    let new = MenuRow {
        enter: Some(MenuAct::Open(FormRef::New(doc))),
        ..row(format!(" + {create}"), Tone::Accent)
    };
    let mut rows = vec![new];
    if documents.is_empty() {
        rows.extend(empty_rows(doc.menu()));
    }
    let mut packs: BTreeMap<String, Vec<MenuRow>> = BTreeMap::new();
    for document in documents {
        let tags: Vec<String> =
            serde_json::from_value(document["tags"].clone()).unwrap_or_default();
        let group = match gents::pack::pack_origin_from_tags(&tags) {
            Ok(Some(pack)) => packs.entry(pack.to_string()).or_default(),
            Ok(None) | Err(_) => &mut rows,
        };
        group.push(shown(document));
    }
    for (pack, entries) in packs {
        let header = format!(" {pack} · {} from this pack", entries.len());
        rows.push(row(header, Tone::Label));
        rows.extend(entries);
    }
    rows
}

/// The desktop's copy for `menu` with nothing in it.
fn empty_rows(menu: MenuId) -> impl Iterator<Item = MenuRow> {
    empty(menu)
        .iter()
        .map(|line| row(line.to_string(), Tone::Label))
}

/// `AllowedFoldersPanel.tsx:53-119`: " + Add a folder", the empty copy, then each folder, under
/// `home_dir` shortened to `~` as `shortPath` shortens it (`lib/paths.ts`). `d` switches its
/// access and `.` removes it.
pub(super) fn folders(dirs: &[AllowedDir], home_dir: Option<&Path>) -> Vec<MenuRow> {
    let add = MenuRow {
        enter: Some(MenuAct::Open(FormRef::AddFolder)),
        ..row(" + Add a folder".to_string(), Tone::Accent)
    };
    let mut rows = vec![add];
    if dirs.is_empty() {
        rows.extend(empty_rows(MenuId::AllowedFolders));
    }
    rows.extend(dirs.iter().map(|dir| {
        let path = dir.path.display().to_string();
        let (access, other) = match dir.access {
            BindAccess::Read => ("Read only", BindAccess::ReadWrite),
            BindAccess::ReadWrite => ("Read and write", BindAccess::Read),
        };
        let shown = match home_dir.and_then(|home| dir.path.strip_prefix(home).ok()) {
            Some(rest) if rest.as_os_str().is_empty() => "~".to_string(),
            Some(rest) => format!("~/{}", rest.display()),
            None => path.clone(),
        };
        let switch = MenuAct::SetFolderAccess {
            path: path.clone(),
            access: other.as_str().to_string(),
        };
        MenuRow {
            d: Some(switch),
            more: vec![("Remove".to_string(), MenuAct::RemoveFolder(path))],
            ..row(format!(" {shown}   {access}"), Tone::Plain)
        }
    }));
    rows
}

/// `UxPluginsPanel.tsx:25-31,72-86`, sorted by id (`:36`), a stable sort keeping listing order
/// for one id. The shell loads no plugin, so each is named by the id its listing declares, as the
/// desktop names a plugin it has not loaded (`runtime-door.ts:61-69,132-147`).
pub(super) fn ux_plugins(listings: &[UxListing]) -> Vec<MenuRow> {
    let mut listed: Vec<_> = listings
        .iter()
        .map(|listing| {
            let (id, door) = match listing.door {
                UxDoor::Dev => (listing.id.as_str(), "Dev folder"),
                UxDoor::Pack => (listing.id.rsplit('/').next().unwrap_or_default(), "Pack"),
            };
            let mut about = vec![door];
            about.extend(listing.pack.as_deref());
            if listing.producer == UxProducer::Afb {
                about.push("produced by .afb");
            }
            about.extend(listing.description.as_deref());
            let text = format!(" {id}   {id} · {}", about.join(" · "));
            (id, row(text, Tone::Plain))
        })
        .collect();
    listed.sort_by_key(|&(id, _)| id);
    listed.into_iter().map(|(_, row)| row).collect()
}

fn plain(doc: Doc, document: &Value, meta: &str) -> MenuRow {
    let text = format!(" {}   {meta}", title(doc, document));
    document_row(doc, document, text, Tone::Plain, None)
}

/// A row that opens `document`, with the desktop's row menu (`RowMenu.tsx:93-116`); `enabled`
/// gives it the on/off switch. The desktop duplicates no tool service.
fn document_row(
    doc: Doc,
    document: &Value,
    text: String,
    tone: Tone,
    enabled: Option<bool>,
) -> MenuRow {
    let (id, title) = (id(doc, document), title(doc, document));
    let open = MenuAct::Open(FormRef::Edit(doc, id.clone()));
    let mut more = vec![("Open".to_string(), open.clone())];
    if doc != Doc::RemoteTool {
        more.push(("Duplicate".to_string(), MenuAct::Duplicate(doc, id.clone())));
    }
    let delete = MenuAct::ConfirmDelete {
        doc,
        id: id.clone(),
        label: title.clone(),
        name: title.clone(),
    };
    more.push((format!("Delete {}…", doc.noun()), delete));
    MenuRow {
        text,
        tone,
        enter: Some(open),
        d: enabled.map(|enabled| MenuAct::SetEnabled {
            doc,
            id,
            label: title,
            enabled: !enabled,
        }),
        more,
    }
}

/// `{source} · run {task} with {agent}`, the desktop's trigger meta (`TriggersPanel.tsx:477`,
/// `automation.ts:62-72,111-116`).
pub(super) fn trigger_in_words(trigger: &Value, sources: &Sources) -> String {
    let source = source_in_words(trigger, sources).unwrap_or_else(|| "No source".to_string());
    let action = match task_of(trigger, sources) {
        None => "run a missing task".to_string(),
        Some(task) => {
            let agent = agent_of(task, sources);
            let with = agent.map_or(String::new(), |agent| {
                format!(" with {}", agent.display_name)
            });
            format!("run {}{with}", task_name(task))
        }
    };
    format!("{source} · {action}")
}

/// Why `trigger` won't fire (`Off` when it is off), or the note it shows when it will: the
/// desktop's `triggerReadiness` (`automation.ts:82-109`). Of the agent's own readiness only its
/// being off is known here, in `agentReadiness`'s words (`agent-readiness.ts:6`).
pub(super) fn trigger_readiness(
    trigger: &Value,
    sources: &Sources,
) -> Result<Option<String>, String> {
    if trigger["enabled"] == false {
        return Err("Off".to_string());
    }
    if source_in_words(trigger, sources).is_none() {
        let missing = match trigger["source"]["kind"] == "schedule" {
            true => "Schedule is missing",
            false => "Event source is missing",
        };
        return Err(missing.to_string());
    }
    let task = task_of(trigger, sources).ok_or("Task is missing")?;
    if task["enabled"] == false {
        return Err("Task is disabled".to_string());
    }
    let agent = agent_of(task, sources).ok_or("Agent is missing")?;
    if !agent.enabled {
        return Err(format!("{}: agent is disabled", agent.display_name));
    }
    if last_run_failed(trigger) {
        let error = trigger["last_error"]
            .as_str()
            .filter(|error| !error.is_empty());
        let error = error.map_or(String::new(), |error| format!(": {error}"));
        return Ok(Some(format!("Last run failed{error}")));
    }
    let next = trigger["next_run_at"]
        .as_str()
        .filter(|next| !next.is_empty());
    Ok(next.map(|next| format!("Next {}", local_time(next))))
}

/// The RFC 3339 time `at` in the local zone, or as it is when it does not parse.
pub(super) fn local_time(at: &str) -> String {
    let local = DateTime::parse_from_rfc3339(at).map(|at| {
        at.with_timezone(&Local)
            .format("%Y-%m-%d %H:%M")
            .to_string()
    });
    local.unwrap_or_else(|_| at.to_string())
}

/// A task's name as the desktop's views carry it, else its id (`snapshot/runtime.rs:458`).
pub(super) fn task_name(task: &Value) -> &str {
    let name = named(&task["display_name"]).or(task["task_id"].as_str());
    name.unwrap_or_default()
}

/// A schedule writes `error` for a fire that failed; other sources keep `failed`
/// (`automation.ts:74-77`).
fn last_run_failed(trigger: &Value) -> bool {
    matches!(trigger["last_status"].as_str(), Some("error" | "failed"))
}

fn source_in_words(trigger: &Value, sources: &Sources) -> Option<String> {
    let source = &trigger["source"];
    match source["kind"].as_str() {
        Some("schedule") => find(&sources.schedules, "schedule_id", &source["schedule_id"])
            .map(|schedule| cadence_in_words(&schedule["cadence"])),
        _ => find(
            &sources.event_sources,
            "event_source_id",
            &source["event_source_id"],
        )
        .map(event_in_words),
    }
}

fn task_of<'a>(trigger: &Value, sources: &'a Sources) -> Option<&'a Value> {
    find(&sources.tasks, "task_id", &trigger["task_id"])
}

fn agent_of<'a>(task: &Value, sources: &'a Sources) -> Option<&'a AgentView> {
    let agent = &task["agent_id"];
    sources
        .agents
        .iter()
        .find(|known| *agent == known.agent_id.as_str())
}

fn find<'a>(documents: &'a [Value], field: &str, id: &Value) -> Option<&'a Value> {
    documents.iter().find(|document| document[field] == *id)
}

/// The desktop's `cadenceInWords` (`automation.ts:23-54`).
pub(super) fn cadence_in_words(cadence: &Value) -> String {
    let plural = |n: i64, unit: &str| format!("{n} {unit}{}", if n == 1 { "" } else { "s" });
    if cadence["kind"] == "interval" {
        let secs = cadence["interval_secs"].as_i64().unwrap_or_default();
        return match secs {
            3600 => "Every hour".to_string(),
            _ if secs % 3600 == 0 => format!("Every {}", plural(secs / 3600, "hour")),
            60 => "Every minute".to_string(),
            _ if secs % 60 == 0 => format!("Every {}", plural(secs / 60, "minute")),
            _ => format!("Every {}", plural(secs, "second")),
        };
    }
    let expression = cadence["expression"].as_str().unwrap_or_default();
    let parts: Vec<_> = expression.split_whitespace().collect();
    let cron = || format!("Cron {expression}");
    let [min, hour, dom, mon, dow] = parts[..] else {
        return cron();
    };
    let number = |part: &str| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit());
    let step = |part: &str| {
        let n = part.strip_prefix("*/").filter(|n| number(n));
        n.and_then(|n| n.parse::<i64>().ok())
    };
    if dom == "*" && mon == "*" && number(min) && number(hour) {
        let at = format!("{hour:0>2}:{min:0>2}");
        let day = dow.parse::<usize>().ok().filter(|_| number(dow));
        match (dow, day.and_then(|day| DAYS.get(day))) {
            ("*", _) => return format!("Every day at {at}"),
            ("1-5", _) => return format!("Every weekday at {at}"),
            ("0,6" | "6,0", _) => return format!("Every weekend day at {at}"),
            (_, Some(day)) => return format!("Every {day} at {at}"),
            _ => {}
        }
    }
    if hour == "*" && dom == "*" && mon == "*" && dow == "*" {
        if min == "*" {
            return "Every minute".to_string();
        }
        if let Some(n) = step(min) {
            return format!("Every {}", plural(n, "minute"));
        }
    }
    if dom == "*" && mon == "*" && dow == "*" && number(min) {
        if let Some(n) = step(hour) {
            return format!("Every {}", plural(n, "hour"));
        }
        if hour == "*" {
            return format!("Every hour at :{min:0>2}");
        }
    }
    cron()
}

/// The desktop's `eventInWords` (`automation.ts:56-60`).
pub(super) fn event_in_words(source: &Value) -> String {
    let collection = source["source_collection"].as_str().unwrap_or_default();
    let kind = source["event_kind"].as_str().unwrap_or("changed");
    let first = collection.chars().next().map(|c| c.to_ascii_lowercase());
    let article = match first {
        Some('a' | 'e' | 'i' | 'o' | 'u') => "an",
        _ => "a",
    };
    let filtered = source["filter"]
        .as_str()
        .is_some_and(|filter| !filter.is_empty());
    let filter = if filtered { " matching a filter" } else { "" };
    format!("When {article} {collection} is {kind}{filter}")
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn texts(rows: &[MenuRow]) -> Vec<(&str, Tone)> {
        rows.iter()
            .map(|row| (row.text.as_str(), row.tone))
            .collect()
    }

    fn agent(id: &str, name: &str) -> AgentView {
        AgentView {
            agent_id: id.to_string(),
            node_did: "did:key:z".to_string(),
            display_name: name.to_string(),
            description: None,
            context_id: None,
            inference_profile_id: None,
            enabled: true,
            is_default: false,
            tags: Vec::new(),
            created_at: None,
        }
    }

    fn sources() -> Sources {
        Sources {
            schedules: vec![json!({
                "schedule_id": "night",
                "cadence": { "kind": "cron", "expression": "0 2 * * *", "timezone": "UTC" },
            })],
            event_sources: vec![json!({
                "event_source_id": "asks",
                "source_collection": "AgentRequest",
                "event_kind": "created",
                "filter": null,
            })],
            tasks: vec![
                json!({ "task_id": "triage", "display_name": "Triage", "agent_id": "engineer", "enabled": true }),
                json!({ "task_id": "sweep", "display_name": null, "agent_id": "engineer", "enabled": false }),
                json!({ "task_id": "orphan", "display_name": "Orphan", "agent_id": "gone", "enabled": true }),
                json!({ "task_id": "padded", "display_name": " Padded ", "agent_id": "engineer", "enabled": true }),
                json!({ "task_id": "blank", "display_name": "  ", "agent_id": "engineer", "enabled": true }),
            ],
            agents: vec![agent("engineer", "Engineer")],
        }
    }

    fn no_sources() -> Sources {
        Sources {
            schedules: Vec::new(),
            event_sources: Vec::new(),
            tasks: Vec::new(),
            agents: Vec::new(),
        }
    }

    fn trigger(id: &str, name: &str, source: Value, task: &str, enabled: bool) -> Value {
        json!({
            "trigger_id": id,
            "display_name": name,
            "task_id": task,
            "source": source,
            "enabled": enabled,
            "tags": null,
        })
    }

    fn on_schedule(id: &str) -> Value {
        json!({ "kind": "schedule", "schedule_id": id })
    }

    fn on_event(id: &str) -> Value {
        json!({ "kind": "event", "event_source_id": id })
    }

    #[test]
    fn skill_rows_mirror_the_desktop() {
        let rows = skills(&[
            json!({
                "skill_id": "review",
                "name": "review",
                "display_name": "Review",
                "tool_refs": ["read", "grep"],
                "enabled": true,
                "tags": null,
            }),
            json!({
                "skill_id": "draft",
                "name": " draft ",
                "display_name": "  ",
                "tool_refs": ["read"],
                "enabled": false,
                "tags": [],
            }),
            json!({
                "skill_id": "commit",
                "name": null,
                "display_name": null,
                "tool_refs": null,
                "enabled": true,
                "tags": ["gents:pack:git"],
            }),
            json!({
                "skill_id": "odd",
                "display_name": "Odd",
                "tool_refs": [],
                "enabled": true,
                "tags": ["gents:pack:a", "gents:pack:b"],
            }),
        ]);

        assert_eq!(
            texts(&rows),
            [
                (" + New skill", Tone::Accent),
                (" ● Review   2 tools", Tone::Plain),
                (" ○ draft   1 tool · disabled", Tone::Label),
                (" ● Odd   0 tools", Tone::Plain),
                (" git · 1 from this pack", Tone::Label),
                (" ● commit   0 tools", Tone::Plain),
            ]
        );
        let set = |id: &str, label: &str, enabled| {
            Some(MenuAct::SetEnabled {
                doc: Doc::Skill,
                id: id.to_string(),
                label: label.to_string(),
                enabled,
            })
        };
        assert_eq!(rows[1].d, set("review", "Review", false));
        assert_eq!(rows[2].d, set("draft", "draft", true));
        assert_eq!((&rows[4].enter, &rows[4].d), (&None, &None));
        assert_eq!(rows[5].d, set("commit", "commit", false));
    }

    #[test]
    fn every_list_starts_with_new() {
        let firsts = [
            skills(&[]),
            triggers(&[], &no_sources()),
            schedules(&[]),
            event_sources(&[]),
            remote_tools(&[]),
        ]
        .map(|rows| rows[0].clone());

        let new = |text: &str, doc| MenuRow {
            text: text.to_string(),
            tone: Tone::Accent,
            enter: Some(MenuAct::Open(FormRef::New(doc))),
            d: None,
            more: Vec::new(),
        };
        assert_eq!(
            firsts,
            [
                new(" + New skill", Doc::Skill),
                new(" + New trigger", Doc::Trigger),
                new(" + New schedule", Doc::Schedule),
                new(" + New event source", Doc::EventSource),
                new(" + New remote tools", Doc::RemoteTool),
            ]
        );
        assert_eq!(
            texts(&schedules(&[])),
            [
                (" + New schedule", Tone::Accent),
                (" No schedules.", Tone::Label),
                (" A trigger binds a task to a schedule.", Tone::Label),
            ]
        );
    }

    #[test]
    fn rows_offer_open_duplicate_and_delete() {
        let schedule = json!({
            "schedule_id": "daily",
            "display_name": "Daily",
            "cadence": { "kind": "cron", "expression": "0 9 * * *", "timezone": "UTC" },
        });
        let service =
            json!({ "service_id": "local", "display_name": null, "hostname": "127.0.0.1" });

        let open = |doc, id: &str| MenuAct::Open(FormRef::Edit(doc, id.to_string()));
        let delete = |doc, id: &str, title: &str| MenuAct::ConfirmDelete {
            doc,
            id: id.to_string(),
            label: title.to_string(),
            name: title.to_string(),
        };
        let scheduled = &schedules(&[schedule])[1];
        assert_eq!(scheduled.enter, Some(open(Doc::Schedule, "daily")));
        assert_eq!(
            scheduled.more,
            [
                ("Open".to_string(), open(Doc::Schedule, "daily")),
                (
                    "Duplicate".to_string(),
                    MenuAct::Duplicate(Doc::Schedule, "daily".to_string())
                ),
                (
                    "Delete schedule…".to_string(),
                    delete(Doc::Schedule, "daily", "Daily")
                ),
            ]
        );
        let served = &remote_tools(&[service])[1];
        assert_eq!(
            served.more,
            [
                ("Open".to_string(), open(Doc::RemoteTool, "local")),
                (
                    "Delete tool service…".to_string(),
                    delete(Doc::RemoteTool, "local", "local")
                ),
            ]
        );
    }

    #[test]
    fn trigger_rows_name_their_source_task_and_agent() {
        let rows = triggers(
            &[
                trigger("nightly", "Nightly", on_schedule("night"), "triage", true),
                json!({
                    "trigger_id": "asked",
                    "display_name": null,
                    "task_id": "triage",
                    "source": on_event("asks"),
                    "enabled": null,
                }),
                trigger("padded", "Padded", on_schedule("night"), "padded", true),
                trigger("blank", "Blank", on_schedule("night"), "blank", true),
            ],
            &sources(),
        );

        assert_eq!(
            texts(&rows[1..]),
            [
                (
                    " ● Nightly   Every day at 02:00 · run Triage with Engineer",
                    Tone::Plain
                ),
                (
                    " ● asked   When an AgentRequest is created · run Triage with Engineer",
                    Tone::Plain
                ),
                (
                    " ● Padded   Every day at 02:00 · run Padded with Engineer",
                    Tone::Plain
                ),
                (
                    " ● Blank   Every day at 02:00 · run blank with Engineer",
                    Tone::Plain
                ),
            ]
        );
        assert_eq!(
            rows[1].d,
            Some(MenuAct::SetEnabled {
                doc: Doc::Trigger,
                id: "nightly".to_string(),
                label: "Nightly".to_string(),
                enabled: false,
            })
        );
    }

    #[test]
    fn a_trigger_whose_agent_is_off_will_not_fire() {
        let mut off = sources();
        off.agents[0].enabled = false;
        let mut nightly = trigger("nightly", "Nightly", on_schedule("night"), "triage", true);
        nightly["next_run_at"] = json!("2026-11-08T09:00:00Z");

        let rows = triggers(&[nightly], &off);

        let words = "Every day at 02:00 · run Triage with Engineer";
        assert_eq!(
            texts(&rows[1..]),
            [(
                format!(" ● Nightly   {words}   Engineer: agent is disabled").as_str(),
                Tone::Warn
            )]
        );
    }

    #[test]
    fn a_trigger_says_what_is_missing() {
        let rows = triggers(
            &[
                trigger("a", "A", on_schedule("gone"), "triage", true),
                trigger("b", "B", on_event("gone"), "triage", true),
                trigger("c", "C", on_schedule("night"), "gone", true),
                trigger("d", "D", on_schedule("night"), "sweep", true),
                trigger("e", "E", on_schedule("night"), "orphan", true),
                trigger("f", "F", on_schedule("night"), "gone", false),
            ],
            &sources(),
        );

        assert_eq!(
            texts(&rows[1..]),
            [
                (
                    " ● A   No source · run Triage with Engineer   Schedule is missing",
                    Tone::Warn
                ),
                (
                    " ● B   No source · run Triage with Engineer   Event source is missing",
                    Tone::Warn
                ),
                (
                    " ● C   Every day at 02:00 · run a missing task   Task is missing",
                    Tone::Warn
                ),
                (
                    " ● D   Every day at 02:00 · run sweep with Engineer   Task is disabled",
                    Tone::Warn
                ),
                (
                    " ● E   Every day at 02:00 · run Orphan   Agent is missing",
                    Tone::Warn
                ),
                (
                    " ○ F   Every day at 02:00 · run a missing task",
                    Tone::Label
                ),
            ]
        );
        assert_eq!(
            rows[6].d,
            Some(MenuAct::SetEnabled {
                doc: Doc::Trigger,
                id: "f".to_string(),
                label: "F".to_string(),
                enabled: true,
            })
        );
    }

    #[test]
    fn a_trigger_shows_its_last_failure_or_next_run() {
        let at = |trigger: Value, fields: Value| {
            let mut trigger = trigger;
            trigger
                .as_object_mut()
                .unwrap()
                .extend(fields.as_object().unwrap().clone());
            trigger
        };
        let nightly = |id, name| trigger(id, name, on_schedule("night"), "triage", true);
        let rows = triggers(
            &[
                at(
                    nightly("failed", "Failed"),
                    json!({ "last_status": "error", "last_error": "the agent timed out" }),
                ),
                at(
                    nightly("quiet", "Quiet"),
                    json!({ "last_status": "failed" }),
                ),
                at(
                    nightly("next", "Next"),
                    json!({ "last_status": "ok", "next_run_at": "2026-11-08T09:00:00Z" }),
                ),
                at(
                    trigger("off", "Off", on_schedule("night"), "triage", false),
                    json!({ "last_status": "error", "next_run_at": "2026-11-08T09:00:00Z" }),
                ),
            ],
            &sources(),
        );

        let next = DateTime::parse_from_rfc3339("2026-11-08T09:00:00Z")
            .unwrap()
            .with_timezone(&Local)
            .format("%Y-%m-%d %H:%M");
        let words = "Every day at 02:00 · run Triage with Engineer";
        assert_eq!(
            texts(&rows[1..]),
            [
                (
                    format!(" ● Failed   {words}   Last run failed: the agent timed out").as_str(),
                    Tone::Warn
                ),
                (
                    format!(" ● Quiet   {words}   Last run failed").as_str(),
                    Tone::Warn
                ),
                (
                    format!(" ● Next   {words}   Next {next}").as_str(),
                    Tone::Plain
                ),
                (format!(" ○ Off   {words}").as_str(), Tone::Label),
            ]
        );
    }

    #[test]
    fn schedule_event_source_and_remote_tool_rows() {
        let schedule_rows = schedules(&[
            json!({
                "schedule_id": "daily",
                "display_name": "Daily",
                "cadence": { "kind": "cron", "expression": "0 9 * * *", "timezone": "UTC" },
            }),
            json!({
                "schedule_id": "often",
                "display_name": null,
                "cadence": { "kind": "interval", "interval_secs": 900 },
            }),
        ]);
        let event_rows = event_sources(&[json!({
            "event_source_id": "asks",
            "display_name": "Requests",
            "source_collection": "AgentRequest",
        })]);
        let tool_rows = remote_tools(&[json!({
            "service_id": "local",
            "display_name": "Local",
            "hostname": "127.0.0.1",
        })]);

        assert_eq!(
            texts(&schedule_rows[1..]),
            [
                (" Daily   0 9 * * *", Tone::Plain),
                (" often   every 900s", Tone::Plain),
            ]
        );
        assert_eq!(
            texts(&event_rows[1..]),
            [(" Requests   AgentRequest", Tone::Plain)]
        );
        assert_eq!(
            texts(&tool_rows[1..]),
            [(" Local   127.0.0.1", Tone::Plain)]
        );
        let rows = [&schedule_rows[1..], &event_rows[1..], &tool_rows[1..]].concat();
        assert!(rows.iter().all(|row| row.d.is_none()), "{rows:?}");
    }

    #[test]
    fn sources_read_in_words_as_the_desktop_does() {
        let cron = |expression: &str| json!({ "kind": "cron", "expression": expression });
        let every = |secs: i64| json!({ "kind": "interval", "interval_secs": secs });
        let cadences = [
            (cron("0 9 * * *"), "Every day at 09:00"),
            (cron("30 8 * * 1-5"), "Every weekday at 08:30"),
            (cron("0 10 * * 6,0"), "Every weekend day at 10:00"),
            (cron("0 9 * * 1"), "Every Monday at 09:00"),
            (cron("* * * * *"), "Every minute"),
            (cron("*/15 * * * *"), "Every 15 minutes"),
            (cron("0 */2 * * *"), "Every 2 hours"),
            (cron("5 * * * *"), "Every hour at :05"),
            (cron("0 9 1 * *"), "Cron 0 9 1 * *"),
            (cron("@daily"), "Cron @daily"),
            (every(3600), "Every hour"),
            (every(7200), "Every 2 hours"),
            (every(60), "Every minute"),
            (every(900), "Every 15 minutes"),
            (every(45), "Every 45 seconds"),
        ];
        for (cadence, words) in cadences {
            assert_eq!(cadence_in_words(&cadence), words, "{cadence}");
        }
        let event = |collection: &str, kind: Value, filter: Value| json!({ "source_collection": collection, "event_kind": kind, "filter": filter });
        let events = [
            (
                event("AgentRequest", json!("created"), Value::Null),
                "When an AgentRequest is created",
            ),
            (
                event("Task", Value::Null, json!("{}")),
                "When a Task is changed matching a filter",
            ),
        ];
        for (event, words) in events {
            assert_eq!(event_in_words(&event), words, "{event}");
        }
    }

    #[test]
    fn ux_rows_say_where_they_came_from() {
        let listing =
            |id: &str, door, producer, pack: Option<&str>, description: Option<&str>| UxListing {
                id: id.to_string(),
                door,
                producer,
                file: format!("/plugins/{id}/plugin.js"),
                pack: pack.map(str::to_string),
                description: description.map(str::to_string),
                contributes: None,
                default_enabled: None,
                data: Vec::new(),
            };

        let rows = ux_plugins(&[
            listing("scratch", UxDoor::Dev, UxProducer::File, None, None),
            listing(
                "acme/review/review_panel",
                UxDoor::Pack,
                UxProducer::Afb,
                Some("acme/review"),
                Some("Reads diffs"),
            ),
            listing(
                "acme/notes/board",
                UxDoor::Pack,
                UxProducer::File,
                Some("acme/notes"),
                None,
            ),
        ]);

        assert_eq!(
            texts(&rows),
            [
                (" board   board · Pack · acme/notes", Tone::Plain),
                (
                    " review_panel   review_panel · Pack · acme/review · produced by .afb · Reads diffs",
                    Tone::Plain
                ),
                (" scratch   scratch · Dev folder", Tone::Plain),
            ]
        );
        let acts = rows.iter().map(|row| (&row.enter, &row.d, row.more.len()));
        assert!(
            acts.into_iter().all(|acts| acts == (&None, &None, 0)),
            "{rows:?}"
        );
        let pack =
            |id: &str, pack: &str| listing(id, UxDoor::Pack, UxProducer::File, Some(pack), None);
        let twins = ux_plugins(&[
            pack("acme/zeta/board", "acme/zeta"),
            pack("acme/alpha/board", "acme/alpha"),
        ]);
        assert_eq!(
            texts(&twins),
            [
                (" board   board · Pack · acme/zeta", Tone::Plain),
                (" board   board · Pack · acme/alpha", Tone::Plain),
            ],
            "one id sorts as listed, as the desktop claims it for the first listing"
        );
    }

    #[test]
    fn a_new_id_is_its_prefix_and_the_time_in_base_36() {
        let before = chrono::Utc::now().timestamp_millis();
        let id = new_id("trig");
        let after = chrono::Utc::now().timestamp_millis();

        let millis = id
            .strip_prefix("trig-")
            .map(|digits| i64::from_str_radix(digits, 36));
        let millis = millis.unwrap().unwrap();
        assert!((before..=after).contains(&millis), "{id}");
        assert!(id.chars().all(|c| !c.is_ascii_uppercase()), "{id}");
    }
}
