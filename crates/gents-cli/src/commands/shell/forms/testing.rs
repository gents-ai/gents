use gents_shell::source::Form;

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
