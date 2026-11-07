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

/// Applies selected variable substitutions to recorded literals. The caller
/// maps authored paths to the corresponding recorded document paths.
pub fn override_installed_literals(
    authored: &serde_json::Value,
    installed: &mut serde_json::Value,
    overrides: &std::collections::BTreeMap<String, Option<String>>,
    known: &std::collections::BTreeMap<String, Option<String>>,
    locate: &dyn Fn(&str) -> Option<String>,
) -> anyhow::Result<()> {
    let uses = inspect_install_variables(authored, &|_| Some("inspection".to_owned())).map_err(
        |names| anyhow::anyhow!("cannot inspect install variables: {}", names.join(", ")),
    )?;
    let mut used = std::collections::BTreeSet::new();
    for entry in uses {
        if !entry
            .variables
            .keys()
            .any(|name| overrides.contains_key(name))
        {
            continue;
        }
        for name in entry.variables.keys() {
            if overrides.contains_key(name) {
                used.insert(name.clone());
            }
            anyhow::ensure!(
                overrides.contains_key(name) || known.contains_key(name),
                "field {} also uses unknown variable {name:?}; supply it explicitly",
                entry.path
            );
        }
        let authored_value = authored
            .pointer(&entry.path)
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| anyhow::anyhow!("invalid authored field {}", entry.path))?;
        let value = interpolate_with(authored_value, &|name| {
            overrides
                .get(name)
                .or_else(|| known.get(name))
                .cloned()
                .flatten()
        })
        .map_err(|names| anyhow::anyhow!("unset variables: {}", names.join(", ")))?;
        anyhow::ensure!(
            !value.starts_with("./"),
            "field {} resolves to a sidecar; reinstall the dependency explicitly",
            entry.path
        );
        let target = locate(&entry.path).ok_or_else(|| {
            anyhow::anyhow!(
                "cannot identify installed field {}; reinstall the dependency explicitly",
                entry.path
            )
        })?;
        let slot = installed
            .pointer_mut(&target)
            .ok_or_else(|| anyhow::anyhow!("installed field {} is missing", entry.path))?;
        *slot = serde_json::Value::String(value);
    }
    for name in overrides.keys() {
        anyhow::ensure!(
            used.contains(name),
            "dependency does not use variable {name:?}"
        );
    }
    Ok(())
}

#[cfg(test)]
mod install_variable_conformance {
    use super::{inspect_install_variables, override_installed_literals, resolve_install_variable};
    use serde_json::Value;

    #[test]
    fn overriding_one_field_preserves_other_installed_literals() {
        use serde_json::json;
        let authored = json!({"first": "${FIRST}", "second": "${SECOND}"});
        let mut installed = json!({"first": "old first", "second": "old second"});
        let override_values =
            std::collections::BTreeMap::from([("FIRST".into(), Some("new first".into()))]);
        override_installed_literals(
            &authored,
            &mut installed,
            &override_values,
            &Default::default(),
            &|path| Some(path.into()),
        )
        .unwrap();
        assert_eq!(
            installed,
            json!({"first": "new first", "second": "old second"})
        );
        let ambiguous = json!({"first": "${FIRST}-${SECOND}"});
        let before = installed.clone();
        assert!(override_installed_literals(
            &ambiguous,
            &mut installed,
            &override_values,
            &Default::default(),
            &|path| Some(path.into())
        )
        .is_err());
        assert_eq!(installed, before);
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
