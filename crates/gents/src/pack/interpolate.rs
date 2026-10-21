pub fn interpolate_with(
    input: &str,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<String, Vec<String>> {
    let mut out = String::with_capacity(input.len());
    let mut missing: Vec<String> = Vec::new();
    let bytes = input.as_bytes();
    let mut i = 0;

    while i < bytes.len() {
        if bytes[i] != b'$' {
            let start = i;
            while i < bytes.len() && bytes[i] != b'$' {
                i += 1;
            }
            out.push_str(&input[start..i]);
            continue;
        }

        if bytes.get(i + 1) == Some(&b'$') {
            out.push('$');
            i += 2;
            continue;
        }

        if bytes.get(i + 1) != Some(&b'{') {
            out.push('$');
            i += 1;
            continue;
        }

        let Some(close) = input[i + 2..].find('}').map(|offset| i + 2 + offset) else {
            out.push_str(&input[i..]);
            break;
        };

        let spec = &input[i + 2..close];
        let (name, default) = match spec.split_once(":-") {
            Some((name, default)) => (name.trim(), Some(default)),
            None => (spec.trim(), None),
        };

        match lookup(name).filter(|value| !value.is_empty()) {
            Some(value) => out.push_str(&value),
            None => match default {
                Some(default) => out.push_str(default),
                None => {
                    if !missing.iter().any(|seen| seen == name) {
                        missing.push(name.to_string());
                    }
                }
            },
        }
        i = close + 1;
    }

    if missing.is_empty() {
        Ok(out)
    } else {
        Err(missing)
    }
}

pub fn interpolate(input: &str) -> Result<String, Vec<String>> {
    interpolate_with(input, &|name| std::env::var(name).ok())
}

pub fn escape(text: &str) -> String {
    text.replace('$', "$$")
}

/// Escapes every string in `value` so the pack loader reads it back verbatim.
pub fn escape_values(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(text) => *text = escape(text),
        serde_json::Value::Array(values) => values.iter_mut().for_each(escape_values),
        serde_json::Value::Object(values) => values.values_mut().for_each(escape_values),
        _ => {}
    }
}
