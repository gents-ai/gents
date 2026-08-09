//! Environment interpolation for desired-state document files.
//!
//! Applies to document JSON only. `.md` sidecars carry runtime `{{ }}` prompt
//! templates and are hydrated untouched, so the two substitution systems never
//! meet.
//!
//! - `${VAR}` — required; an unset variable is an error, never an empty string.
//! - `${VAR:-default}` — falls back to `default` when unset or empty.
//! - `$$` — a literal `$`, so `$${VAR}` survives as the text `${VAR}`.
/// Expand `${VAR}` references using `lookup`. Returns every unresolved
/// variable name rather than the first, so one pass reports all of them.
pub(crate) fn interpolate_with(
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
            // Unterminated: emit verbatim rather than silently eating the rest.
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

/// Expand references in JSON string values without ever interpolating into
/// the JSON grammar itself. Object keys and non-string values are deliberately
/// left untouched.
pub(crate) fn interpolate_json_value_with(
    value: &mut serde_json::Value,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<(), Vec<String>> {
    fn visit(
        value: &mut serde_json::Value,
        lookup: &dyn Fn(&str) -> Option<String>,
        missing: &mut Vec<String>,
    ) {
        match value {
            serde_json::Value::String(input) => match interpolate_with(input, lookup) {
                Ok(expanded) => *input = expanded,
                Err(unresolved) => {
                    for name in unresolved {
                        if !missing.iter().any(|seen| seen == &name) {
                            missing.push(name);
                        }
                    }
                }
            },
            serde_json::Value::Array(values) => {
                for value in values {
                    visit(value, lookup, missing);
                }
            }
            serde_json::Value::Object(values) => {
                for value in values.values_mut() {
                    visit(value, lookup, missing);
                }
            }
            serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {
            }
        }
    }

    // Do not partially rewrite a caller's value when one or more required
    // variables are missing.
    let mut expanded = value.clone();
    let mut missing = Vec::new();
    visit(&mut expanded, lookup, &mut missing);
    if missing.is_empty() {
        *value = expanded;
        Ok(())
    } else {
        Err(missing)
    }
}

/// [`interpolate_json_value_with`] against the process environment.
pub(crate) fn interpolate_json_value(value: &mut serde_json::Value) -> Result<(), Vec<String>> {
    interpolate_json_value_with(value, &|name| std::env::var(name).ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    fn expand(input: &str, pairs: &[(&str, &str)]) -> Result<String, Vec<String>> {
        let map = env(pairs);
        interpolate_with(input, &move |name| map.get(name).cloned())
    }

    fn expand_json(input: &str, pairs: &[(&str, &str)]) -> Result<serde_json::Value, Vec<String>> {
        let map = env(pairs);
        let mut value: serde_json::Value = serde_json::from_str(input).unwrap();
        interpolate_json_value_with(&mut value, &move |name| map.get(name).cloned())?;
        Ok(value)
    }

    #[test]
    fn leaves_text_without_references_untouched() {
        let input = r#"{"endpoint":"http://127.0.0.1:8000/v1"}"#;
        assert_eq!(expand(input, &[]).unwrap(), input);
    }

    #[test]
    fn substitutes_a_set_variable() {
        assert_eq!(
            expand(r#"{"endpoint":"${EP}"}"#, &[("EP", "http://host:1/v1")]).unwrap(),
            r#"{"endpoint":"http://host:1/v1"}"#
        );
    }

    #[test]
    fn falls_back_to_the_default_when_unset_or_empty() {
        assert_eq!(
            expand("${EP:-http://fallback}", &[]).unwrap(),
            "http://fallback"
        );
        assert_eq!(
            expand("${EP:-http://fallback}", &[("EP", "")]).unwrap(),
            "http://fallback"
        );
    }

    #[test]
    fn a_set_variable_wins_over_its_default() {
        assert_eq!(
            expand("${MODEL:-d4f}", &[("MODEL", "other-model")]).unwrap(),
            "other-model"
        );
    }

    #[test]
    fn reports_every_unresolved_variable_rather_than_just_the_first() {
        let missing = expand(r#"{"a":"${ONE}","b":"${TWO}","c":"${ONE}"}"#, &[]).unwrap_err();
        assert_eq!(missing, vec!["ONE".to_string(), "TWO".to_string()]);
    }

    #[test]
    fn an_unset_required_variable_never_becomes_an_empty_string() {
        // Fail closed: silently emitting "" would apply a backend with no
        // endpoint and fail much later, at reconcile.
        assert!(expand(r#"{"endpoint":"${EP}"}"#, &[]).is_err());
    }

    #[test]
    fn double_dollar_escapes_a_literal_reference() {
        assert_eq!(expand("$${EP}", &[("EP", "substituted")]).unwrap(), "${EP}");
        assert_eq!(expand("costs $$5", &[]).unwrap(), "costs $5");
    }

    #[test]
    fn a_lone_dollar_or_unterminated_reference_is_preserved() {
        assert_eq!(expand("100$ and ${OPEN", &[]).unwrap(), "100$ and ${OPEN");
    }

    #[test]
    fn defaults_may_contain_colons_and_slashes() {
        assert_eq!(
            expand("${EP:-http://100.73.235.38:8000/v1}", &[]).unwrap(),
            "http://100.73.235.38:8000/v1"
        );
    }

    #[test]
    fn json_interpolation_expands_only_string_leaves() {
        let expanded = expand_json(
            r#"{
                "endpoint":"${EP}",
                "nested":["model-${MODEL:-d4f}", 3, true, null],
                "${KEY}":"${VALUE}"
            }"#,
            &[
                ("EP", "http://host:1/v1"),
                ("MODEL", "other-model"),
                ("KEY", "renamed"),
                ("VALUE", "expanded"),
            ],
        )
        .unwrap();
        assert_eq!(expanded["endpoint"], "http://host:1/v1");
        assert_eq!(expanded["nested"][0], "model-other-model");
        assert_eq!(expanded["nested"][1], 3);
        assert_eq!(expanded["nested"][2], true);
        assert!(expanded["nested"][3].is_null());
        assert_eq!(expanded["${KEY}"], "expanded");
        assert!(expanded.get("renamed").is_none());
    }

    #[test]
    fn json_interpolation_preserves_quotes_and_backslashes_as_string_data() {
        let substituted = r#"model"name\windows\path"#;
        let expanded = expand_json(r#"{"model":"${MODEL}"}"#, &[("MODEL", substituted)]).unwrap();
        assert_eq!(expanded["model"], substituted);

        let encoded = serde_json::to_vec(&expanded).unwrap();
        let decoded: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded, expanded);
    }

    #[test]
    fn json_interpolation_cannot_inject_object_members() {
        let injected = r#"d4f"],"endpoint":"http://attacker/v1","models":["x"#;
        let expanded = expand_json(
            r#"{"endpoint":"http://trusted/v1","models":["${MODEL}"]}"#,
            &[("MODEL", injected)],
        )
        .unwrap();

        assert_eq!(expanded["endpoint"], "http://trusted/v1");
        assert_eq!(expanded["models"], serde_json::json!([injected]));
        assert_eq!(expanded.as_object().unwrap().len(), 2);

        let encoded = serde_json::to_vec(&expanded).unwrap();
        let decoded: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
        assert_eq!(decoded, expanded);
    }

    #[test]
    fn json_interpolation_reports_missing_variables_across_nested_leaves_atomically() {
        let original: serde_json::Value =
            serde_json::from_str(r#"{"a":"${ONE}","nested":["${TWO}","${ONE}"]}"#).unwrap();
        let mut value = original.clone();
        let missing = interpolate_json_value_with(&mut value, &|_| None).unwrap_err();
        assert_eq!(missing, vec!["ONE".to_string(), "TWO".to_string()]);
        assert_eq!(
            value, original,
            "failed expansion must not partially mutate JSON"
        );
    }
}
