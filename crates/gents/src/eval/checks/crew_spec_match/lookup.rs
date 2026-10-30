use serde_json::Value;

pub(super) fn lookup(row: &Value, field: &str) -> Value {
    let mut current = row.clone();
    for key in field.split('.') {
        if let Value::String(text) = &current {
            if let Ok(parsed) = serde_json::from_str::<Value>(text) {
                current = parsed;
            }
        }
        match current.get(key) {
            Some(next) => current = next.clone(),
            None => return Value::Null,
        }
    }
    current
}

pub(super) fn is_id(stored: &str, id: &str) -> bool {
    stored == id
        || stored
            .strip_suffix(id)
            .is_some_and(|scope| scope.ends_with(':'))
}

pub(super) fn find<'a>(rows: &'a [Value], key: &str, id: &str) -> Option<&'a Value> {
    rows.iter()
        .find(|row| lookup(row, key).as_str() == Some(id))
        .or_else(|| {
            rows.iter().find(|row| {
                lookup(row, key)
                    .as_str()
                    .is_some_and(|stored| is_id(stored, id))
            })
        })
}

pub(super) fn doc_fields(template: &str) -> Vec<String> {
    let mut names = Vec::new();
    let mut rest = template;
    while let Some(start) = rest.find("doc.") {
        let preceded = rest[..start]
            .chars()
            .next_back()
            .is_some_and(|c| c.is_ascii_alphanumeric() || c == '_' || c == '.');
        let tail = &rest[start + 4..];
        let name: String = tail
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
            .collect();
        if !preceded && !name.is_empty() && !names.contains(&name) {
            names.push(name.clone());
        }
        rest = &tail[name.len()..];
    }
    names
}
