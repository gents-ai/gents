use anyhow::Result;
use serde_json::Value;

fn parse_string_list(raw: Option<&str>) -> Vec<String> {
    let Some(raw) = raw.map(str::trim).filter(|value| !value.is_empty()) else {
        return Vec::new();
    };
    if let Ok(Value::Array(items)) = serde_json::from_str::<Value>(raw) {
        return items
            .into_iter()
            .filter_map(|item| {
                item.as_str()
                    .map(str::trim)
                    .filter(|s| !s.is_empty())
                    .map(str::to_string)
            })
            .collect();
    }
    raw.split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
}

pub fn reject_secret_bearing_callback_fields(
    binding_id: &str,
    filter: Option<&str>,
    source_fields: Option<&str>,
) -> Result<()> {
    if let Some(filter) = filter.map(str::trim).filter(|filter| !filter.is_empty()) {
        crate::store::graphql::validate_graphql_filter_fragment(filter)?;
        if let Some(field) = secret_field_in_filter(filter) {
            anyhow::bail!("CallbackBinding {binding_id} filter field `{field}` is secret-bearing");
        }
    }
    for field in parse_string_list(source_fields) {
        crate::store::graphql::validate_graphql_name(&field)?;
        if crate::documents::callback::is_secret_env_name(&field) {
            anyhow::bail!("CallbackBinding {binding_id} source field `{field}` is secret-bearing");
        }
    }
    Ok(())
}

fn secret_field_in_filter(filter: &str) -> Option<String> {
    let stripped = strip_graphql_string_literals(filter);
    let mut ident = String::new();
    for ch in stripped.chars() {
        if ch.is_ascii_alphanumeric() || ch == '_' {
            ident.push(ch);
            continue;
        }
        if let Some(secret) = secret_ident(&ident) {
            return Some(secret);
        }
        ident.clear();
    }
    secret_ident(&ident)
}

fn secret_ident(ident: &str) -> Option<String> {
    if ident.is_empty() || ident.starts_with('_') {
        return None;
    }
    crate::documents::callback::is_secret_env_name(ident).then(|| ident.to_string())
}

fn strip_graphql_string_literals(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut chars = input.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch != '"' {
            out.push(ch);
            continue;
        }
        while let Some(next) = chars.next() {
            if next == '\\' {
                let _ = chars.next();
                continue;
            }
            if next == '"' {
                break;
            }
        }
    }
    out
}

pub fn strip_secret_fields(value: Value) -> Value {
    match value {
        Value::Object(map) => Value::Object(
            map.into_iter()
                .filter(|(key, _)| !crate::documents::callback::is_secret_env_name(key))
                .map(|(key, child)| (key, strip_secret_fields(child)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.into_iter().map(strip_secret_fields).collect()),
        other => other,
    }
}
