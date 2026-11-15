//! TLA+ function tables of the Lean decisions that TLA+ specifications name.
//!
//! A spec names its Lean owners on its second line (`\* lean: A.b, C.d`). An
//! owner exported as a conformance function over a finite domain (every
//! argument a `Bool` or a field-less enum, every result a `Bool`, `Nat` or
//! enum) becomes `<name>_table`, the decision over its whole domain keyed by
//! the tuple of its arguments in Lean parameter order, and `<name>(...)`, its
//! lookup, in `generated/LeanDecision_<Namespace>.tla`.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

pub const MODULE_PREFIX: &str = "LeanDecision_";

/// One family of the conformance snapshot.
pub struct FamilyView<'a> {
    pub input: &'a Value,
    pub output: &'a Value,
    pub cases: Vec<(&'a Value, &'a Value)>,
}

pub fn tla_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../gents/proofs/tla")
}

pub fn generated_dir() -> PathBuf {
    tla_dir().join("generated")
}

/// The Lean owners every spec in `tla` names. A spec without the header names
/// none here; the TLA+ structure check rejects it.
pub fn named_owners(tla: &Path) -> Result<BTreeSet<String>, String> {
    let mut owners = BTreeSet::new();
    let entries = fs::read_dir(tla).map_err(|error| format!("{}: {error}", tla.display()))?;
    for entry in entries {
        let path = entry.map_err(|error| error.to_string())?.path();
        let stem = path
            .file_stem()
            .and_then(|stem| stem.to_str())
            .unwrap_or("");
        if path.extension().is_none_or(|extension| extension != "tla") || stem.starts_with("MC") {
            continue;
        }
        let text =
            fs::read_to_string(&path).map_err(|error| format!("{}: {error}", path.display()))?;
        let Some(list) = text
            .lines()
            .nth(1)
            .and_then(|line| line.strip_prefix("\\* lean: "))
        else {
            continue;
        };
        if list != "none" {
            owners.extend(list.split(", ").map(str::to_owned));
        }
    }
    Ok(owners)
}

/// `Conformance.wireName`: camelCase to snake_case, a run of capitals is one word.
fn wire_name(lean: &str) -> String {
    let chars: Vec<char> = lean.chars().collect();
    let mut out = String::new();
    for (index, &c) in chars.iter().enumerate() {
        let starts_word = c.is_ascii_uppercase()
            && index > 0
            && match chars[index - 1] {
                previous if previous.is_ascii_lowercase() || previous.is_ascii_digit() => true,
                previous if previous.is_ascii_uppercase() => chars
                    .get(index + 1)
                    .is_some_and(|next| next.is_ascii_lowercase()),
                _ => false,
            };
        if starts_word {
            out.push('_');
        }
        out.push(c.to_ascii_lowercase());
    }
    out
}

fn components(owner: &str) -> Vec<&str> {
    let owner = owner.strip_prefix("Conformance.").unwrap_or(owner);
    owner.split('.').collect()
}

/// The snapshot key of a Lean owner (`Conformance.wireKey`; a family's key
/// drops a leading `Conformance` namespace).
pub fn wire_key(owner: &str) -> String {
    components(owner)
        .iter()
        .map(|part| wire_name(part))
        .collect::<Vec<_>>()
        .join("_")
}

fn is_identifier(name: &str) -> bool {
    name.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn domain(schema: &Value) -> Option<Vec<Value>> {
    if schema == "bool" {
        return Some(vec![Value::Bool(false), Value::Bool(true)]);
    }
    schema.get("enum")?;
    Some(schema.get("values")?.as_array()?.clone())
}

fn literal(value: &Value) -> Option<String> {
    match value {
        Value::Bool(true) => Some("TRUE".to_owned()),
        Value::Bool(false) => Some("FALSE".to_owned()),
        Value::Number(number) => number.as_u64().map(|number| number.to_string()),
        Value::String(text) if text.chars().all(|c| c.is_ascii_graphic() || c == ' ') => Some(
            format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\"")),
        ),
        _ => None,
    }
}

/// The decision's table and lookup, or `None` when it is not a function over
/// a finite domain that TLA+ values can carry.
fn render_decision(owner: &str, name: &str, family: &FamilyView<'_>) -> Option<String> {
    if !(family.output == "bool" || family.output == "nat" || family.output.get("enum").is_some()) {
        return None;
    }
    let mut parameters = Vec::new();
    let mut size = 1usize;
    for field in family.input.get("fields")?.as_array()? {
        let field = field.as_array()?;
        let parameter = field.first()?.as_str()?;
        if !is_identifier(parameter) {
            return None;
        }
        size = size.checked_mul(domain(field.get(1)?)?.len())?;
        parameters.push(parameter);
    }
    let mut seen = BTreeSet::new();
    let mut rows = Vec::new();
    for (input, expected) in &family.cases {
        let key = parameters
            .iter()
            .map(|parameter| literal(input.get(*parameter)?))
            .collect::<Option<Vec<_>>>()?
            .join(", ");
        if !seen.insert(key.clone()) {
            return None;
        }
        rows.push((key, literal(expected)?));
    }
    if parameters.is_empty() || rows.len() != size {
        return None;
    }
    let mut text = format!("\\* Lean `{owner}`.\n{name}_table ==\n");
    for (index, (key, result)) in rows.iter().enumerate() {
        let join = if index == 0 { "      " } else { "   @@ " };
        text.push_str(&format!("{join}<<{key}>> :> {result}\n"));
    }
    let arguments = parameters.join(", ");
    text.push_str(&format!(
        "\n{name}({arguments}) ==\n  {name}_table[<<{arguments}>>]\n"
    ));
    Some(text)
}

/// Module file name -> text, for every named owner that is a finite function.
pub fn render_modules<'a>(
    owners: &BTreeSet<String>,
    family: impl Fn(&str) -> Option<FamilyView<'a>>,
) -> Result<BTreeMap<String, String>, String> {
    let mut modules: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for owner in owners {
        let key = wire_key(owner);
        let view = family(&key)
            .ok_or_else(|| format!("Lean owner {owner} has no conformance family {key}"))?;
        let parts = components(owner);
        let (name, namespace) = parts.split_last().expect("an owner has a name");
        if namespace.is_empty() || !is_identifier(name) {
            continue;
        }
        if let Some(text) = render_decision(owner, name, &view) {
            modules
                .entry(format!("{MODULE_PREFIX}{}", namespace.join("_")))
                .or_default()
                .push(text);
        }
    }
    Ok(modules
        .into_iter()
        .map(|(module, decisions)| {
            let text = format!(
                "---- MODULE {module} ----\n\\* Generated by `make lean-regen` from the Lean conformance snapshot; do not edit.\nEXTENDS TLC\n\n{}\n====\n",
                decisions.join("\n")
            );
            (format!("{module}.tla"), text)
        })
        .collect())
}

/// The modules for a raw snapshot JSON, as regeneration reads it.
pub fn render_modules_from_snapshot(
    snapshot: &Value,
    tla: &Path,
) -> Result<BTreeMap<String, String>, String> {
    let families = snapshot
        .get("families")
        .and_then(Value::as_object)
        .ok_or("snapshot has no families object")?;
    render_modules(&named_owners(tla)?, |key| {
        let family = families.get(key)?;
        Some(FamilyView {
            input: family.get("input")?,
            output: family.get("output")?,
            cases: family
                .get("cases")?
                .as_array()?
                .iter()
                .map(|case| Some((case.get("input")?, case.get("expected")?)))
                .collect::<Option<_>>()?,
        })
    })
}

/// Writes `modules` into `dir` and removes generated modules no longer named.
/// Returns whether any file changed.
pub fn write_modules(dir: &Path, modules: &BTreeMap<String, String>) -> std::io::Result<bool> {
    let mut changed = false;
    if dir.exists() {
        for entry in fs::read_dir(dir)? {
            let path = entry?.path();
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("");
            if name.starts_with(MODULE_PREFIX) && !modules.contains_key(name) {
                fs::remove_file(&path)?;
                changed = true;
            }
        }
    }
    if !modules.is_empty() {
        fs::create_dir_all(dir)?;
    }
    for (name, text) in modules {
        let path = dir.join(name);
        if fs::read(&path).is_ok_and(|current| current == text.as_bytes()) {
            continue;
        }
        fs::write(&path, text)?;
        changed = true;
    }
    Ok(changed)
}

/// The generated modules on disk, by file name.
pub fn checked_in_modules(dir: &Path) -> BTreeMap<String, String> {
    let Ok(entries) = fs::read_dir(dir) else {
        return BTreeMap::new();
    };
    entries
        .filter_map(|entry| {
            let path = entry.ok()?.path();
            let name = path.file_name()?.to_str()?.to_owned();
            name.starts_with(MODULE_PREFIX)
                .then(|| Some((name, fs::read_to_string(&path).ok()?)))?
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owners_map_to_the_snapshot_keys_lean_derives() {
        assert_eq!(
            wire_key("TransactionReplay.disposition"),
            "transaction_replay_disposition"
        );
        assert_eq!(
            wire_key("TransactionReplay.Mode.maxAttempts"),
            "transaction_replay_mode_max_attempts"
        );
        assert_eq!(
            wire_key("Conformance.PluginResources.computeAllowances"),
            "plugin_resources_compute_allowances"
        );
        assert_eq!(wire_name("MCPHealth"), "mcp_health");
        assert_eq!(
            wire_name("clientSignedWriteMayRetry"),
            "client_signed_write_may_retry"
        );
    }

    #[test]
    fn a_finite_decision_renders_its_whole_domain_and_nothing_else_does() {
        let input = serde_json::json!({"struct": "Input", "fields": [
            ["mode", {"enum": "Mode", "values": ["a", "b"]}], ["flag", "bool"]]});
        let output = serde_json::json!({"enum": "Out", "values": ["yes", "no"]});
        let cases: Vec<(Value, Value)> = [
            ("a", false, "no"),
            ("a", true, "yes"),
            ("b", false, "no"),
            ("b", true, "no"),
        ]
        .into_iter()
        .map(|(mode, flag, out)| {
            (
                serde_json::json!({"mode": mode, "flag": flag}),
                serde_json::json!(out),
            )
        })
        .collect();
        fn view<'a>(
            input: &'a Value,
            output: &'a Value,
            cases: &'a [(Value, Value)],
        ) -> FamilyView<'a> {
            FamilyView {
                input,
                output,
                cases: cases.iter().map(|(i, e)| (i, e)).collect(),
            }
        }
        let text = render_decision("Ns.decide", "decide", &view(&input, &output, &cases)).unwrap();
        assert_eq!(
            text,
            "\\* Lean `Ns.decide`.\ndecide_table ==\n      <<\"a\", FALSE>> :> \"no\"\n   @@ <<\"a\", TRUE>> :> \"yes\"\n   @@ <<\"b\", FALSE>> :> \"no\"\n   @@ <<\"b\", TRUE>> :> \"no\"\n\ndecide(mode, flag) ==\n  decide_table[<<mode, flag>>]\n"
        );
        assert!(
            render_decision("Ns.decide", "decide", &view(&input, &output, &cases[..3])).is_none()
        );
        let doubled: Vec<_> = cases.iter().chain(&cases[..1]).cloned().collect();
        assert!(render_decision("Ns.decide", "decide", &view(&input, &output, &doubled)).is_none());
    }

    #[test]
    fn generated_tla_modules_are_current() {
        let snapshot = crate::lean_contract_snapshot();
        let owners = named_owners(&tla_dir()).unwrap();
        let live = render_modules(&owners, |key| {
            let family = snapshot.families.get(key)?;
            Some(FamilyView {
                input: &family.input,
                output: &family.output,
                cases: family
                    .cases
                    .iter()
                    .map(|case| (&case.input, &case.expected))
                    .collect(),
            })
        })
        .unwrap();
        assert_eq!(
            live,
            checked_in_modules(&generated_dir()),
            "the generated TLA+ Lean decisions are stale; regenerate them with make lean-regen"
        );
    }
}
