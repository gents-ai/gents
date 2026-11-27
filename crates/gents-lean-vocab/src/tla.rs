//! TLA+ function tables of the Lean decisions that TLA+ specifications name.
//!
//! A spec names its Lean owners on its second line (`\* lean: A.b, C.d`). Each
//! must be exported as a conformance function over a finite domain (every
//! argument a `Bool` or a field-less enum, every result a `Bool`, `Nat` or
//! enum, or an `Option` of one whose `none` is the string `"none"`); it
//! becomes `<name>_table`, the decision over its whole domain keyed
//! by the tuple of its arguments in Lean parameter order, and `<name>(...)`,
//! its lookup, in `generated/LeanDecision_<Namespace>.tla`. Lean a spec
//! restates without a table goes on its `\* models:` line, which this
//! generator does not read.

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
    PathBuf::from(std::env::var_os("CARGO_MANIFEST_DIR").expect("runtime CARGO_MANIFEST_DIR"))
        .join("../gents/proofs/tla")
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

/// `rows` joined by `@@` as a balanced tree, one row per line. TLC's parser
/// and evaluator recurse once per operand of a left-nested `@@` chain, so a
/// chain of a few hundred rows overflows a default Java thread stack; the
/// tree's depth is logarithmic in its rows.
fn balanced(rows: &[String]) -> Vec<String> {
    if rows.len() <= 1 {
        return rows.to_vec();
    }
    let (left, right) = rows.split_at(rows.len().div_ceil(2));
    let mut lines: Vec<String> = balanced(left)
        .into_iter()
        .enumerate()
        .map(|(index, line)| {
            if index == 0 {
                format!("({line}")
            } else {
                format!(" {line}")
            }
        })
        .collect();
    lines.extend(
        balanced(right)
            .into_iter()
            .enumerate()
            .map(|(index, line)| {
                if index == 0 {
                    format!(" @@ {line}")
                } else {
                    format!("    {line}")
                }
            }),
    );
    if let Some(last) = lines.last_mut() {
        last.push(')');
    }
    lines
}

/// The decision's table and lookup, or `None` when it is not a function over
/// a finite domain that TLA+ values can carry.
fn render_decision(owner: &str, name: &str, family: &FamilyView<'_>) -> Option<String> {
    let optional = family.output.get("option");
    let result = optional.unwrap_or(family.output);
    if !(result == "bool" || result == "nat" || result.get("enum").is_some()) {
        return None;
    }
    let none = Value::from("none");
    if optional.is_some()
        && result
            .get("values")
            .and_then(Value::as_array)
            .is_some_and(|values| values.contains(&none))
    {
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
        let expected = match (optional, expected) {
            (Some(_), Value::Null) => &none,
            _ => *expected,
        };
        rows.push((key, literal(expected)?));
    }
    if parameters.is_empty() || rows.len() != size {
        return None;
    }
    let rows: Vec<String> = rows
        .iter()
        .map(|(key, result)| format!("<<{key}>> :> {result}"))
        .collect();
    let mut text = format!("\\* Lean `{owner}`.\n{name}_table ==\n");
    for line in balanced(&rows) {
        text.push_str(&format!("  {line}\n"));
    }
    let arguments = parameters.join(", ");
    text.push_str(&format!(
        "\n{name}({arguments}) ==\n  {name}_table[<<{arguments}>>]\n"
    ));
    Some(text)
}

/// Module file name -> text for the named owners; an owner that is not a
/// finite function TLA+ values can carry is an error, so a spec never loses
/// the table it extends.
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
        let text = (!namespace.is_empty() && is_identifier(name))
            .then(|| render_decision(owner, name, &view))
            .flatten()
            .ok_or_else(|| {
                format!(
                    "Lean owner {owner} is not a namespaced function over a finite domain, so it has no TLA+ table; name it on the spec's `\\* models:` line instead"
                )
            })?;
        modules
            .entry(format!("{MODULE_PREFIX}{}", namespace.join("_")))
            .or_default()
            .push(text);
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
            "\\* Lean `Ns.decide`.\ndecide_table ==\n  ((<<\"a\", FALSE>> :> \"no\"\n    @@ <<\"a\", TRUE>> :> \"yes\")\n   @@ (<<\"b\", FALSE>> :> \"no\"\n       @@ <<\"b\", TRUE>> :> \"no\"))\n\ndecide(mode, flag) ==\n  decide_table[<<mode, flag>>]\n"
        );
        assert!(
            render_decision("Ns.decide", "decide", &view(&input, &output, &cases[..3])).is_none()
        );
        let doubled: Vec<_> = cases.iter().chain(&cases[..1]).cloned().collect();
        assert!(render_decision("Ns.decide", "decide", &view(&input, &output, &doubled)).is_none());
    }

    #[test]
    fn an_optional_result_renders_none_as_a_value_unless_the_enum_has_one() {
        let input = serde_json::json!({"struct": "Input", "fields": [["flag", "bool"]]});
        let output = serde_json::json!({"option": {"enum": "Out", "values": ["yes", "no"]}});
        let cases = [
            (serde_json::json!({"flag": false}), Value::Null),
            (serde_json::json!({"flag": true}), serde_json::json!("yes")),
        ];
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
        assert_eq!(
            render_decision("Ns.decide", "decide", &view(&input, &output, &cases)).unwrap(),
            "\\* Lean `Ns.decide`.\ndecide_table ==\n  (<<FALSE>> :> \"none\"\n   @@ <<TRUE>> :> \"yes\")\n\ndecide(flag) ==\n  decide_table[<<flag>>]\n"
        );
        let ambiguous = serde_json::json!({"option": {"enum": "Out", "values": ["yes", "none"]}});
        assert!(
            render_decision("Ns.decide", "decide", &view(&input, &ambiguous, &cases)).is_none()
        );
    }

    /// The most `@@` operators any one parenthesized group joins, and the
    /// deepest parenthesis nesting, of a rendered table.
    fn chain_and_depth(text: &str) -> (usize, usize) {
        let mut groups = vec![0usize];
        let (mut longest, mut deepest) = (0, 0);
        for (index, c) in text.char_indices() {
            match c {
                '(' => {
                    groups.push(0);
                    deepest = deepest.max(groups.len() - 1);
                }
                ')' => {
                    groups.pop();
                }
                '@' if text[index..].starts_with("@@") && !text[..index].ends_with('@') => {
                    let group = groups.last_mut().expect("the top level is a group");
                    *group += 1;
                    longest = longest.max(*group);
                }
                _ => {}
            }
        }
        (longest, deepest)
    }

    #[test]
    fn a_large_table_nests_logarithmically_not_as_one_chain() {
        let values = |prefix: &str, count: usize| -> Value {
            serde_json::json!((0..count)
                .map(|i| format!("{prefix}{i}"))
                .collect::<Vec<_>>())
        };
        let input = serde_json::json!({"struct": "Input", "fields": [
            ["a", {"enum": "A", "values": values("a", 8)}],
            ["b", {"enum": "B", "values": values("b", 8)}],
            ["c", {"enum": "C", "values": values("c", 9)}]]});
        let output = Value::from("bool");
        let mut cases = Vec::new();
        for a in 0..8 {
            for b in 0..8 {
                for c in 0..9 {
                    cases.push((
                        serde_json::json!({"a": format!("a{a}"), "b": format!("b{b}"), "c": format!("c{c}")}),
                        Value::Bool((a + b + c) % 2 == 0),
                    ));
                }
            }
        }
        let view = FamilyView {
            input: &input,
            output: &output,
            cases: cases.iter().map(|(i, e)| (i, e)).collect(),
        };
        let text = render_decision("Ns.decide", "decide", &view).unwrap();
        assert_eq!(text.matches(" :> ").count(), 576);
        let (longest, deepest) = chain_and_depth(&text);
        assert_eq!(longest, 1, "a group joins more than two operands");
        assert!(deepest <= 11, "the table nests {deepest} deep");
    }

    #[test]
    fn a_named_owner_without_a_finite_table_is_an_error() {
        let input = serde_json::json!({"struct": "Input", "fields": [["count", "nat"]]});
        let output = Value::from("bool");
        let case = (serde_json::json!({"count": 0}), Value::Bool(true));
        let owners = BTreeSet::from(["Ns.decide".to_owned()]);
        let error = render_modules(&owners, |_| {
            Some(FamilyView {
                input: &input,
                output: &output,
                cases: vec![(&case.0, &case.1)],
            })
        })
        .unwrap_err();
        assert!(error.contains("Ns.decide"), "{error}");
        assert!(error.contains("models:"), "{error}");
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
