use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{bail, Context, Result};

use super::{systemd_environment, NativeServicePlatform};

pub fn read_environment_file(path: &Path) -> Result<BTreeMap<String, String>> {
    let bytes = std::fs::read(path).context("reading the service environment file")?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| {
        anyhow::anyhow!(
            "service environment file is invalid JSON at line {}, column {}",
            error.line(),
            error.column()
        )
    })?;
    let object = value
        .as_object()
        .context("service environment file must be a JSON object")?;
    let environment = object
        .iter()
        .map(|(key, value)| {
            Ok((
                key.clone(),
                value
                    .as_str()
                    .with_context(|| {
                        format!("service environment variable {key:?} must be a string")
                    })?
                    .to_owned(),
            ))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    validate(&environment)?;
    Ok(environment)
}

fn owned(key: &str) -> bool {
    matches!(key, "PATH" | "GENTS_SYSTEM_LOG")
}

fn validate(environment: &BTreeMap<String, String>) -> Result<()> {
    for (key, value) in environment {
        let mut chars = key.chars();
        if !chars
            .next()
            .is_some_and(|ch| ch == '_' || ch.is_ascii_alphabetic())
            || !chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
        {
            bail!("invalid service environment variable name {key:?}");
        }
        if owned(key) {
            bail!("{key} is managed by the service installer; omit it from --environment-file");
        }
        if value.chars().any(char::is_control) {
            bail!("service environment variable {key:?} contains unsupported control characters");
        }
    }
    Ok(())
}

pub(super) fn apply(
    platform: NativeServicePlatform,
    definition: String,
    installed: Option<&[u8]>,
    replacement: Option<&BTreeMap<String, String>>,
) -> Result<String> {
    if replacement.is_none() && platform == NativeServicePlatform::Linux {
        let environment = installed
            .map(preserve_systemd)
            .transpose()
            .map(Option::unwrap_or_default)?;
        return apply_systemd(definition, &environment);
    }
    let environment = match replacement {
        Some(environment) => environment.clone(),
        None => {
            let Some(installed) = installed else {
                return Ok(definition);
            };
            let plist = plist::Value::from_reader_xml(installed)
                .context("parsing the installed service environment")?;
            let dictionary = plist
                .as_dictionary()
                .context("installed service is not a dictionary")?;
            let Some(environment) = dictionary.get("EnvironmentVariables") else {
                return Ok(definition);
            };
            environment
                .as_dictionary()
                .context("installed service environment is not a dictionary")?
                .iter()
                .filter(|(key, _)| !owned(key))
                .map(|(key, value)| {
                    Ok((
                        key.clone(),
                        value
                            .as_string()
                            .with_context(|| {
                                format!("installed environment variable {key:?} must be a string")
                            })?
                            .to_owned(),
                    ))
                })
                .collect::<Result<BTreeMap<_, _>>>()?
        }
    };
    validate(&environment)?;
    if environment.is_empty() {
        return Ok(definition);
    }
    match platform {
        NativeServicePlatform::Macos => {
            let mut plist = plist::Value::from_reader_xml(definition.as_bytes())?;
            let target = plist
                .as_dictionary_mut()
                .and_then(|dictionary| dictionary.get_mut("EnvironmentVariables"))
                .and_then(plist::Value::as_dictionary_mut)
                .context("rendered service has no environment dictionary")?;
            for (key, value) in environment {
                target.insert(key, plist::Value::String(value));
            }
            let mut bytes = Vec::new();
            plist.to_writer_xml(&mut bytes)?;
            String::from_utf8(bytes).context("rendering the service environment")
        }
        NativeServicePlatform::Linux => {
            let environment = environment
                .iter()
                .map(|(key, value)| {
                    Ok(format!(
                        "Environment={}\n",
                        systemd_environment(key, value)?
                    ))
                })
                .collect::<Result<String>>()?;
            apply_systemd(definition, &environment)
        }
    }
}

fn apply_systemd(definition: String, environment: &str) -> Result<String> {
    if environment.is_empty() {
        return Ok(definition);
    }
    let (head, service) = definition
        .split_once("\n[Service]\n")
        .context("rendered service has no Service section")?;
    Ok(format!("{head}\n[Service]\n{environment}{service}"))
}

/// Preserve native systemd syntax, including specifiers and external file
/// directives, without treating it as shell syntax or resolving it on this host.
fn preserve_systemd(bytes: &[u8]) -> Result<String> {
    let text = std::str::from_utf8(bytes).context("installed service is not UTF-8")?;
    let mut lines = text.lines();
    let mut service = false;
    let mut output = String::new();
    while let Some(line) = lines.next() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with(';') {
            continue;
        }
        if trimmed.starts_with('[') {
            service = trimmed == "[Service]";
            continue;
        }
        let directive = logical_line(line, &mut lines)?;
        let Some((key, value)) = trimmed.split_once('=') else {
            continue;
        };
        if service
            && matches!(
                key.trim(),
                "Environment" | "EnvironmentFile" | "PassEnvironment" | "UnsetEnvironment"
            )
            && !(key.trim() == "Environment"
                && !directive.contains('\n')
                && owned_assignment(value.trim()))
        {
            output.push_str(&directive);
            output.push('\n');
        }
    }
    Ok(output)
}

fn logical_line(first: &str, lines: &mut std::str::Lines<'_>) -> Result<String> {
    let mut directive = first.to_owned();
    let mut continued = continuation(first);
    while continued {
        let next = lines
            .next()
            .context("installed service has an unfinished continuation")?;
        directive.push('\n');
        directive.push_str(next);
        let trimmed = next.trim();
        if !(trimmed.starts_with('#') || trimmed.starts_with(';')) {
            continued = continuation(next);
        }
    }
    Ok(directive)
}

fn continuation(line: &str) -> bool {
    line.trim_end()
        .chars()
        .rev()
        .take_while(|ch| *ch == '\\')
        .count()
        % 2
        == 1
}

fn owned_assignment(value: &str) -> bool {
    let Some(value) = value
        .strip_prefix('"')
        .and_then(|value| value.strip_suffix('"'))
    else {
        return false;
    };
    let Some((key, _)) = value.split_once('=') else {
        return false;
    };
    if !owned(key) {
        return false;
    }
    let mut escaped = false;
    for ch in value.chars() {
        if escaped {
            escaped = false;
        } else if ch == '\\' {
            escaped = true;
        } else if ch == '"' {
            return false;
        }
    }
    !escaped
}

/// Strip exactly the custom environment directives the installer preserves;
/// retain the rest of the installed service definition byte for byte.
pub(super) fn without_custom_directives(definition: &str) -> Result<String> {
    let custom = preserve_systemd(definition.as_bytes())?;
    let mut result = definition.to_owned();
    for directive in split_directives(&custom)? {
        result = result.replacen(&(directive + "\n"), "", 1);
    }
    Ok(result)
}

fn split_directives(text: &str) -> Result<Vec<String>> {
    let mut lines = text.lines();
    let mut directives = Vec::new();
    while let Some(line) = lines.next() {
        directives.push(logical_line(line, &mut lines)?);
    }
    Ok(directives)
}
