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

/// [`interpolate_with`] against the process environment.
pub(crate) fn interpolate(input: &str) -> Result<String, Vec<String>> {
    interpolate_with(input, &|name| std::env::var(name).ok())
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
}
