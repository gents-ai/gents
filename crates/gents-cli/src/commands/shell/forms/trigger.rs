use gents::documents::{default_agent_id_for_node, PackConfig, Trigger};
use gents_desktop_core::inference::AgentView;
use gents_shell::source::{Doc, FieldKind, Form, FormError, FormRef};
use serde_json::{json, Value};

use super::{
    choice, delete, field, helped, hinted, merged, optional, refused, shown, shown_when, switch,
    tags, tags_field, value,
};
use crate::commands::shell::sections::{self, Sources};

/// The dialog's choice of a new document rather than an existing one (`NewAutomationDialog.tsx:49`).
const NEW: &str = "new:";

/// The dialog's cadences for a new schedule: id, label and cron (`NewAutomationDialog.tsx:52-59`).
const PRESETS: [(&str, &str, &str); 6] = [
    ("daily", "Every day at 09:00", "0 9 * * *"),
    ("weekdays", "Every weekday at 09:00", "0 9 * * 1-5"),
    ("weekly", "Every Monday at 09:00", "0 9 * * 1"),
    ("hourly", "Every hour", "0 * * * *"),
    ("15min", "Every 15 minutes", "*/15 * * * *"),
    ("custom", "Custom cron…", ""),
];

const CONCURRENCY: [(&str, &str); 5] = [
    ("", "Default (parallel)"),
    ("parallel", "Parallel"),
    ("serial", "Serial (skip when busy)"),
    ("queued_serial", "Queued serial"),
    ("latest_only", "Latest only"),
];

/// The desktop's New trigger dialog (`NewAutomationDialog.tsx:288-499`) choosing among `sources`.
pub(super) fn new_form(sources: &Sources) -> Form {
    let or_new = |mut pairs: Vec<(String, String)>, label: &str| {
        pairs.push((NEW.to_string(), label.to_string()));
        FieldKind::Choice(pairs)
    };
    let [tasks, schedules, events] = choices(sources);
    let agents = sources.agents.iter().map(|agent| {
        let default = if agent.is_default { " (default)" } else { "" };
        let off = if agent.enabled { "" } else { " · off" };
        let label = format!("{}{default}{off}", agent.display_name);
        (agent.agent_id.clone(), label)
    });
    // The desktop's `defaultAgentOf`, else the first agent (`:119-120`,
    // `operationalState.ts:84-101`).
    let listed = &sources.agents;
    let conventional =
        |agent: &&AgentView| agent.agent_id == default_agent_id_for_node(&agent.node_did);
    let agent = listed.iter().find(|agent| agent.is_default);
    let agent = agent.or_else(|| listed.iter().find(conventional));
    let agent = agent.or_else(|| listed.iter().find(|agent| agent.enabled));
    let agent = agent.or(listed.first());
    let agent = agent.map_or("", |agent| agent.agent_id.as_str());
    let presets = PRESETS.map(|(id, label, _)| (id, label));
    let fields = vec![
        hinted(
            field("name", "Name", FieldKind::Text, ""),
            "Morning mailbox digest",
        ),
        field(
            "kind",
            "When",
            choice([
                ("schedule", "On a schedule"),
                ("event", "When something happens"),
            ]),
            "schedule",
        ),
        shown_when(
            field(
                "schedule",
                "Schedule",
                or_new(schedules, "New schedule…"),
                NEW,
            ),
            "kind",
            "schedule",
        ),
        shown_when(
            helped(
                field("preset", "Cadence", choice(presets), "daily"),
                &format!("Times are in {}.", zone()),
            ),
            "schedule",
            NEW,
        ),
        shown_when(
            hinted(
                field("cron", "Cron expression", FieldKind::Text, ""),
                "0 9 * * 1-5",
            ),
            "preset",
            "custom",
        ),
        shown_when(
            field("event", "Event", or_new(events, "New event source…"), NEW),
            "kind",
            "event",
        ),
        shown_when(
            hinted(
                field("collection", "Collection", FieldKind::Text, ""),
                "MailboxItem",
            ),
            "event",
            NEW,
        ),
        shown_when(
            field("event_kind", "Event kind", FieldKind::Text, "created"),
            "event",
            NEW,
        ),
        field("task", "What", or_new(tasks, "New task…"), NEW),
        shown_when(
            hinted(
                field("prompt", "Prompt", FieldKind::Lines, ""),
                "Read the mailbox items from the last day and post a short digest.",
            ),
            "task",
            NEW,
        ),
        shown_when(field("agent", "Who", choice(agents), agent), "task", NEW),
        switch("enable", "Turn it on now", false),
    ];
    Form {
        form: FormRef::New(Doc::Trigger),
        title: "New trigger".to_string(),
        note: vec![
            "When it runs, what it does, and which agent does it. Everything else can be tuned \
             afterwards."
                .to_string(),
        ],
        fields,
        actions: Vec::new(),
    }
}

/// The documents the dialog's `values` create, in one component apply, with the dialog's checks
/// (`NewAutomationDialog.tsx:169-267`). The trigger saves as `id`, and each new document under
/// `id`'s stamp.
pub(super) fn created(
    sources: &Sources,
    values: &[(String, String)],
    node_did: &str,
    id: &str,
) -> Result<PackConfig, FormError> {
    let (task_id, prompt, agent_id) = (
        value(values, "task"),
        value(values, "prompt").trim(),
        value(values, "agent"),
    );
    let new_task = task_id == NEW;
    if new_task && prompt.is_empty() {
        return Err(refused("Say what the agent should do."));
    }
    if new_task && agent_id.is_empty() {
        return Err(refused("Choose the agent that runs it."));
    }
    let task = sources.tasks.iter().find(|task| task["task_id"] == task_id);
    let display_name = match value(values, "name").trim() {
        "" if new_task => {
            let first = prompt.lines().next().unwrap_or_default();
            first.chars().take(60).collect()
        }
        "" => task
            .and_then(|task| sections::named(&task["display_name"]))
            .unwrap_or("Automation")
            .to_string(),
        name => name.to_string(),
    };
    let mut config = json!({ "node": { "node_did": node_did } });
    let source = match value(values, "kind") {
        "event" => {
            let mut event_source_id = value(values, "event").to_string();
            if event_source_id == NEW {
                let collection = value(values, "collection").trim();
                if collection.is_empty() {
                    return Err(refused("Name the collection to watch."));
                }
                event_source_id = stamped(id, Doc::EventSource);
                let mut source = json!({
                    "node_did": node_did,
                    "event_source_id": event_source_id,
                    "source_collection": collection,
                    "event_kind": optional(value(values, "event_kind")),
                });
                source["display_name"] = json!(sections::event_in_words(&source));
                config["event_sources"] = json!([source]);
            }
            json!({ "kind": "event", "event_source_id": event_source_id })
        }
        _ => {
            let mut schedule_id = value(values, "schedule").to_string();
            if schedule_id == NEW {
                schedule_id = stamped(id, Doc::Schedule);
                let expression = match value(values, "preset") {
                    "custom" => value(values, "cron"),
                    preset => PRESETS
                        .iter()
                        .find(|(id, ..)| *id == preset)
                        .map_or("", |(.., cron)| cron),
                };
                let cadence = json!({
                    "kind": "cron",
                    "expression": expression.trim(),
                    "timezone": zone(),
                    "missed_run_policy": "latest_only",
                });
                config["schedules"] = json!([{
                    "node_did": node_did,
                    "schedule_id": schedule_id,
                    "display_name": sections::cadence_in_words(&cadence),
                    "cadence": cadence,
                }]);
            }
            json!({ "kind": "schedule", "schedule_id": schedule_id })
        }
    };
    let (task_id, agent_id) = match new_task {
        true => {
            let task_id = stamped(id, Doc::Task);
            config["tasks"] = json!([{
                "node_did": node_did,
                "task_id": task_id,
                "display_name": display_name,
                "description": null,
                "agent_id": agent_id,
                "prompt_template": prompt,
                "emit_outcome": false,
                "goal_objective_template": null,
                "goal_token_budget": null,
                "enabled": true,
                "output_schema_ref": null,
            }]);
            (task_id, agent_id)
        }
        false => {
            let agent = task.and_then(|task| task["agent_id"].as_str());
            (task_id.to_string(), agent.unwrap_or_default())
        }
    };
    let off = sources
        .agents
        .iter()
        .any(|agent| agent.agent_id == agent_id && !agent.enabled);
    config["triggers"] = json!([{
        "node_did": node_did,
        "trigger_id": id,
        "display_name": display_name,
        "task_id": task_id,
        "source": source,
        "enabled": value(values, "enable") == "on" && !off,
        "concurrency": "serial",
    }]);
    serde_json::from_value(config).map_err(refused)
}

/// The desktop's trigger editor (`TriggersPanel.tsx:163-445`) on `trigger` as the runtime
/// observes it, choosing among `sources`.
pub(super) fn form(trigger: &Value, sources: &Sources) -> Form {
    let id = shown(&trigger["trigger_id"]);
    let source = &trigger["source"];
    let kind = source["kind"].as_str().unwrap_or_default();
    // Switching the source kind in the desktop selects that kind's first source (`:288-297`).
    let source_id = |own: &str, key: &str, documents: &[Value]| match kind == own {
        true => shown(&source[key]),
        false => documents
            .first()
            .map_or(String::new(), |first| shown(&first[key])),
    };
    let mut note = vec![sections::trigger_in_words(trigger, sources)];
    note.extend(match sections::trigger_readiness(trigger, sources) {
        Ok(note) => note,
        Err(reason) => Some(format!("Won’t fire: {reason}")),
    });
    let text = |key: &str| shown(&trigger[key]);
    let at = |key: &str| {
        let at = trigger[key].as_str().filter(|at| !at.is_empty());
        at.map_or("—".to_string(), sections::local_time)
    };
    let fixed = |key: &str, label: &str, value: &str| field(key, label, FieldKind::Fixed, value);
    let [tasks, schedules, events] = choices(sources);
    let fired = format!("{} times", trigger["fire_count"].as_i64().unwrap_or(0));
    let mut fields = vec![
        field("trigger", "Trigger", FieldKind::Heading, ""),
        fixed("trigger_id", "Trigger ID", &id),
        field(
            "display_name",
            "Display name",
            FieldKind::Text,
            &text("display_name"),
        ),
        field(
            "description",
            "Description",
            FieldKind::Lines,
            &text("description"),
        ),
        helped(
            field(
                "task_id",
                "Task",
                FieldKind::Choice(tasks),
                &text("task_id"),
            ),
            "The prompt that runs, and the agent that runs it.",
        ),
        field(
            "source",
            "Source",
            choice([("schedule", "Schedule"), ("event", "Event source")]),
            kind,
        ),
        shown_when(
            field(
                "schedule_id",
                "Schedule",
                FieldKind::Choice(schedules),
                &source_id("schedule", "schedule_id", &sources.schedules),
            ),
            "source",
            "schedule",
        ),
        shown_when(
            field(
                "event_source_id",
                "Event source",
                FieldKind::Choice(events),
                &source_id("event", "event_source_id", &sources.event_sources),
            ),
            "source",
            "event",
        ),
        switch("enabled", "Enabled", trigger["enabled"] != false),
        helped(
            field(
                "session_id_template",
                "Existing session template",
                FieldKind::Text,
                &text("session_id_template"),
            ),
            "Optional session ID or template such as {{ doc.lead_session_id }}. Busy sessions \
             queue the request. Blank starts a new session.",
        ),
        field(
            "concurrency",
            "Concurrency",
            choice(CONCURRENCY),
            &text("concurrency"),
        ),
        field("runs", "Runs", FieldKind::Heading, ""),
        fixed("fired", "Fired", &fired),
        fixed("last_attempt", "Last attempt", &at("last_attempt_at")),
        fixed(
            "last_status",
            "Last status",
            trigger["last_status"].as_str().unwrap_or("—"),
        ),
    ];
    let error = trigger["last_error"]
        .as_str()
        .filter(|error| !error.is_empty());
    fields.extend(error.map(|error| fixed("last_error", "Last error", error)));
    fields.extend([
        fixed("next_run", "Next run", &at("next_run_at")),
        field("metadata", "Metadata", FieldKind::Heading, ""),
        tags_field(trigger),
    ]);
    let title = sections::title(Doc::Trigger, trigger);
    Form {
        form: FormRef::Edit(Doc::Trigger, id),
        title: title.clone(),
        note,
        fields,
        actions: vec![delete(Doc::Trigger, title)],
    }
}

/// The trigger `values` save as `id`, over `current`, as the desktop's editor writes it
/// (`TriggersPanel.tsx:95-110`).
pub(super) fn document(
    current: &Value,
    values: &[(String, String)],
    node_did: &str,
    id: &str,
) -> Result<Trigger, FormError> {
    let source = match value(values, "source") {
        "schedule" => json!({ "kind": "schedule", "schedule_id": value(values, "schedule_id") }),
        _ => json!({ "kind": "event", "event_source_id": value(values, "event_source_id") }),
    };
    let fields = json!({
        "node_did": node_did,
        "trigger_id": id,
        "display_name": optional(value(values, "display_name")),
        "description": optional(value(values, "description")),
        "task_id": value(values, "task_id"),
        "enabled": value(values, "enabled") == "on",
        "concurrency": optional(value(values, "concurrency")),
        "session_id_template": optional(value(values, "session_id_template")),
        "source": source,
        "tags": tags(Some(current), values),
    });
    merged(Some(current), fields).map_err(refused)
}

/// The dialog's new `doc` id: `id`'s stamp under `doc`'s prefix. The desktop makes the dialog's
/// ids in one moment, so they share it (`NewAutomationDialog.tsx:133-134`, `draft.ts:333`).
fn stamped(id: &str, doc: Doc) -> String {
    let stamp = id
        .strip_prefix(sections::prefix(Doc::Trigger))
        .unwrap_or(id);
    format!("{}{stamp}", sections::prefix(doc))
}

/// The zone a new schedule runs in, the desktop's `TZ` (`NewAutomationDialog.tsx:50`).
fn zone() -> String {
    iana_time_zone::get_timezone().unwrap_or_else(|_| "UTC".to_string())
}

/// The node's tasks, schedules and event sources as `(id, name)` choices, named as the dialog and
/// the editor list them (`NewAutomationDialog.tsx:341-344,386-389,424-427`).
fn choices(sources: &Sources) -> [Vec<(String, String)>; 3] {
    let tasks = sources.tasks.iter().map(|task| {
        let name = sections::task_name(task).to_string();
        (shown(&task["task_id"]), name)
    });
    let schedules = sources.schedules.iter().map(|schedule| {
        let name = schedule["display_name"].as_str().map(str::to_string);
        let name = name.unwrap_or_else(|| sections::cadence_in_words(&schedule["cadence"]));
        (shown(&schedule["schedule_id"]), name)
    });
    let events = sources.event_sources.iter().map(|source| {
        let name = source["display_name"].as_str().map(str::to_string);
        let name = name.unwrap_or_else(|| sections::event_in_words(source));
        (shown(&source["event_source_id"]), name)
    });
    [tasks.collect(), schedules.collect(), events.collect()]
}

#[cfg(test)]
mod tests {
    use gents::documents::{ConcurrencyMode, Trigger, TriggerSource};
    use gents_desktop_core::inference::AgentView;
    use gents_shell::source::{Doc, FormError, FormRef};
    use serde_json::json;

    use super::*;
    use crate::commands::shell::sections::{self, Sources};
    use crate::commands::shell::testing::{
        agent, conditions, keys_of, options, sent, shown_values,
    };

    fn sources() -> Sources {
        Sources {
            schedules: vec![
                json!({
                    "schedule_id": "daily",
                    "display_name": "Daily",
                    "cadence": { "kind": "cron", "expression": "0 9 * * *", "timezone": "UTC" },
                }),
                json!({
                    "schedule_id": "night",
                    "display_name": null,
                    "cadence": { "kind": "cron", "expression": "0 2 * * *", "timezone": "UTC" },
                }),
            ],
            event_sources: vec![json!({
                "event_source_id": "asks",
                "display_name": null,
                "source_collection": "AgentRequest",
                "event_kind": "created",
            })],
            tasks: vec![
                json!({ "task_id": "triage", "display_name": "Triage", "agent_id": "scout", "enabled": true }),
                json!({ "task_id": "sweep", "display_name": " ", "agent_id": "engineer", "enabled": true }),
            ],
            agents: vec![
                agent("engineer", "Engineer", true, true),
                agent("scout", "Scout", false, false),
            ],
        }
    }

    fn refused(message: &str) -> Result<(), FormError> {
        Err(FormError {
            field: None,
            message: message.to_string(),
        })
    }

    #[test]
    fn new_trigger_mirrors_the_desktop_dialog() {
        let new = new_form(&sources());

        assert_eq!(new.form, FormRef::New(Doc::Trigger));
        assert_eq!(new.title, "New trigger");
        assert_eq!(
            new.note,
            ["When it runs, what it does, and which agent does it. Everything else can be tuned afterwards."]
        );
        assert_eq!(
            shown_values(&new),
            [
                ("name", ""),
                ("kind", "schedule"),
                ("schedule", "new:"),
                ("preset", "daily"),
                ("cron", ""),
                ("event", "new:"),
                ("collection", ""),
                ("event_kind", "created"),
                ("task", "new:"),
                ("prompt", ""),
                ("agent", "engineer"),
                ("enable", "off"),
            ]
        );
        assert!(keys_of(&new, &FieldKind::Fixed).is_empty());
        assert_eq!(keys_of(&new, &FieldKind::Lines), ["prompt"]);
        let schedules = [
            ("daily", "Daily"),
            ("night", "Every day at 02:00"),
            ("new:", "New schedule…"),
        ];
        assert_eq!(options(&new, "schedule"), schedules);
        let events = [
            ("asks", "When an AgentRequest is created"),
            ("new:", "New event source…"),
        ];
        assert_eq!(options(&new, "event"), events);
        let tasks = [
            ("triage", "Triage"),
            ("sweep", "sweep"),
            ("new:", "New task…"),
        ];
        assert_eq!(options(&new, "task"), tasks);
        let agents = [("engineer", "Engineer (default)"), ("scout", "Scout · off")];
        assert_eq!(options(&new, "agent"), agents);
        assert_eq!(
            conditions(&new),
            [
                ("schedule", "kind", "schedule"),
                ("preset", "schedule", "new:"),
                ("cron", "preset", "custom"),
                ("event", "kind", "event"),
                ("collection", "event", "new:"),
                ("event_kind", "event", "new:"),
                ("prompt", "task", "new:"),
                ("agent", "task", "new:"),
            ]
        );
        assert_eq!(new.actions, []);
    }

    #[test]
    fn a_new_trigger_is_run_by_the_default_agent() {
        let who = |agents: Vec<AgentView>| {
            let sources = Sources {
                agents,
                ..sources()
            };
            let new = new_form(&sources).fields.into_iter();
            new.filter(|field| field.key == "agent")
                .map(|field| field.value)
                .collect::<Vec<_>>()
        };

        let default_second = who(vec![
            agent("analyst", "Analyst", true, false),
            agent("engineer", "Engineer", true, true),
        ]);
        let conventional = who(vec![
            agent("analyst", "Analyst", true, false),
            agent("owner:default", "Default", true, false),
        ]);
        let first_enabled = who(vec![
            agent("scout", "Scout", false, false),
            agent("analyst", "Analyst", true, false),
        ]);
        let all_off = who(vec![
            agent("scout", "Scout", false, false),
            agent("analyst", "Analyst", false, false),
        ]);

        assert_eq!(default_second, ["engineer"]);
        assert_eq!(conventional, ["owner:default"]);
        assert_eq!(first_enabled, ["analyst"]);
        assert_eq!(all_off, ["scout"]);
    }

    #[test]
    fn a_new_trigger_needs_a_prompt_and_an_agent() {
        let new_task = |prompt: &str, agent: &str| {
            let values = [
                ("kind", "schedule"),
                ("schedule", "daily"),
                ("task", "new:"),
                ("prompt", prompt),
                ("agent", agent),
            ];
            created(&sources(), &sent(&values), "owner", "trig-a").map(|_| ())
        };
        let existing = |task: &str| {
            let values = [
                ("name", ""),
                ("kind", "schedule"),
                ("schedule", "daily"),
                ("task", task),
                ("enable", "on"),
            ];
            let created = created(&sources(), &sent(&values), "owner", "trig-a");
            let trigger = created.unwrap().triggers.remove(0);
            (trigger.display_name.unwrap(), trigger.enabled)
        };

        assert_eq!(
            new_task(" \n ", "engineer"),
            refused("Say what the agent should do.")
        );
        assert_eq!(
            new_task("Sweep", ""),
            refused("Choose the agent that runs it.")
        );
        assert_eq!(
            existing("triage"),
            ("Triage".to_string(), false),
            "Scout is off, so its trigger is created off"
        );
        assert_eq!(existing("sweep"), ("Automation".to_string(), true));
    }

    #[test]
    fn an_event_trigger_needs_a_collection() {
        let event = |collection: &str| {
            sent(&[
                ("name", " Asks "),
                ("kind", "event"),
                ("event", "new:"),
                ("collection", collection),
                ("event_kind", " "),
                ("task", "triage"),
                ("enable", "off"),
            ])
        };

        let blank = created(&sources(), &event(" "), "owner", "trig-abc");
        let watched = created(&sources(), &event(" MailboxItem "), "owner", "trig-abc");

        assert_eq!(blank.map(|_| ()), refused("Name the collection to watch."));
        assert_eq!(
            serde_json::to_value(watched.unwrap()).unwrap(),
            json!({
                "node": { "node_did": "owner" },
                "event_sources": [{
                    "node_did": "owner",
                    "event_source_id": "evsrc-abc",
                    "display_name": "When a MailboxItem is changed",
                    "source_collection": "MailboxItem",
                }],
                "triggers": [{
                    "node_did": "owner",
                    "trigger_id": "trig-abc",
                    "display_name": "Asks",
                    "task_id": "triage",
                    "source": { "kind": "event", "event_source_id": "evsrc-abc" },
                    "enabled": false,
                    "concurrency": "serial",
                }],
            })
        );
    }

    #[test]
    fn trigger_form_mirrors_the_desktop() {
        let nightly = json!({
            "node_did": "owner",
            "trigger_id": "nightly",
            "display_name": "Nightly",
            "description": "Sweeps.\nDaily.",
            "task_id": "triage",
            "source": { "kind": "schedule", "schedule_id": "night" },
            "session_id_template": null,
            "enabled": null,
            "concurrency": "serial",
            "tags": ["gents:pack:ops", "ops"],
            "fire_count": 3,
            "last_attempt_at": "2026-11-07T09:00:00Z",
            "last_status": "error",
            "last_error": "agent busy",
            "next_run_at": "2026-11-08T02:00:00Z",
        });
        let asked = json!({
            "trigger_id": "asked",
            "task_id": "gone",
            "source": { "kind": "event", "event_source_id": "asks" },
            "enabled": false,
            "last_error": "",
        });

        let mut scout_on = sources();
        scout_on.agents[1].enabled = true;

        let edit = form(&nightly, &sources());
        let off = form(&asked, &sources());
        let ready = form(&nightly, &scout_on);

        assert_eq!(
            edit.form,
            FormRef::Edit(Doc::Trigger, "nightly".to_string())
        );
        assert_eq!(edit.title, "Nightly");
        assert_eq!(
            edit.note,
            [
                "Every day at 02:00 · run Triage with Scout",
                "Won’t fire: Scout: agent is disabled"
            ]
        );
        assert_eq!(
            ready.note,
            [
                "Every day at 02:00 · run Triage with Scout",
                "Last run failed: agent busy"
            ]
        );
        let (attempt, next) = (
            sections::local_time("2026-11-07T09:00:00Z"),
            sections::local_time("2026-11-08T02:00:00Z"),
        );
        assert_eq!(
            shown_values(&edit),
            [
                ("trigger", ""),
                ("trigger_id", "nightly"),
                ("display_name", "Nightly"),
                ("description", "Sweeps.\nDaily."),
                ("task_id", "triage"),
                ("source", "schedule"),
                ("schedule_id", "night"),
                ("event_source_id", "asks"),
                ("enabled", "on"),
                ("session_id_template", ""),
                ("concurrency", "serial"),
                ("runs", ""),
                ("fired", "3 times"),
                ("last_attempt", attempt.as_str()),
                ("last_status", "error"),
                ("last_error", "agent busy"),
                ("next_run", next.as_str()),
                ("metadata", ""),
                ("tags", "ops"),
            ]
        );
        let fixed = [
            "trigger_id",
            "fired",
            "last_attempt",
            "last_status",
            "last_error",
            "next_run",
        ];
        assert_eq!(keys_of(&edit, &FieldKind::Fixed), fixed);
        assert_eq!(keys_of(&edit, &FieldKind::Lines), ["description"]);
        let tasks = [("triage", "Triage"), ("sweep", "sweep")];
        assert_eq!(options(&edit, "task_id"), tasks);
        let schedules = [("daily", "Daily"), ("night", "Every day at 02:00")];
        assert_eq!(options(&edit, "schedule_id"), schedules);
        let events = [("asks", "When an AgentRequest is created")];
        assert_eq!(options(&edit, "event_source_id"), events);
        assert_eq!(
            conditions(&edit),
            [
                ("schedule_id", "source", "schedule"),
                ("event_source_id", "source", "event"),
            ]
        );
        assert_eq!(
            edit.actions,
            [super::super::delete(Doc::Trigger, "Nightly".to_string())]
        );

        assert_eq!(off.title, "asked");
        assert_eq!(
            off.note,
            [
                "When an AgentRequest is created · run a missing task",
                "Won’t fire: Off"
            ]
        );
        assert_eq!(
            shown_values(&off)[5..],
            [
                ("source", "event"),
                ("schedule_id", "daily"),
                ("event_source_id", "asks"),
                ("enabled", "off"),
                ("session_id_template", ""),
                ("concurrency", ""),
                ("runs", ""),
                ("fired", "0 times"),
                ("last_attempt", "—"),
                ("last_status", "—"),
                ("next_run", "—"),
                ("metadata", ""),
                ("tags", ""),
            ]
        );
    }

    #[test]
    fn an_edit_keeps_what_the_form_does_not_show() {
        let stored = json!({
            "_docID": "bae-nightly",
            "node_did": "owner",
            "trigger_id": "nightly",
            "display_name": "Nightly",
            "description": null,
            "task_id": "triage",
            "source": { "kind": "schedule", "schedule_id": "daily" },
            "session_id_template": null,
            "enabled": true,
            "concurrency": null,
            "created_at": "2026-01-01T00:00:00Z",
            "updated_at": "2026-01-02T00:00:00Z",
            "tags": ["gents:pack:ops", "ops"],
        });
        let values = sent(&[
            ("display_name", " Mornings "),
            ("description", ""),
            ("task_id", "sweep"),
            ("source", "event"),
            ("event_source_id", "asks"),
            ("enabled", "off"),
            ("session_id_template", " {{ doc.lead_session_id }} "),
            ("concurrency", "queued_serial"),
            ("tags", "ops, daily"),
        ]);

        let saved = document(&stored, &values, "owner", "nightly");

        assert_eq!(
            saved,
            Ok(Trigger {
                node_did: "owner".to_string(),
                trigger_id: "nightly".to_string(),
                display_name: Some("Mornings".to_string()),
                description: None,
                task_id: "sweep".to_string(),
                source: TriggerSource::Event {
                    event_source_id: "asks".to_string(),
                },
                session_id_template: Some("{{ doc.lead_session_id }}".to_string()),
                enabled: false,
                concurrency: Some(ConcurrencyMode::QueuedSerial),
                created_at: Some("2026-01-01T00:00:00Z".to_string()),
                updated_at: Some("2026-01-02T00:00:00Z".to_string()),
                tags: vec![
                    "gents:pack:ops".to_string(),
                    "ops".to_string(),
                    "daily".to_string()
                ],
            })
        );
    }
}
