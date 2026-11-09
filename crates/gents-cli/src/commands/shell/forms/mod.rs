mod event_source;
mod folder;
mod node;
mod remote_tool;
mod schedule;
pub(super) mod skill;
#[cfg(test)]
mod testing;
mod trigger;

use std::fmt::Display;
use std::path::Path;

use gents::store::config_client::ConfigAccess;
use gents_desktop_core::client::{
    apply_config_components_on, resolve_schedule_now_on, upsert_event_source_on,
    upsert_schedule_on, upsert_skill_on, upsert_tool_service_registry_on, upsert_trigger_on,
};
use gents_shell::sample::Tone;
use gents_shell::source::{
    Doc, FieldKind, Form, FormAct, FormAction, FormError, FormField, FormRef,
};
use serde::de::DeserializeOwned;
use serde_json::{json, Value};

use super::menus::{run_task, trigger_sources, NOT_WIRED};
use crate::commands::config::crud::{
    load_one, ConfigDocumentSpec, EVENT_SOURCE_SPEC, MCP_SPEC, SCHEDULE_SPEC, SKILL_SPEC,
    TRIGGER_SPEC,
};

/// The desktop's toast for a saved form (`draft.ts:95`).
const SAVED: &str = "Saved";

/// The tags a form keeps without showing them.
const KEPT_TAGS: &str = "gents:";

/// `form`, read as the node. `id` is its document's, or the one a new document saves under.
pub(super) async fn open(
    access: &ConfigAccess,
    node_did: &str,
    form: &FormRef,
    id: &str,
) -> anyhow::Result<Form> {
    match doc(form) {
        Some(Doc::Skill) => {
            let current = read(access, SKILL_SPEC, node_did, form).await?;
            let users = match current {
                Some(_) => skill::users(access, node_did, id).await?,
                None => Vec::new(),
            };
            Ok(skill::form(current.as_ref(), id, &users))
        }
        Some(Doc::Schedule) => {
            let current = read(access, SCHEDULE_SPEC, node_did, form).await?;
            Ok(schedule::form(current.as_ref(), id))
        }
        Some(Doc::EventSource) => {
            let current = read(access, EVENT_SOURCE_SPEC, node_did, form).await?;
            Ok(event_source::form(current.as_ref(), id))
        }
        Some(Doc::Trigger) => {
            let current = read(access, TRIGGER_SPEC, node_did, form).await?;
            let (triggers, sources) = trigger_sources(access, node_did).await?;
            Ok(match current {
                None => trigger::new_form(&sources),
                Some(current) => {
                    let observed = triggers.iter().find(|trigger| trigger["trigger_id"] == id);
                    trigger::form(observed.unwrap_or(&current), &sources)
                }
            })
        }
        Some(Doc::RemoteTool) => {
            let current = read(access, MCP_SPEC, node_did, form).await?;
            Ok(remote_tool::form(current.as_ref(), id))
        }
        _ if *form == FormRef::Node => node::open(access, node_did).await,
        _ if *form == FormRef::AddFolder => Ok(folder::form()),
        _ => anyhow::bail!(NOT_WIRED),
    }
}

/// Writes `form`'s document with `values` over what is stored, and answers the notice.
pub(super) async fn save(
    access: &ConfigAccess,
    home: &Path,
    node_did: &str,
    form: &FormRef,
    values: &[(String, String)],
    id: &str,
) -> Result<String, FormError> {
    match doc(form) {
        Some(Doc::Skill) => {
            let current = read(access, SKILL_SPEC, node_did, form).await;
            let skill = skill::document(current.map_err(refused)?.as_ref(), values, node_did, id)?;
            upsert_skill_on(access, &skill).await.map_err(refused)?;
        }
        Some(Doc::Schedule) => {
            let current = read(access, SCHEDULE_SPEC, node_did, form).await;
            let current = current.map_err(refused)?;
            let schedule = schedule::document(current.as_ref(), values, node_did, id)?;
            upsert_schedule_on(access, &schedule)
                .await
                .map_err(refused)?;
        }
        Some(Doc::EventSource) => {
            let current = read(access, EVENT_SOURCE_SPEC, node_did, form).await;
            let current = current.map_err(refused)?;
            let source = event_source::document(current.as_ref(), values, node_did, id)?;
            upsert_event_source_on(access, &source)
                .await
                .map_err(refused)?;
        }
        Some(Doc::Trigger) => {
            let current = read(access, TRIGGER_SPEC, node_did, form).await;
            match current.map_err(refused)? {
                None => {
                    let (_, sources) = trigger_sources(access, node_did).await.map_err(refused)?;
                    let created = trigger::created(&sources, values, node_did, id)?;
                    apply_config_components_on(access, &created)
                        .await
                        .map_err(refused)?;
                }
                Some(current) => {
                    let trigger = trigger::document(&current, values, node_did, id)?;
                    upsert_trigger_on(access, &trigger).await.map_err(refused)?;
                }
            }
        }
        Some(Doc::RemoteTool) => {
            let current = read(access, MCP_SPEC, node_did, form).await;
            let current = current.map_err(refused)?;
            let service = remote_tool::document(current.as_ref(), values, node_did, id)?;
            upsert_tool_service_registry_on(access, &service)
                .await
                .map_err(refused)?;
        }
        _ if *form == FormRef::Node => node::save(access, node_did, values).await?,
        _ if *form == FormRef::AddFolder => return folder::add(home, values).await,
        _ => return Err(refused(NOT_WIRED)),
    }
    Ok(SAVED.to_string())
}

/// What `act` on `form`, showing `values` for the document `id`, did, in the tone the form
/// shows it.
pub(super) async fn act(
    access: &ConfigAccess,
    home: &Path,
    node_did: &str,
    form: &FormRef,
    act: &FormAct,
    values: &[(String, String)],
    id: &str,
) -> (String, Tone) {
    match (form, act) {
        (FormRef::Edit(Doc::Schedule, schedule), FormAct::RunSchedule) => {
            match run_schedule(access, home, node_did, schedule).await {
                Ok(request) => (format!("Schedule started · {request}"), Tone::Accent),
                Err(error) => (format!("Couldn’t run the schedule: {error:#}"), Tone::Warn),
            }
        }
        (
            FormRef::New(Doc::RemoteTool) | FormRef::Edit(Doc::RemoteTool, _),
            FormAct::TestConnection,
        ) => remote_tool::test(values, id).await,
        _ => (NOT_WIRED.to_string(), Tone::Warn),
    }
}

/// Runs the task of the enabled trigger bound to `schedule`, and answers its request's id. The
/// desktop's toasts are `SchedulesPanel.tsx:117` and `hooks/taskActions.ts:31,38`.
async fn run_schedule(
    access: &ConfigAccess,
    home: &Path,
    node_did: &str,
    schedule: &str,
) -> anyhow::Result<String> {
    let invocation = resolve_schedule_now_on(access, node_did, schedule).await?;
    Ok(run_task(access, home, &invocation.task_id)
        .await?
        .request_id)
}

fn doc(form: &FormRef) -> Option<Doc> {
    match form {
        FormRef::New(doc) | FormRef::Edit(doc, _) => Some(*doc),
        FormRef::Node
        | FormRef::AddFolder
        | FormRef::Delete(..)
        | FormRef::PackSearch
        | FormRef::PackSignIn => None,
    }
}

/// The stored document an edit shows and saves over; a new form has none.
async fn read(
    access: &ConfigAccess,
    spec: ConfigDocumentSpec,
    node_did: &str,
    form: &FormRef,
) -> anyhow::Result<Option<Value>> {
    match form {
        FormRef::Edit(_, id) => load_one(access, spec, node_did, id).await.map(Some),
        _ => Ok(None),
    }
}

/// `current`, or a new document, with `fields` written over it, as the typed document an upsert
/// replaces whole: every key `fields` leaves out keeps its stored value.
pub(super) fn merged<T>(current: Option<&Value>, fields: Value) -> serde_json::Result<T>
where
    T: DeserializeOwned,
{
    let mut document = current.cloned().unwrap_or_else(|| json!({}));
    if let (Some(document), Value::Object(fields)) = (document.as_object_mut(), fields) {
        document.remove("_docID");
        document.extend(fields);
    }
    serde_json::from_value(document)
}

/// A failure the form shows in its status, in its owner's words.
pub(super) fn refused<E>(error: E) -> FormError
where
    E: Display,
{
    FormError {
        field: None,
        message: format!("{error:#}"),
    }
}

/// A failure shown under the field `key`.
pub(super) fn problem(key: &str, message: &str) -> FormError {
    FormError {
        field: Some(key.to_string()),
        message: message.to_string(),
    }
}

/// The form on `current`, or on a new `doc` while there is none.
pub(super) fn form_ref(doc: Doc, current: Option<&Value>, id: &str) -> FormRef {
    match current {
        Some(_) => FormRef::Edit(doc, id.to_string()),
        None => FormRef::New(doc),
    }
}

pub(super) fn field(key: &str, label: &str, kind: FieldKind, value: &str) -> FormField {
    FormField {
        key: key.to_string(),
        label: label.to_string(),
        kind,
        value: value.to_string(),
        placeholder: String::new(),
        help: None,
        when: None,
    }
}

/// `field` with `help` under it.
pub(super) fn helped(field: FormField, help: &str) -> FormField {
    FormField {
        help: Some(help.to_string()),
        ..field
    }
}

/// `field` with `placeholder` while it is empty.
pub(super) fn hinted(field: FormField, placeholder: &str) -> FormField {
    FormField {
        placeholder: placeholder.to_string(),
        ..field
    }
}

/// `field`, shown only while the field `key` holds `value`.
pub(super) fn shown_when(field: FormField, key: &str, value: &str) -> FormField {
    FormField {
        when: Some((key.to_string(), vec![value.to_string()])),
        ..field
    }
}

/// A switch, `on` or `off`, with the desktop's enabled switch's labels (`SkillsPanel.tsx:148`).
pub(super) fn switch(key: &str, label: &str, on: bool) -> FormField {
    let on_off = choice([("on", "Enabled"), ("off", "Disabled")]);
    let value = if on { "on" } else { "off" };
    field(key, label, on_off, value)
}

/// A choice of `(value, label)` pairs.
pub(super) fn choice<I, V, L>(pairs: I) -> FieldKind
where
    I: IntoIterator<Item = (V, L)>,
    V: Into<String>,
    L: Into<String>,
{
    let pairs = pairs.into_iter();
    FieldKind::Choice(
        pairs
            .map(|(value, label)| (value.into(), label.into()))
            .collect(),
    )
}

/// The desktop's tags row (`editors.tsx:949-971`): the tags comma-separated, without the kept
/// ones.
pub(super) fn tags_field(document: &Value) -> FormField {
    let shown = items(&document["tags"]).filter(|tag| !tag.starts_with(KEPT_TAGS));
    let shown = shown.collect::<Vec<_>>().join(", ");
    hinted(field("tags", "Tags", FieldKind::Text, &shown), "Add a tag")
}

/// The danger zone's "Delete this {noun}", which asks for `name` typed (`ListDetail.tsx:613-638`).
pub(super) fn delete(doc: Doc, name: String) -> FormAction {
    FormAction {
        label: format!("Delete this {}", doc.noun()),
        act: FormAct::Delete {
            label: name.clone(),
            name,
        },
    }
}

/// A field's value as the form shows it: text as it is, a number in digits, nothing as blank.
pub(super) fn shown(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

/// `list`'s strings.
pub(super) fn items(list: &Value) -> impl Iterator<Item = &str> {
    list.as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
}

/// The value sent for `key`; a field not sent (hidden, fixed or a heading) is blank.
pub(super) fn value<'a>(values: &'a [(String, String)], key: &str) -> &'a str {
    let sent = values.iter().find(|(sent, _)| sent == key);
    sent.map_or("", |(_, value)| value)
}

/// `text` trimmed, or null when blank.
pub(super) fn optional(text: &str) -> Value {
    match text.trim() {
        "" => Value::Null,
        text => json!(text),
    }
}

/// The comma-separated items of `text`, each once, or null for none, as the desktop writes an
/// empty list.
pub(super) fn list(text: &str) -> Value {
    listed(split(text))
}

/// The tags typed in `values` after the kept ones of `current`, each once.
pub(super) fn tags(current: Option<&Value>, values: &[(String, String)]) -> Value {
    let kept = current
        .into_iter()
        .flat_map(|document| items(&document["tags"]));
    let kept = kept.filter(|tag| tag.starts_with(KEPT_TAGS));
    listed(kept.chain(split(value(values, "tags"))))
}

fn split(text: &str) -> impl Iterator<Item = &str> {
    text.split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
}

/// `items` in order without repeats, as the desktop's tags row adds them (`editors.tsx:962-970`).
fn listed<'a, I>(items: I) -> Value
where
    I: Iterator<Item = &'a str>,
{
    let mut once: Vec<&str> = Vec::new();
    for item in items {
        if !once.contains(&item) {
            once.push(item);
        }
    }
    match once.is_empty() {
        true => Value::Null,
        false => json!(once),
    }
}

/// The whole number typed for `key`, none when blank, with the desktop's message for one that is
/// not (`draft.ts:133-141`). `label` is the field's.
pub(super) fn whole(
    values: &[(String, String)],
    key: &str,
    label: &str,
) -> Result<Option<i64>, FormError> {
    let typed = value(values, key).trim();
    if typed.is_empty() {
        return Ok(None);
    }
    let message = format!("{label} must be a whole number");
    typed.parse().map(Some).map_err(|_| problem(key, &message))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::testing::sent;
    use super::*;

    #[test]
    fn a_tag_is_saved_once() {
        let stored = json!({ "tags": ["gents:pack:git"] });
        let typed = sent(&[("tags", "a, b, a, gents:pack:git")]);

        let saved = tags(Some(&stored), &typed);

        assert_eq!(saved, json!(["gents:pack:git", "a", "b"]));
        assert_eq!(list("read, grep, read"), json!(["read", "grep"]));
    }
}
