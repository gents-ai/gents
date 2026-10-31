use super::*;

pub(super) fn parse_limit(parsed: &ParsedArgs) -> Result<usize> {
    parsed
        .one("limit")?
        .map(str::parse::<usize>)
        .transpose()
        .context("--limit must be a positive integer")
        .map(|limit| limit.unwrap_or(20))
}

pub(super) fn extract_agent_target(argv: &[String]) -> Result<(Option<String>, Vec<String>)> {
    let mut agent_id = None;
    let mut rest = Vec::with_capacity(argv.len());
    let mut index = 0;
    while index < argv.len() {
        let arg = &argv[index];
        let value = if arg == "--agent" {
            index += 1;
            Some(
                argv.get(index)
                    .filter(|value| !value.starts_with("--"))
                    .context("--agent requires a AGENT_ID")?
                    .clone(),
            )
        } else {
            arg.strip_prefix("--agent=").map(ToOwned::to_owned)
        };
        if let Some(value) = value {
            anyhow::ensure!(
                !value.trim().is_empty(),
                "--agent requires a non-empty AGENT_ID"
            );
            anyhow::ensure!(
                agent_id.replace(value).is_none(),
                "--agent may be supplied once"
            );
        } else {
            rest.push(arg.clone());
        }
        index += 1;
    }
    Ok((agent_id, rest))
}

pub(super) fn extract_allow_drop(argv: &[String]) -> Result<(BTreeSet<String>, Vec<String>)> {
    let mut groups = BTreeSet::new();
    let mut rest = Vec::with_capacity(argv.len());
    let mut index = 0;
    while index < argv.len() {
        if argv[index] == "--allow-drop" {
            index += 1;
            let value = argv
                .get(index)
                .filter(|value| !value.starts_with("--"))
                .context("--allow-drop requires a Tools group name")?;
            let names: Vec<String> = if value.starts_with('[') {
                serde_json::from_str(value).context("allow-drop must name Tools groups")?
            } else {
                value
                    .split(',')
                    .map(|name| name.trim().to_owned())
                    .collect()
            };
            for name in names {
                anyhow::ensure!(
                    SelfConfigTarget::Tools
                        .writable_fields()
                        .contains(&name.as_str()),
                    "allow-drop names unknown Tools group {name:?}; see [\"help\",\"tools\"]"
                );
                groups.insert(name);
            }
        } else {
            rest.push(argv[index].clone());
        }
        index += 1;
    }
    Ok((groups, rest))
}

pub(super) fn parse_patch(argv: &[String], target: SelfConfigTarget) -> Result<SelfConfigPatch> {
    parse_patch_args(ParsedArgs::parse(argv)?, target)
}

pub(super) fn parse_patch_args(
    parsed: ParsedArgs,
    target: SelfConfigTarget,
) -> Result<SelfConfigPatch> {
    anyhow::ensure!(
        parsed.positionals.is_empty(),
        "unexpected positional argument {:?}; use top-level set for fields or clear for optional field names",
        parsed.positionals[0]
    );
    anyhow::ensure!(
        parsed.switches.is_empty(),
        "unexpected switch; use top-level set for fields or clear for optional field names"
    );
    for name in parsed.options.keys() {
        anyhow::ensure!(
            matches!(name.as_str(), "set" | "clear"),
            "unknown patch option --{name}; put fields in top-level set or clear, not options"
        );
    }
    let mut seen = BTreeSet::new();
    let mut patch = Vec::new();
    for assignment in parsed.options.get("set").into_iter().flatten() {
        let (field, raw) = assignment
            .split_once('=')
            .context("--set must be FIELD=JSON; strings need JSON quotes")?;
        anyhow::ensure!(!field.is_empty(), "--set field must not be empty");
        anyhow::ensure!(
            seen.insert(field.to_owned()),
            "field {field:?} was supplied more than once"
        );
        let value = serde_json::from_str(raw).with_context(|| {
            format!("invalid JSON value for field {field:?}; strings need JSON quotes")
        })?;
        patch.push((field.to_owned(), Some(value)));
    }
    for field in parsed.options.get("clear").into_iter().flatten() {
        anyhow::ensure!(
            seen.insert(field.to_owned()),
            "field {field:?} was supplied more than once"
        );
        patch.push((field.to_owned(), None));
    }
    anyhow::ensure!(
        !patch.is_empty(),
        "empty patch; use top-level set for fields or clear for optional field names"
    );
    crate::store::config_client::patch::ensure_admissible(target, &patch)?;
    Ok(patch)
}

#[derive(Default)]
pub(super) struct ParsedArgs {
    pub(super) positionals: Vec<String>,
    pub(super) options: BTreeMap<String, Vec<String>>,
    pub(super) switches: BTreeSet<String>,
}

impl ParsedArgs {
    pub(super) fn parse(argv: &[String]) -> Result<Self> {
        let mut parsed = Self::default();
        let mut index = 0;
        while index < argv.len() {
            let arg = &argv[index];
            if let Some(raw) = arg.strip_prefix("--") {
                if raw.is_empty() {
                    bail!("empty option name");
                }
                if raw == "default" {
                    anyhow::ensure!(parsed.switches.insert(raw.into()), "duplicate --default");
                    index += 1;
                    continue;
                }
                let (name, inline) = raw
                    .split_once('=')
                    .map_or((raw, None), |(name, value)| (name, Some(value)));
                let value = match inline {
                    Some(value) => value.to_owned(),
                    None => {
                        index += 1;
                        argv.get(index)
                            .filter(|value| !value.starts_with("--"))
                            .cloned()
                            .with_context(|| format!("--{name} requires a value"))?
                    }
                };
                parsed
                    .options
                    .entry(name.to_owned())
                    .or_default()
                    .push(value);
            } else {
                parsed.positionals.push(arg.clone());
            }
            index += 1;
        }
        Ok(parsed)
    }

    pub(super) fn one(&self, name: &str) -> Result<Option<&str>> {
        let Some(values) = self.options.get(name) else {
            return Ok(None);
        };
        anyhow::ensure!(values.len() == 1, "--{name} may be supplied once");
        Ok(values.first().map(String::as_str))
    }

    pub(super) fn reject_mutation_flags(&self) -> Result<()> {
        anyhow::ensure!(
            self.positionals.is_empty(),
            "list command accepts no positional arguments"
        );
        anyhow::ensure!(
            self.switches.is_empty(),
            "read command received a write-only switch"
        );
        for name in self.options.keys() {
            anyhow::ensure!(
                matches!(name.as_str(), "limit" | "cursor"),
                "unknown read option --{name}"
            );
        }
        Ok(())
    }
}
