//! Pack authoring verbs on the native `config` tool: `pack scaffold`,
//! `pack check` and `pack verify`, integrated into the existing `pack`
//! resource syntax through the shared `crate::pack::authoring` owners.
//!
//! The `pack` resource grant gates these verbs exactly as it gates install
//! (the caller selects the pack page's authoring commands); file authority
//! comes from the invoking agent's effective file tools, through the same
//! admission `skill import` and `discovery scan` use: the caller path is
//! resolved inside the effective tool root and never expands it. Scaffold
//! and check may write under that root, so they require readwrite file
//! tools; verify is observational and needs only read. `pack build` and
//! `pack test` are refused with the owner that holds them, never run here:
//! plugin compilation shells out to language toolchains through the
//! [`crate::pack::authoring::build::PluginCompiler`] seam, whose
//! implementation (`crates/gents-cli/src/commands/afterburner_build.rs`)
//! only the CLI owns, and author-test execution is the CLI's `gents pack
//! test`. `check` reports what it could not verify in `unverified` and
//! never reports full validation success from an empty problem list.

use std::path::PathBuf;

use super::*;
use crate::pack::authoring::{
    check_dir, scaffold, verify_pack_file, CheckReport, Kind, ScaffoldInput, Template,
};

/// One authoring call after argument parsing, before root and mode admission.
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
            "pack build is unavailable through config: compiling plugin sources shells out to language toolchains through the afterburner runner, whose implementation only the CLI owns. Run `gents pack build` from the operator CLI; the seam to supply is crate::pack::authoring::build::PluginCompiler (crates/gents-cli/src/commands/afterburner_build.rs). pack check names any plugin artifact that still needs it."
        ),
        "test" => bail!(
            "pack test is unavailable through config: author-test execution is the CLI's owner. Run `gents pack test` from the operator CLI; no canonical process admission for it exists on the config tool."
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

/// Effective file tools admit authoring: `write` calls need readwrite
/// (scaffold writes the template; check may rewrite a graph pack's README
/// topology), read calls need read. Off refuses both.
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
            "pack {operation} may write inside the tool root (scaffold writes the template files; check may rewrite a graph pack's README topology), so it requires readwrite file tools, not the current read-only mode"
        ),
        _ => Ok(()),
    }
}

/// The caller-named scaffold target, resolved inside the effective tool
/// root by the existing path admission owner.
fn admit_scaffold_dir(
    context: &crate::toolset::ToolContext,
    name: &str,
) -> Result<PathBuf> {
    context.resolve_path_allow_create(name)
}

/// Whether the check validated everything it owns, and the note that says
/// what an empty problem list does and does not mean.
fn check_outcome_fields(report: &CheckReport) -> (bool, String) {
    let validated = report.problems.is_empty() && report.unverified.is_empty();
    let note = if !report.problems.is_empty() {
        "Fix each named problem and check again."
    } else if !report.unverified.is_empty() {
        "unverified names the checks this call could not run (event-source filter queries against a node; experiment.json scenario validation). An empty problems list is not full validation; run `gents pack check` from the operator CLI for those, or fold their results in before reporting success."
    } else {
        "Every validation this layer owns ran, including the apply plan an install would use."
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
        crate::toolset::ToolContext::new(root.into(), false)
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
            "next": json!({"argv":["pack","check",relative]}),
            "note": "Scaffolded inside the effective tool root. The pack is not installed and nothing runs; check it, then build plugin artifacts with `gents pack build` from the operator CLI before installing.",
        }
        .pretty())
    }

    async fn check(&self, dir: Option<&str>) -> Result<String> {
        // Admission stays write-level even though the shared check writes
        // nothing itself: the topology rewrite (`pack graph --write-readme`
        // semantics) is one integration step away, and read-only callers
        // must not assume this verb never writes.
        let context = self.authoring_paths("check", true).await?;
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
        .pretty())
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
        Ok(ordered! {
            "verified": true,
            "header": serde_json::to_value(&header)?,
            "note": "Verified the archive in place; nothing was stored or installed. Install with [\"pack\",\"install\"].",
        }
        .pretty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn argv(words: &[&str]) -> Vec<String> {
        words.iter().map(|word| word.to_string()).collect()
    }

    #[test]
    fn scaffold_arguments_map_to_the_shared_input() {
        match parse_authoring_args("scaffold", &argv(&["review_toolkit"])).unwrap() {
            AuthoringCall::Scaffold { name, input } => {
                assert_eq!(name, "review_toolkit");
                assert_eq!(input.namespace, "gents");
                assert_eq!(input.template, None);
                assert_eq!(input.kind, None);
                assert_eq!(input.language, None);
            }
            _ => panic!("expected a scaffold call"),
        }
        match parse_authoring_args(
            "scaffold",
            &argv(&[
                "review_toolkit",
                "--namespace",
                "acme",
                "--kind",
                "graph",
                "--language",
                "rust",
            ]),
        )
        .unwrap()
        {
            AuthoringCall::Scaffold { name, input } => {
                assert_eq!(name, "review_toolkit");
                assert_eq!(input.namespace, "acme");
                assert_eq!(input.kind, Some(Kind::Graph));
                assert_eq!(input.template, None);
                assert_eq!(input.language.as_deref(), Some("rust"));
            }
            _ => panic!("expected a scaffold call"),
        }
        for bad in [
            vec![],
            vec!["a", "b"],
            vec!["a", "--bogus", "x"],
            vec!["a", "--kind", "widgets"],
            vec!["a", "--template", "widgets"],
            vec!["a", "--switch"],
        ] {
            assert!(
                parse_authoring_args("scaffold", &argv(&bad)).is_err(),
                "accepted {bad:?}"
            );
        }
    }

    #[test]
    fn check_and_verify_arguments_are_positional_only() {
        match parse_authoring_args("check", &argv(&[])).unwrap() {
            AuthoringCall::Check { dir } => assert_eq!(dir, None),
            _ => panic!("expected a check call"),
        }
        match parse_authoring_args("check", &argv(&["packs/review_toolkit"])).unwrap() {
            AuthoringCall::Check { dir } => {
                assert_eq!(dir.as_deref(), Some("packs/review_toolkit"))
            }
            _ => panic!("expected a check call"),
        }
        for bad in [vec!["a", "b"], vec!["--limit", "2"], vec!["--write-readme"]] {
            assert!(
                parse_authoring_args("check", &argv(&bad)).is_err(),
                "accepted {bad:?}"
            );
        }
        match parse_authoring_args("verify", &argv(&["built/review_toolkit.pack"])).unwrap() {
            AuthoringCall::Verify { pack } => assert_eq!(pack, "built/review_toolkit.pack"),
            _ => panic!("expected a verify call"),
        }
        for bad in [vec![], vec!["a", "b"], vec!["--store", "x"]] {
            assert!(
                parse_authoring_args("verify", &argv(&bad)).is_err(),
                "accepted {bad:?}"
            );
        }
    }

    #[test]
    fn build_and_test_name_the_existing_owner_instead_of_running_them() {
        for verb in ["build", "test"] {
            // err().expect, not unwrap_err: AuthoringCall has no Debug and
            // the test needs none.
            let error = parse_authoring_args(verb, &argv(&[]))
                .err()
                .expect("build and test refuse through config");
            let message = format!("{error:#}");
            assert!(message.contains("unavailable through config"), "{message}");
            assert!(message.contains("gents pack "), "{message}");
        }
        let error = parse_authoring_args("deploy", &argv(&[]))
            .err()
            .expect("unknown authoring verbs refuse");
        let message = format!("{error:#}");
        assert!(message.contains("scaffold, check and verify"), "{message}");
        assert_eq!(
            error.downcast_ref::<CommandGuidance>().unwrap().next_call,
            json!({"argv":["help","pack"]})
        );
    }

    #[test]
    fn the_caller_path_never_expands_authority() {
        let root = tempfile::tempdir().unwrap();
        let context =
            crate::toolset::ToolContext::new(root.path().to_path_buf(), false).unwrap();
        let inside = admit_scaffold_dir(&context, "review_toolkit").unwrap();
        assert!(inside.starts_with(root.path()));
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
            let error = ensure_mode(mode, true, "scaffold").err().expect("a write-refusing mode refuses");
            let message = format!("{error:#}");
            assert!(
                message.contains(expected),
                "{message} should name {expected}"
            );
        }
        let error = ensure_mode(FileToolMode::Off, false, "verify").unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("file permission"), "{message}");
        assert!(ensure_mode(FileToolMode::ReadOnly, false, "verify").is_ok());
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
