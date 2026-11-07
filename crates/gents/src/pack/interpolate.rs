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
    override_value.or(if dependency && installed {
        previous
    } else {
        Some(ambient)
    })
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct InstallVariableUse {
    pub path: String,
    pub variables: std::collections::BTreeMap<String, Option<String>>,
    pub resolved: String,
}

/// The paths and final literal values use the same interpolation parser as installation.
pub fn inspect_install_variables(
    value: &serde_json::Value,
    environment: &dyn Fn(&str) -> Option<String>,
) -> Result<Vec<InstallVariableUse>, Vec<String>> {
    fn visit(
        value: &serde_json::Value,
        path: &str,
        environment: &dyn Fn(&str) -> Option<String>,
        uses: &mut Vec<InstallVariableUse>,
    ) -> Result<(), Vec<String>> {
        match value {
            serde_json::Value::String(text) => {
                let seen = std::cell::RefCell::new(std::collections::BTreeMap::new());
                let resolved = interpolate_with(text, &|name| {
                    seen.borrow_mut()
                        .entry(name.to_owned())
                        .or_insert_with(|| environment(name))
                        .clone()
                })?;
                let variables = seen.into_inner();
                if !variables.is_empty() {
                    uses.push(InstallVariableUse {
                        path: path.to_owned(),
                        variables,
                        resolved,
                    });
                }
            }
            serde_json::Value::Array(values) => {
                for (index, value) in values.iter().enumerate() {
                    visit(value, &format!("{path}/{index}"), environment, uses)?;
                }
            }
            serde_json::Value::Object(values) => {
                for (name, value) in values {
                    let escaped = name.replace('~', "~0").replace('/', "~1");
                    visit(value, &format!("{path}/{escaped}"), environment, uses)?;
                }
            }
            _ => {}
        }
        Ok(())
    }
    let mut uses = Vec::new();
    visit(value, "", environment, &mut uses)?;
    Ok(uses)
}

pub fn resolve_upgrade_variable(
    previous: Option<String>,
    supplied: Option<String>,
) -> Option<String> {
    supplied.or(previous)
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

#[cfg(test)]
mod install_variable_conformance {
    use super::{inspect_install_variables, resolve_install_variable, resolve_upgrade_variable};
    use serde_json::Value;

    #[test]
    fn upgrade_variable_precedence_matches_the_executable_model() {
        let output = gents_lean_contract::run_prepared_generator(
            &gents_lean_contract::proofs_dir().unwrap(),
            "Proofs.Conformance.PackInstallUpgrade:olean",
            "Proofs/Conformance/PackInstallUpgrade.lean",
        )
        .unwrap();
        let cases: Vec<Value> = serde_json::from_str(&output).unwrap();
        assert_eq!(cases.len(), 4);
        for case in cases {
            let previous = case["previous"].as_str().map(str::to_owned);
            let supplied = case["supplied"].as_str().map(str::to_owned);
            assert_eq!(
                resolve_upgrade_variable(previous, supplied),
                case["result"].as_str().map(str::to_owned)
            );
        }
    }

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

    #[test]
    fn preview_reports_interpolated_document_fields_and_defaults() {
        let config = serde_json::json!({"tasks": [{"prompt": "${TOPIC:-general}"}],
            "a/b": "$${LITERAL} ${HOST}"});
        let uses = inspect_install_variables(&config, &|name| {
            (name == "HOST").then(|| "example.test".to_owned())
        })
        .unwrap();
        assert_eq!(uses.len(), 2);
        let task = uses
            .iter()
            .find(|item| item.path == "/tasks/0/prompt")
            .unwrap();
        assert_eq!(task.resolved, "general");
        assert_eq!(task.variables.get("TOPIC"), Some(&None));
        let escaped = uses.iter().find(|item| item.path == "/a~1b").unwrap();
        assert_eq!(escaped.resolved, "${LITERAL} example.test");
        assert_eq!(
            escaped.variables.get("HOST"),
            Some(&Some("example.test".to_owned()))
        );
        assert!(!escaped.variables.contains_key("LITERAL"));
    }
}
