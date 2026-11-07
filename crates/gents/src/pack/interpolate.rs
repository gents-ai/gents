/// Matches exactly the strings in which [`interpolate_with`] substitutes at
/// least one `${NAME}` or `${NAME:-default}`: a `${`, not escaped by `$$`,
/// with a closing `}`. ECMA-262 syntax, for JSON Schema `pattern`; the docs
/// bundle conformance test checks it against this parser.
pub const SUBSTITUTION_PATTERN: &str = r"(^|[^$])(\$\$)*\$\{[^}]*\}";

/// The outer option distinguishes an unknown prior binding from a known unset value.
pub fn resolve_install_variable(
    dependency: bool,
    installed: bool,
    previous: Option<Option<String>>,
    override_value: Option<Option<String>>,
    ambient: Option<String>,
) -> Option<Option<String>> {
    override_value.or_else(|| {
        if dependency && installed {
            previous
        } else {
            Some(ambient)
        }
    })
}

#[cfg(test)]
mod install_variable_conformance {
    use super::resolve_install_variable;
    use serde_json::Value;

    #[test]
    fn variable_precedence_matches_the_executable_model() {
        let output = std::process::Command::new("lake")
            .args([
                "env",
                "lean",
                "--run",
                "Proofs/Conformance/PackInstallVariables.lean",
            ])
            .current_dir(std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("proofs"))
            .output()
            .expect("run the pack-variable model");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let cases: Vec<Value> = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(cases.len(), 72);
        for case in cases {
            let nested = |key: &str| {
                case[key]["present"]
                    .as_bool()
                    .unwrap()
                    .then(|| case[key]["value"].as_str().map(str::to_owned))
            };
            let actual = resolve_install_variable(
                case["dependency"].as_bool().unwrap(),
                case["installed"].as_bool().unwrap(),
                nested("previous"),
                nested("override"),
                case["ambient"].as_str().map(str::to_owned),
            );
            assert_eq!(actual, nested("result"), "{case}");
        }
    }
}

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
