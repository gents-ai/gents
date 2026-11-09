use gents_desktop_core::inference::AgentView;
use gents_shell::sample::Tone;
use gents_shell::source::{FieldKind, Form, MenuAct, MenuRow, Update};

/// `pairs` as the shell sends a form's values.
pub(super) fn sent(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
    let sent = pairs.iter();
    sent.map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

/// Each of `form`'s fields' key and the value it shows.
pub(super) fn shown_values(form: &Form) -> Vec<(&str, &str)> {
    let shown = form.fields.iter();
    shown
        .map(|field| (field.key.as_str(), field.value.as_str()))
        .collect()
}

/// The `(value, label)` pairs `form`'s field `key` chooses among, none for a field that is no
/// choice.
pub(super) fn options<'a>(form: &'a Form, key: &str) -> Vec<(&'a str, &'a str)> {
    let field = form.fields.iter().find(|field| field.key == key);
    match field.map(|field| &field.kind) {
        Some(FieldKind::Choice(pairs)) => pairs
            .iter()
            .map(|(value, label)| (value.as_str(), label.as_str()))
            .collect(),
        _ => Vec::new(),
    }
}

/// Each field `form` shows only while another holds a value: its key, the other's and the value.
pub(super) fn conditions(form: &Form) -> Vec<(&str, &str, &str)> {
    let shown = form.fields.iter().filter_map(|field| {
        let (key, values) = field.when.as_ref()?;
        Some(
            values
                .iter()
                .map(move |value| (field.key.as_str(), key.as_str(), value.as_str())),
        )
    });
    shown.flatten().collect()
}

pub(super) fn agent(id: &str, name: &str, enabled: bool, is_default: bool) -> AgentView {
    AgentView {
        agent_id: id.to_string(),
        node_did: "owner".to_string(),
        display_name: name.to_string(),
        description: None,
        context_id: None,
        inference_profile_id: None,
        enabled,
        is_default,
        tags: Vec::new(),
        created_at: None,
    }
}

pub(super) fn shown_rows(rows: &[MenuRow]) -> Vec<(&str, Tone)> {
    rows.iter()
        .map(|row| (row.text.as_str(), row.tone))
        .collect()
}

/// An Ask's title and its rows' text, tone and enter act.
pub(super) fn question(update: Update) -> (String, Vec<(String, Tone, Option<MenuAct>)>) {
    let Update::Ask { title, rows } = update else {
        panic!("{update:?}");
    };
    let rows = rows.into_iter().map(|row| (row.text, row.tone, row.enter));
    (title, rows.collect())
}
