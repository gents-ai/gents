use gents_protocol::error_chain::ErrorChain;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::pack::PackManifest;
use crate::plugin::executor::{call_runner, PluginRunError};
use crate::plugin::{BoundDir, PluginBudget, PluginError, PluginRunner, PluginVerdict};

#[derive(Debug, thiserror::Error)]
pub enum PackTestError {
    #[error("reading manifest.json")]
    ReadManifest(#[source] std::io::Error),
    #[error("parsing manifest.json")]
    ParseManifest(#[source] serde_json::Error),
    #[error("reading the built {artifact}")]
    ReadArtifact {
        artifact: String,
        #[source]
        source: std::io::Error,
    },
    #[error("{artifact} is not a readable plugin")]
    NotReadable {
        artifact: String,
        #[source]
        source: afterburner_afb::AfbError,
    },
    #[error("a case is {{\"input\": ..., \"expect\": ..., \"bind\": ...}}")]
    CaseShape(#[source] serde_json::Error),
    #[error("the case file has no parent directory")]
    CaseWithoutParent,
    #[error("binding {relative:?} for the case at {}", path.display())]
    Bind {
        relative: String,
        path: PathBuf,
        #[source]
        source: crate::plugin::BoundDirError,
    },
    #[error("{verdict:?}: {diagnostics}")]
    Verdict {
        verdict: PluginVerdict,
        diagnostics: String,
    },
    #[error("returned {output} instead of {expect}")]
    Mismatch { output: Value, expect: Value },
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Plugin(#[from] PluginError),
    #[error(transparent)]
    Run(#[from] PluginRunError),
    #[error(transparent)]
    Authority(#[from] crate::plugin::authority::AuthorityError),
}

type Result<T, E = PackTestError> = std::result::Result<T, E>;

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
}

pub async fn run_plugin_cases(dir: &Path) -> Result<Vec<PluginCases>> {
    let manifest: PackManifest = serde_json::from_slice(
        &std::fs::read(dir.join("manifest.json")).map_err(PackTestError::ReadManifest)?,
    )
    .map_err(PackTestError::ParseManifest)?;
    let mut results = Vec::new();
    for plugin in &manifest.metadata.plugins {
        let Some(source) = &plugin.source else {
            continue;
        };
        let cases_dir = dir.join(source).join("tests");
        let mut cases: Vec<PathBuf> = match std::fs::read_dir(&cases_dir) {
            Ok(entries) => entries
                .filter_map(std::result::Result::ok)
                .map(|entry| entry.path())
                .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
                .collect(),
            Err(_) => Vec::new(),
        };
        cases.sort();
        let artifact = std::fs::read(dir.join(&plugin.artifact)).map_err(|source| {
            PackTestError::ReadArtifact {
                artifact: plugin.artifact.clone(),
                source,
            }
        })?;
        let runner = Arc::new(PluginRunner::compile_within(
            &artifact,
            plugin,
            &crate::plugin::authority::declared_manifold(plugin)?,
        )?);
        let coordinate = format!("{}/{}", manifest.metadata.namespace, plugin.name);
        let budget = PluginBudget::for_plugin(
            &afterburner_afb::Afb::from_bytes(&artifact).map_err(|source| {
                PackTestError::NotReadable {
                    artifact: plugin.artifact.clone(),
                    source,
                }
            })?,
            plugin,
        );
        let mut report = PluginCases {
            plugin: plugin.name.clone(),
            passed: 0,
            failures: Vec::new(),
        };
        for path in cases {
            let name = path
                .file_name()
                .map(|name| name.to_string_lossy().into_owned())
                .unwrap_or_default();
            match run_case(&coordinate, &runner, budget, &path).await {
                Ok(()) => report.passed += 1,
                Err(error) => report
                    .failures
                    .push(format!("{name}: {}", ErrorChain(&error))),
            }
        }
        results.push(report);
    }
    Ok(results)
}

async fn run_case(
    coordinate: &str,
    runner: &Arc<PluginRunner>,
    budget: PluginBudget,
    path: &Path,
) -> Result<()> {
    let case: PluginCase =
        serde_json::from_slice(&std::fs::read(path)?).map_err(PackTestError::CaseShape)?;
    let bound = match &case.bind {
        Some(relative) => {
            let case_dir = path.parent().ok_or(PackTestError::CaseWithoutParent)?;
            Some(
                BoundDir::new(&case_dir.join(relative), Some(case_dir)).map_err(|source| {
                    PackTestError::Bind {
                        relative: relative.clone(),
                        path: path.to_path_buf(),
                        source,
                    }
                })?,
            )
        }
        None => None,
    };
    let outcome = call_runner(coordinate, runner.clone(), case.input, budget, bound).await?;
    if outcome.verdict != PluginVerdict::Success {
        return Err(PackTestError::Verdict {
            verdict: outcome.verdict,
            diagnostics: outcome.diagnostics,
        });
    }
    if let Some(expect) = case.expect {
        if outcome.output != expect {
            return Err(PackTestError::Mismatch {
                output: outcome.output,
                expect,
            });
        }
    }
    Ok(())
}
