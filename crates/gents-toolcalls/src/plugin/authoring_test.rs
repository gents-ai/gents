use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::plugin::executor::call_runner;
use gents_pack::pack::PackManifest;
use gents_pack::plugin::{BoundDir, PluginBudget, PluginRunner, PluginVerdict};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PluginCase {
    pub input: Value,
    #[serde(default)]
    pub expect: Option<Value>,
    #[serde(default)]
    pub bind: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct PluginCases {
    pub plugin: String,
    pub passed: usize,
    pub failures: Vec<String>,
    /// The whole documents of every failure whose text names only the paths
    /// that differ, so a caller can still read one case in full.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub shortened: Vec<ShortenedCase>,
}

#[derive(Debug, Serialize)]
pub struct ShortenedCase {
    pub case: String,
    pub returned: Value,
    pub expect: Value,
}

pub async fn run_plugin_cases(dir: &Path) -> Result<Vec<PluginCases>> {
    let manifest: PackManifest = serde_json::from_slice(
        &std::fs::read(dir.join("manifest.json")).context("reading manifest.json")?,
    )
    .context("parsing manifest.json")?;
    let mut results = Vec::new();
    for plugin in &manifest.metadata.plugins {
        let Some(source) = &plugin.source else {
            continue;
        };
        let cases_dir = dir.join(source).join("tests");
        let mut cases: Vec<PathBuf> = match std::fs::read_dir(&cases_dir) {
            Ok(entries) => entries
                .filter_map(Result::ok)
                .map(|entry| entry.path())
                .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
                .collect(),
            Err(_) => Vec::new(),
        };
        cases.sort();
        let artifact = std::fs::read(dir.join(&plugin.artifact))
            .with_context(|| format!("reading the built {}", plugin.artifact))?;
        let runner = Arc::new(PluginRunner::compile_within(
            &artifact,
            plugin,
            &gents_pack::plugin::authority::declared_manifold(plugin)?,
        )?);
        let coordinate = format!("{}/{}", manifest.metadata.namespace, plugin.name);
        let budget = PluginBudget::for_plugin(
            &afterburner_afb::Afb::from_bytes(&artifact)
                .with_context(|| format!("{} is not a readable plugin", plugin.artifact))?,
            plugin,
        );
        let mut report = PluginCases {
            plugin: plugin.name.clone(),
            passed: 0,
            failures: Vec::new(),
            shortened: Vec::new(),
        };
        for path in cases {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            match run_case(&coordinate, &runner, budget, &path).await {
                Ok(None) => report.passed += 1,
                Ok(Some((returned, expect))) => {
                    let (text, shortened) = describe_mismatch(&returned, &expect);
                    report.failures.push(format!("{name}: {text}"));
                    if shortened {
                        report.shortened.push(ShortenedCase {
                            case: name,
                            returned,
                            expect,
                        });
                    }
                }
                Err(error) => report.failures.push(format!("{name}: {error:#}")),
            }
        }
        results.push(report);
    }
    Ok(results)
}

/// A case's returned output and its expectation when they differ.
type Mismatch = (Value, Value);

/// A mismatch is reported whole while it is small; a larger one names the
/// paths that differ, each side cut to `MISMATCH_VALUE_CHARS`, because the
/// author holds `expect` already and two full documents cost their size again
/// in every failing run.
const MISMATCH_WHOLE_CHARS: usize = 2_000;
const MISMATCH_VALUE_CHARS: usize = 400;
const MISMATCH_PATHS: usize = 12;

/// The failure text, and whether it stands in for the whole documents.
fn describe_mismatch(actual: &Value, expect: &Value) -> (String, bool) {
    let whole = format!("returned {actual} instead of {expect}");
    if whole.len() <= MISMATCH_WHOLE_CHARS {
        return (whole, false);
    }
    let mut differences = Vec::new();
    collect_differences("$", Some(actual), Some(expect), &mut differences);
    let shown = differences
        .iter()
        .take(MISMATCH_PATHS)
        .map(|(path, actual, expect)| {
            format!(
                "{path}: returned {} instead of {}",
                clip_value(*actual),
                clip_value(*expect)
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    let more = differences.len().saturating_sub(MISMATCH_PATHS);
    let more = if more == 0 {
        String::new()
    } else {
        format!("; and {more} more paths")
    };
    (
        format!(
            "output differs from expect at {} path(s): {shown}{more}",
            differences.len()
        ),
        true,
    )
}

fn clip_value(value: Option<&Value>) -> String {
    let Some(value) = value else {
        return "nothing".to_owned();
    };
    let text = value.to_string();
    let total = text.chars().count();
    if total <= MISMATCH_VALUE_CHARS {
        return text;
    }
    let head: String = text.chars().take(MISMATCH_VALUE_CHARS).collect();
    format!("{head}… ({total} chars)")
}

fn collect_differences<'a>(
    path: &str,
    actual: Option<&'a Value>,
    expect: Option<&'a Value>,
    out: &mut Vec<(String, Option<&'a Value>, Option<&'a Value>)>,
) {
    match (actual, expect) {
        (Some(Value::Object(actual)), Some(Value::Object(expect))) => {
            let keys = actual
                .keys()
                .chain(expect.keys().filter(|key| !actual.contains_key(*key)));
            for key in keys {
                collect_differences(
                    &format!("{path}.{key}"),
                    actual.get(key),
                    expect.get(key),
                    out,
                );
            }
        }
        (Some(Value::Array(actual)), Some(Value::Array(expect)))
            if actual.len() == expect.len() =>
        {
            for (index, (actual, expect)) in actual.iter().zip(expect).enumerate() {
                collect_differences(&format!("{path}[{index}]"), Some(actual), Some(expect), out);
            }
        }
        (actual, expect) if actual != expect => out.push((path.to_owned(), actual, expect)),
        _ => {}
    }
}

async fn run_case(
    coordinate: &str,
    runner: &Arc<PluginRunner>,
    budget: PluginBudget,
    path: &Path,
) -> Result<Option<Mismatch>> {
    let case: PluginCase = serde_json::from_slice(&std::fs::read(path)?)
        .context("a case is {\"input\": ..., \"expect\": ..., \"bind\": ...}")?;
    let bound = match &case.bind {
        Some(relative) => {
            let case_dir = path
                .parent()
                .context("the case file has no parent directory")?;
            Some(
                BoundDir::new(&case_dir.join(relative), Some(case_dir)).with_context(|| {
                    format!("binding {relative:?} for the case at {}", path.display())
                })?,
            )
        }
        None => None,
    };
    let outcome = call_runner(coordinate, runner.clone(), case.input, budget, bound).await?;
    anyhow::ensure!(
        outcome.verdict == PluginVerdict::Success,
        "{:?}: {}",
        outcome.verdict,
        outcome.diagnostics
    );
    Ok(case
        .expect
        .filter(|expect| outcome.output != *expect)
        .map(|expect| (outcome.output, expect)))
}

#[cfg(test)]
mod mismatch_tests {
    use super::describe_mismatch;
    use serde_json::json;

    #[test]
    fn a_small_mismatch_is_reported_whole() {
        assert_eq!(
            describe_mismatch(&json!({"ok":false}), &json!({"ok":true})),
            (
                r#"returned {"ok":false} instead of {"ok":true}"#.to_owned(),
                false
            )
        );
    }

    /// A failing case costs a bounded amount however large its documents are.
    #[test]
    fn a_large_mismatch_names_the_paths_that_differ_within_a_ceiling() {
        let big = "isle ".repeat(2_000);
        let expect =
            json!({"ok":true,"deal":{"isles":&big,"order":[1,2,3]},"rows":[{"id":1},{"id":2}]});
        let actual = json!({"ok":true,"deal":{"isles":&big,"order":[1,2,4]},"rows":[{"id":1},{"id":3}],"writes":[&big]});
        let (text, shortened) = describe_mismatch(&actual, &expect);
        assert!(
            shortened,
            "the caller keeps the whole documents for one read"
        );
        assert!(
            text.starts_with("output differs from expect at 3 path(s): "),
            "{text}"
        );
        for part in [
            "$.deal.order[2]: returned 4 instead of 3",
            "$.rows[1].id: returned 3 instead of 2",
            "$.writes: returned [\"isle isle",
            "chars) instead of nothing",
        ] {
            assert!(text.contains(part), "{part}: {text}");
        }
        assert!(text.len() <= 1_500, "{} chars", text.len());
    }
}
