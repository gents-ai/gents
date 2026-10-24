use std::path::PathBuf;

use super::*;
use crate::pack::authoring::{
    check_dir, scaffold, verify_pack_file, CheckReport, Kind, ScaffoldInput, Template,
};

enum AuthoringCall {
    Scaffold { name: String, input: ScaffoldInput },
    Check { dir: Option<String> },
    Verify { pack: String },
}

fn parse_authoring_args(verb: &str, argv: &[String]) -> Result<AuthoringCall> {
    let parsed = ParsedArgs::parse(argv)?;
    match verb {
        "scaffold" => {
            anyhow::ensure!(
                parsed.switches.is_empty(),
                "pack scaffold takes no switches"
            );
            anyhow::ensure!(
                parsed.positionals.len() == 1,
                "pack scaffold requires exactly one snake_case NAME; the pack is created at <NAME> inside the effective tool root"
            );
            for option in parsed.options.keys() {
                anyhow::ensure!(
                    matches!(
                        option.as_str(),
                        "namespace" | "kind" | "template" | "language"
                    ),
                    "unknown pack scaffold option --{option}; accepted: --namespace, --kind, --template, --language"
                );
            }
            let kind = parsed
                .one("kind")?
                .map(|kind| match kind {
                    "documents" => Ok(Kind::Documents),
                    "graph" => Ok(Kind::Graph),
                    "assets" => Ok(Kind::Assets),
                    "plugins" => Ok(Kind::Plugins),
                    other => Err(anyhow::anyhow!(
                        "unknown pack kind {other:?}; accepted: documents, graph, assets, plugins"
                    )),
                })
                .transpose()?;
            let template = parsed
                .one("template")?
                .map(|slug| match slug {
                    "minimal" => Ok(Template::Minimal),
                    "automation" => Ok(Template::Automation),
                    "graph" => Ok(Template::Graph),
                    "plugin-tool" => Ok(Template::PluginTool),
                    "assets" => Ok(Template::Assets),
                    other => Err(anyhow::anyhow!(
                        "unknown pack template {other:?}; accepted: minimal, automation, graph, plugin-tool, assets"
                    )),
                })
                .transpose()?;
            Ok(AuthoringCall::Scaffold {
                name: parsed.positionals[0].clone(),
                input: ScaffoldInput {
                    namespace: parsed.one("namespace")?.unwrap_or("gents").to_owned(),
                    template,
                    kind,
                    language: parsed.one("language")?.map(ToOwned::to_owned),
                    ux: false,
                },
            })
        }
        "check" => {
            anyhow::ensure!(
                parsed.switches.is_empty() && parsed.options.is_empty(),
                "pack check takes only an optional DIR; it accepts no options"
            );
            anyhow::ensure!(
                parsed.positionals.len() <= 1,
                "pack check accepts at most one DIR"
            );
            Ok(AuthoringCall::Check {
                dir: parsed.positionals.first().cloned(),
            })
        }
        "verify" => {
            anyhow::ensure!(
                parsed.switches.is_empty() && parsed.options.is_empty(),
                "pack verify takes only one PACK path; it accepts no options"
            );
            anyhow::ensure!(
                parsed.positionals.len() == 1,
                "pack verify requires exactly one built .pack path"
            );
            Ok(AuthoringCall::Verify {
                pack: parsed.positionals[0].clone(),
            })
        }
        "build" => bail!(
            "pack build isn't available through config. Run `gents pack build` from the operator CLI to build plugin artifacts before installing."
        ),
        "test" => bail!(
            "pack test isn't available through config. Run `gents pack test` from the operator CLI."
        ),
        other => Err(CommandGuidance {
            message: format!(
                "unknown pack authoring command {other:?}; authoring verbs are scaffold, check and verify"
            ),
            next_call: json!({"argv":["help","pack"]}),
        }
        .into()),
    }
}

fn ensure_mode(
    mode: crate::tool_surface::FileToolMode,
    write: bool,
    operation: &str,
) -> Result<()> {
    use crate::tool_surface::FileToolMode;
    match (write, mode) {
        (_, FileToolMode::Off) => bail!(
            "pack {operation} requires effective file permission on the invoking agent; the effective file tools are Off. Grant file tools, or run pack authoring from the operator CLI"
        ),
        (true, FileToolMode::ReadOnly) => bail!(
            "pack {operation} may write inside the tool root (scaffold writes the template files), so it requires readwrite file tools, not the current read-only mode"
        ),
        _ => Ok(()),
    }
}

fn admit_scaffold_dir(context: &crate::toolset::ToolContext, name: &str) -> Result<PathBuf> {
    Ok(context.resolve_path_allow_create(name)?)
}

fn check_outcome_fields(report: &CheckReport) -> (bool, String) {
    let validated = report.problems.is_empty() && report.unverified.is_empty();
    let note = if !report.problems.is_empty() {
        "Fix each named problem before installing; this call made no change."
    } else if !report.unverified.is_empty() {
        "unverified lists checks this call could not run. An empty problems list is not full validation; run `gents pack check` before reporting success."
    } else {
        "Every validation available in this call passed."
    };
    (validated, note.to_owned())
}

impl ConfigCommandTool {
    pub(super) async fn authoring(&self, verb: &str, argv: &[String]) -> Result<String> {
        match parse_authoring_args(verb, argv)? {
            AuthoringCall::Scaffold { name, input } => self.scaffold(&name, &input).await,
            AuthoringCall::Check { dir } => self.check(dir.as_deref()).await,
            AuthoringCall::Verify { pack } => self.verify(&pack).await,
        }
    }

    async fn authoring_paths(
        &self,
        operation: &str,
        write: bool,
    ) -> Result<crate::toolset::ToolContext> {
        let effective = self
            .core
            .read_effective_config(&BTreeSet::new(), false, false)
            .await?;
        let mode = effective
            .pointer("/runtime_effective/effective/file_mode")
            .and_then(Value::as_str)
            .map(crate::tool_surface::FileToolMode::parse)
            .transpose()?
            .unwrap_or_default();
        ensure_mode(mode, write, operation)?;
        let root = effective
            .pointer("/runtime_effective/effective/root")
            .and_then(Value::as_str)
            .with_context(|| format!("pack {operation} requires an effective tool root"))?;
        Ok(crate::toolset::ToolContext::new(root.into(), false)?)
    }

    async fn scaffold(&self, name: &str, input: &ScaffoldInput) -> Result<String> {
        let context = self.authoring_paths("scaffold", true).await?;
        let dir = admit_scaffold_dir(&context, name)?;
        let report = scaffold(&dir, name, input)?;
        let relative = context.display_path(&report.dir);
        Ok(ordered! {
            "completed": true,
            "pack": report.pack,
            "namespace": report.namespace,
            "kind": report.kind,
            "template": report.template,
            "dir": relative,
            "files": report.files,
            "next_call": json!({"argv":["pack","check",relative]}),
            "note": "Scaffolded inside the effective tool root. The pack is not installed and nothing runs; check it, then build plugin artifacts with `gents pack build` from the operator CLI before installing.",
        }
        .pretty()?)
    }

    async fn check(&self, dir: Option<&str>) -> Result<String> {
        let context = self.authoring_paths("check", false).await?;
        let dir = context.resolve_existing_dir(dir)?;
        let report = check_dir(&dir).await;
        let (validated, note) = check_outcome_fields(&report);
        Ok(ordered! {
            "pack": report.pack,
            "dir": context.display_path(&report.dir),
            "digest": report.digest,
            "graphs": report.graphs,
            "problems": report.problems,
            "unverified": report.unverified,
            "validated": validated,
            "note": note,
        }
        .pretty()?)
    }

    async fn verify(&self, pack: &str) -> Result<String> {
        let context = self.authoring_paths("verify", false).await?;
        let path = context.resolve_existing_file(pack)?;
        anyhow::ensure!(
            path.extension()
                .is_some_and(|extension| extension == crate::pack_archive::EXTENSION),
            "pack verify accepts a built .pack artifact; {} is not one",
            context.display_path(&path)
        );
        let header = verify_pack_file(&path)?;
        let resolved = context.display_path(&path);
        Ok(ordered! {
            "verified": true,
            "header": serde_json::to_value(&header)?,
            "next_call": json!({"argv":["pack","preview","install",resolved]}),
            "note": "Verified the archive in place; nothing was stored or installed.",
        }
        .pretty()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_caller_path_never_expands_authority() {
        let root = tempfile::tempdir().unwrap();
        let context = crate::toolset::ToolContext::new(root.path().to_path_buf(), false).unwrap();
        let inside = admit_scaffold_dir(&context, "review_toolkit").unwrap();
        // ToolContext::new canonicalizes the root, so containment is judged
        // against the canonical root, not the caller alias (e.g. /var is a
        // symlink to /private/var on macOS).
        let canonical_root = std::fs::canonicalize(root.path()).unwrap();
        assert!(inside.starts_with(&canonical_root));
        for escape in ["../escape", "/etc/gents_escape"] {
            let error = admit_scaffold_dir(&context, escape).unwrap_err();
            let message = format!("{error:#}");
            assert!(
                message.contains("outside the allowed tool root"),
                "{message}"
            );
        }
    }

    #[test]
    fn write_authoring_requires_readwrite_and_verify_only_read() {
        use crate::tool_surface::FileToolMode;
        assert!(ensure_mode(FileToolMode::ReadWrite, true, "scaffold").is_ok());
        for (mode, expected) in [
            (FileToolMode::Off, "file permission"),
            (FileToolMode::ReadOnly, "readwrite"),
        ] {
            let error =
                ensure_mode(mode, true, "scaffold").expect_err("a write-refusing mode refuses");
            let message = format!("{error:#}");
            assert!(
                message.contains(expected),
                "{message} should name {expected}"
            );
        }
        for operation in ["check", "verify"] {
            let error = ensure_mode(FileToolMode::Off, false, operation).unwrap_err();
            let message = format!("{error:#}");
            assert!(message.contains("file permission"), "{message}");
            assert!(ensure_mode(FileToolMode::ReadOnly, false, operation).is_ok());
        }
    }

    #[test]
    fn an_unverified_check_never_reports_full_validation() {
        let report = |problems: Vec<String>, unverified: Vec<&'static str>| CheckReport {
            dir: PathBuf::from("review_toolkit"),
            pack: Some("review_toolkit".into()),
            digest: Some("sha256:x".into()),
            graphs: Vec::new(),
            problems,
            unverified,
        };
        let (validated, note) = check_outcome_fields(&report(Vec::new(), Vec::new()));
        assert!(validated);
        assert!(note.contains("Every validation"), "{note}");

        let (validated, note) =
            check_outcome_fields(&report(Vec::new(), vec!["experiment_scenario"]));
        assert!(!validated, "an empty problems list is not full validation");
        assert!(note.contains("unverified"), "{note}");
        assert!(note.contains("not full validation"), "{note}");

        let (validated, note) = check_outcome_fields(&report(
            vec!["declares x, which is missing".into()],
            Vec::new(),
        ));
        assert!(!validated);
        assert!(note.contains("Fix each named problem"), "{note}");
    }
}
