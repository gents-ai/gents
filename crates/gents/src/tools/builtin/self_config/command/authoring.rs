use std::path::PathBuf;

use super::*;
use crate::pack::authoring::{
    check_dir, export::collection_sdl, export_pack, pack_built, pack_name, plugins_with_source,
    read_manifest, scaffold, selectable_kind, source as source_pack, verify_pack_file, CheckReport,
    ExportSelection, Kind, PackAddition, PackEdit, ScaffoldInput, Template, SELECTABLE_KINDS,
};

mod plugin_source;

const PACK_PARTS: &str = "agent, task, trigger, schema, skill, plugin, page";

enum AuthoringCall {
    Scaffold {
        name: String,
        input: ScaffoldInput,
    },
    Check {
        dir: Option<String>,
    },
    Verify {
        pack: String,
    },
    Add {
        dir: Option<String>,
        addition: PackAddition,
    },
    Build {
        dir: Option<String>,
    },
    Test {
        dir: Option<String>,
        case: Option<String>,
    },
    Export {
        name: String,
        namespace: String,
        selection: ExportSelection,
    },
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
                        "namespace" | "kind" | "template" | "language" | "page"
                    ),
                    "unknown pack scaffold option --{option}; accepted: --namespace, --kind, --template, --language, --page"
                );
            }
            let page = match parsed.one("page")? {
                None | Some("false") => false,
                Some("true") => true,
                Some(other) => bail!("--page takes true or false, not {other:?}"),
            };
            let kind = parsed
                .one("kind")?
                .map(|kind| match kind {
                    "documents" => Ok(Kind::Documents),
                    "assets" => Ok(Kind::Assets),
                    "plugins" => Ok(Kind::Plugins),
                    other => Err(anyhow::anyhow!(
                        "unknown pack kind {other:?}; accepted: documents, assets, plugins"
                    )),
                })
                .transpose()?;
            let template = parsed
                .one("template")?
                .map(|slug| match slug {
                    "minimal" => Ok(Template::Minimal),
                    "automation" => Ok(Template::Automation),
                    "plugin-tool" => Ok(Template::PluginTool),
                    "assets" => Ok(Template::Assets),
                    "workflow" => Ok(Template::Workflow),
                    other => Err(anyhow::anyhow!(
                        "unknown pack template {other:?}; accepted: minimal, automation, plugin-tool, assets, workflow"
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
                    ux: page,
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
        "add" => {
            anyhow::ensure!(parsed.switches.is_empty(), "pack add takes no switches");
            let [part, id] = parsed.positionals.as_slice() else {
                bail!("pack add requires PART and ID; parts: {PACK_PARTS}");
            };
            let accepted: &[&str] = match part.as_str() {
                "agent" => &["slot"],
                "task" => &["agent"],
                "trigger" => &["task", "on"],
                "schema" | "page" => &[],
                "skill" => &["from"],
                "plugin" => &["language", "prebuilt"],
                other => bail!("unknown pack part {other:?}; parts: {PACK_PARTS}"),
            };
            for option in parsed.options.keys() {
                anyhow::ensure!(
                    option == "dir" || accepted.contains(&option.as_str()),
                    "pack add {part} does not take --{option}; it takes --dir{}",
                    accepted.iter().map(|name| format!(", --{name}")).collect::<String>()
                );
            }
            let id = id.clone();
            let optional = |name: &str| Ok::<_, anyhow::Error>(parsed.one(name)?.map(ToOwned::to_owned));
            let required = |name: &str| {
                optional(name)?.with_context(|| format!("pack add {part} requires options.{name}"))
            };
            let addition = match part.as_str() {
                "agent" => PackAddition::Agent { id, slot: optional("slot")? },
                "task" => PackAddition::Task { id, agent: required("agent")? },
                "trigger" => PackAddition::Trigger { id, task: required("task")?, on: required("on")? },
                "schema" => PackAddition::Schema { collection: id },
                "skill" => PackAddition::Skill { id, from: optional("from")?.map(PathBuf::from) },
                "plugin" => PackAddition::Plugin {
                    name: id,
                    language: optional("language")?,
                    prebuilt: optional("prebuilt")?.map(PathBuf::from),
                },
                _ => PackAddition::Page { name: id },
            };
            Ok(AuthoringCall::Add { dir: optional("dir")?, addition })
        }
        "build" | "test" => {
            let case = if verb == "test" {
                parsed.one("case")?.map(str::to_owned)
            } else {
                None
            };
            anyhow::ensure!(
                parsed.switches.is_empty()
                    && parsed
                        .options
                        .keys()
                        .all(|name| verb == "test" && name == "case"),
                "pack build takes only an optional DIR; pack test also takes options.case, one case file name"
            );
            anyhow::ensure!(
                parsed.positionals.len() <= 1,
                "pack {verb} accepts at most one DIR"
            );
            let dir = parsed.positionals.first().cloned();
            Ok(if verb == "build" {
                AuthoringCall::Build { dir }
            } else {
                AuthoringCall::Test { dir, case }
            })
        }
        "export" => {
            anyhow::ensure!(parsed.switches.is_empty(), "pack export takes no switches");
            let [name] = parsed.positionals.as_slice() else {
                bail!("pack export requires exactly one snake_case NAME; the pack is written to <NAME> inside the effective tool root");
            };
            let mut selection = ExportSelection::default();
            for (option, values) in &parsed.options {
                match (option.as_str(), selectable_kind(option)) {
                    ("namespace", _) => {}
                    ("tag", _) => selection.tags.extend(values.iter().cloned()),
                    ("collection", _) => selection.collections.extend(values.iter().cloned()),
                    (_, Some(kind)) => selection
                        .documents
                        .extend(values.iter().map(|id| (kind, id.clone()))),
                    _ => bail!(
                        "unknown pack export option --{option}; it takes --namespace, --tag, --collection and IDs per kind: {}",
                        SELECTABLE_KINDS
                            .iter()
                            .map(|(word, _)| format!("--{word}"))
                            .collect::<Vec<_>>()
                            .join(", ")
                    ),
                }
            }
            Ok(AuthoringCall::Export {
                name: name.clone(),
                namespace: parsed.one("namespace")?.unwrap_or("gents").to_owned(),
                selection,
            })
        }
        other => Err(CommandGuidance {
            message: format!(
                "unknown pack authoring command {other:?}; authoring verbs are scaffold, add, export, check, build, test and verify"
            ),
            next_call: json!({"argv":["help","pack"]}),
        }
        .into()),
    }
}

fn ensure_mode(
    mode: crate::tools::surface::FileToolMode,
    write: bool,
    operation: &str,
) -> Result<()> {
    use crate::tools::surface::FileToolMode;
    match (write, mode) {
        (_, FileToolMode::Off) => bail!(
            "pack {operation} requires effective file permission on the invoking agent; the effective file tools are Off. Grant file tools, or run pack authoring from the operator CLI"
        ),
        (true, FileToolMode::ReadOnly) => bail!(
            "pack {operation} writes inside the tool root, so it requires readwrite file tools, not the current read-only mode"
        ),
        _ => Ok(()),
    }
}

fn admit_scaffold_dir(context: &crate::tools::native::ToolContext, name: &str) -> Result<PathBuf> {
    context.resolve_path_allow_create(name)
}

fn check_outcome_fields(report: &CheckReport) -> (bool, String) {
    let validated = report.problems.is_empty() && report.unverified.is_empty();
    let note = if !report.problems.is_empty() {
        "Fix each named problem before installing; this call made no change."
    } else if !report.unverified.is_empty() {
        "unverified lists checks this call could not run. An empty problems list is not full validation; run `gents config pack check` before reporting success."
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
            AuthoringCall::Add { dir, addition } => self.add_part(dir.as_deref(), addition).await,
            AuthoringCall::Build { dir } => self.build(dir.as_deref()).await,
            AuthoringCall::Test { dir, case } => self.test(dir.as_deref(), case.as_deref()).await,
            AuthoringCall::Export {
                name,
                namespace,
                selection,
            } => self.export(&name, &namespace, &selection).await,
        }
    }

    pub(super) async fn authoring_paths(
        &self,
        operation: &str,
        write: bool,
    ) -> Result<crate::tools::native::ToolContext> {
        let effective = self
            .core
            .read_effective_config(&BTreeSet::new(), false, false)
            .await?;
        let mode = effective
            .pointer("/runtime_effective/effective/file_mode")
            .and_then(Value::as_str)
            .map(crate::tools::surface::FileToolMode::parse)
            .transpose()?
            .unwrap_or_default();
        ensure_mode(mode, write, operation)?;
        let root = effective
            .pointer("/runtime_effective/effective/root")
            .and_then(Value::as_str)
            .with_context(|| format!("pack {operation} requires an effective tool root"))?;
        crate::tools::native::ToolContext::new(root.into(), false)
    }

    async fn scaffold(&self, name: &str, input: &ScaffoldInput) -> Result<String> {
        let context = self.authoring_paths("scaffold", true).await?;
        let dir = admit_scaffold_dir(&context, &pack_name(name))?;
        let report = scaffold(&dir, name, input)?;
        let relative = context.display_path(&report.dir);
        let renamed = report.renamed_from.as_ref().map(|requested| {
            format!(
                "Pack names are snake_case: {requested:?} was created as {:?}.",
                report.pack
            )
        });
        ordered! {
            "completed": true,
            "pack": report.pack,
            "renamed": renamed,
            "namespace": report.namespace,
            "kind": report.kind,
            "template": report.template,
            "dir": relative,
            "files": report.files,
            "next_call": json!({"argv":["pack","check",relative]}),
            "note": "Scaffolded inside the effective tool root. The pack is not installed and nothing runs; check it, then pack build.",
        }
        .pretty()
    }

    /// Reads as the invoking agent, so the pack holds only what that agent
    /// can read; nothing on the node changes.
    async fn export(
        &self,
        name: &str,
        namespace: &str,
        selection: &ExportSelection,
    ) -> Result<String> {
        let context = self.authoring_paths("export", true).await?;
        let dir = admit_scaffold_dir(&context, &pack_name(name))?;
        let report = export_pack(
            &self.core.access()?,
            self.core.node_did(),
            &dir,
            name,
            namespace,
            selection,
        )
        .await?;
        let relative = context.display_path(&report.dir);
        let (validated, note) = check_outcome_fields(&report.check);
        ordered! {
            "completed": true,
            "pack": report.pack,
            "dir": relative,
            "selected": report.selected,
            "by_reference": report.by_reference,
            "agents": report.agents,
            "bind_agents": (!report.agents.is_empty()).then(|| json!({
                "var": report.agents.iter().map(|(slot, agent)| format!("{}={agent}", crate::pack::agent_binding_variable(slot))).collect::<Vec<_>>(),
                "note": "Installing this pack makes a new agent for each slot. To install it on this node onto the agents it was exported from, pass these as options.var to pack preview install and pack install.",
            })),
            "inference_slots": report.inference_slots,
            "schemas": report.schemas,
            "plugins": (!report.plugins.is_empty()).then_some(&report.plugins),
            "files": report.files,
            "problems": report.check.problems,
            "unverified": report.check.unverified,
            "validated": validated,
            "next_call": json!({"argv":["pack","build",relative]}),
            "note": format!("Exported from the live documents; the node is unchanged and the pack is not installed. {note}"),
        }
        .pretty()
    }

    async fn check(&self, dir: Option<&str>) -> Result<String> {
        let context = self.authoring_paths("check", false).await?;
        let dir = context.resolve_existing_dir(dir)?;
        let report = check_dir(&dir).await;
        let (validated, note) = check_outcome_fields(&report);
        let relative = context.display_path(&report.dir);
        ordered! {
            "pack": report.pack,
            "dir": relative,
            "digest": report.digest,
            "problems": report.problems,
            "unverified": report.unverified,
            "validated": validated,
            "next_call": (!report.unbuilt.is_empty()).then(|| json!({"argv":["pack","build",relative]})),
            "note": note,
        }
        .pretty()
    }

    async fn add_part(&self, dir: Option<&str>, mut addition: PackAddition) -> Result<String> {
        let context = self.authoring_paths("add", true).await?;
        let dir = context.resolve_existing_dir(dir)?;
        if let PackAddition::Skill {
            from: Some(path), ..
        }
        | PackAddition::Plugin {
            prebuilt: Some(path),
            ..
        } = &mut addition
        {
            *path = context.resolve_existing_file(path.to_str().context("path is not UTF-8")?)?;
        }
        let pack_dir = dir.clone();
        let added = tokio::task::spawn_blocking(move || {
            let mut pack = PackEdit::load(&pack_dir)?;
            let before = pack.manifest["assets"].clone();
            pack.add(addition)?;
            pack.save()?;
            let before = before.as_array().map(Vec::as_slice).unwrap_or_default();
            anyhow::Ok(
                pack.manifest["assets"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|asset| !before.contains(asset))
                    .cloned()
                    .collect::<Vec<_>>(),
            )
        })
        .await
        .context("pack add task did not finish")??;
        let relative = context.display_path(&dir);
        ordered! {
            "completed": true,
            "dir": relative,
            "added": added,
            "next_call": json!({"argv":["pack","check",relative]}),
            "note": "Edit the added files with the file tools, then check the pack. The manifest lists every asset.",
        }
        .pretty()
    }

    /// Compiling runs the pack's build scripts, so it takes the pack authority
    /// grant as well as the pack grant, even in the sandbox.
    fn ensure_compile_grant(&self, operation: &str, plugins: &[&str]) -> Result<()> {
        anyhow::ensure!(
            self.core.held_grants().pack_authority,
            "pack {operation} would compile plugin source ({}), which needs self_config.enable_pack_authority on this agent's Tools; the operator can grant it or run `gents config pack {operation}`",
            plugins.join(", ")
        );
        Ok(())
    }

    async fn build(&self, dir: Option<&str>) -> Result<String> {
        let context = self.authoring_paths("build", true).await?;
        let dir = context.resolve_existing_dir(dir)?;
        anyhow::ensure!(
            dir != context.root(),
            "pack build writes the .pack beside the pack directory, which would be outside the tool root; put the pack in a subdirectory"
        );
        let manifest = read_manifest(&dir)?;
        let sources = plugins_with_source(&manifest);
        let sandbox = if sources.is_empty() {
            None
        } else {
            self.ensure_compile_grant("build", &sources)?;
            self.execution.enter_mutation();
            Some(
                crate::tools::native::run_plugin_build(&context.root(), &dir, false)
                    .await?
                    .sandbox,
            )
        };
        let report = tokio::task::spawn_blocking(move || pack_built(&dir, None))
            .await
            .context("pack build task did not finish")??;
        let out = context.display_path(&report.out);
        ordered! {
            "completed": true,
            "pack": report.pack,
            "namespace": report.namespace,
            "version": report.version,
            "digest": report.digest,
            "out": out,
            "size_bytes": report.size_bytes,
            "sandbox": sandbox,
            "next_call": json!({"argv":["pack","preview","install",out]}),
        }
        .pretty()
    }

    async fn test(&self, dir: Option<&str>, case: Option<&str>) -> Result<String> {
        let context = self.authoring_paths("test", true).await?;
        let dir = context.resolve_existing_dir(dir)?;
        let manifest = read_manifest(&dir)?;
        let sources = plugins_with_source(&manifest);
        let relative = context.display_path(&dir);
        if sources.is_empty() {
            return ordered! {
                "passed": 0,
                "failed": 0,
                "plugins": Vec::<Value>::new(),
                "note": "This pack has no plugin source, so it has no plugin cases to run.",
                "next_call": json!({"argv":["pack","build",relative]}),
            }
            .pretty();
        }
        self.ensure_compile_grant("test", &sources)?;
        self.execution.enter_mutation();
        let build = crate::tools::native::run_plugin_build(&context.root(), &dir, true).await?;
        let mut plugins = build.output["cases"].clone();
        let mut passed = 0;
        let mut failed = 0;
        let mut shortened = Vec::new();
        for plugin in plugins.as_array_mut().into_iter().flatten() {
            passed += plugin["passed"].as_u64().unwrap_or(0);
            failed += plugin["failures"]
                .as_array()
                .map_or(0, |list| list.len() as u64);
            let name = plugin["plugin"].clone();
            if let Some(Value::Array(cases)) = plugin
                .as_object_mut()
                .and_then(|fields| fields.remove("shortened"))
            {
                shortened.extend(cases.into_iter().map(|case| (name.clone(), case)));
            }
        }
        if let Some(case) = case {
            let file = if case.ends_with(".json") {
                case.to_owned()
            } else {
                format!("{case}.json")
            };
            let rerun = json!({"argv":["pack","test",relative]});
            if let Some((plugin, whole)) = shortened
                .iter()
                .find(|(_, whole)| whole["case"] == file.as_str())
            {
                return ordered! {
                    "case": file,
                    "plugin": plugin,
                    "passed": false,
                    "returned": whole["returned"],
                    "expect": whole["expect"],
                    "sandbox": build.sandbox,
                    "next_call": rerun,
                }
                .pretty();
            }
            let prefix = format!("{file}: ");
            let failure = plugins.as_array().into_iter().flatten().find_map(|plugin| {
                let text = plugin["failures"]
                    .as_array()?
                    .iter()
                    .filter_map(Value::as_str)
                    .find(|text| text.starts_with(&prefix))?;
                Some((plugin["plugin"].clone(), text.to_owned()))
            });
            if let Some((plugin, failure)) = failure {
                return ordered! {
                    "case": file,
                    "plugin": plugin,
                    "passed": false,
                    "failure": failure,
                    "sandbox": build.sandbox,
                    "next_call": rerun,
                }
                .pretty();
            }
            let exists = manifest
                .metadata
                .plugins
                .iter()
                .filter_map(|plugin| plugin.source.as_ref())
                .any(|source| dir.join(source).join("tests").join(&file).is_file());
            if !exists {
                return Err(CommandGuidance {
                    message: format!("no plugin case {file:?} in this pack; cases are the tests/*.json files beside each plugin's source"),
                    next_call: rerun,
                }
                .into());
            }
            return ordered! {
                "case": file,
                "passed": true,
                "sandbox": build.sandbox,
                "next_call": rerun,
            }
            .pretty();
        }
        let next_call = match shortened.first() {
            Some((_, whole)) => {
                json!({"argv":["pack","test",relative],"options":{"case":whole["case"]}})
            }
            None if failed == 0 => json!({"argv":["pack","build",relative]}),
            None => json!({"argv":["help","pack"]}),
        };
        ordered! {
            "passed": passed,
            "failed": failed,
            "plugins": plugins,
            "note": (!shortened.is_empty()).then_some("A long mismatch names the paths that differ; options.case with the case file name returns that case's whole returned output and expect."),
            "sandbox": build.sandbox,
            "next_call": next_call,
        }
        .pretty()
    }

    async fn verify(&self, pack: &str) -> Result<String> {
        let context = self.authoring_paths("verify", false).await?;
        let path = context.resolve_existing_file(pack)?;
        anyhow::ensure!(
            path.extension()
                .is_some_and(|extension| extension == crate::pack::archive::EXTENSION),
            "pack verify accepts a built .pack artifact; {} is not one",
            context.display_path(&path)
        );
        let header = verify_pack_file(&path)?;
        let resolved = context.display_path(&path);
        ordered! {
            "verified": true,
            "header": serde_json::to_value(&header)?,
            "next_call": json!({"argv":["pack","preview","install",resolved]}),
            "note": "Verified the archive in place; nothing was stored or installed.",
        }
        .pretty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scaffold_accepts_the_workflow_template_and_a_language() {
        let argv: Vec<String> = ["rome_debate", "--template", "workflow", "--language", "js"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        let AuthoringCall::Scaffold { name, input } =
            parse_authoring_args("scaffold", &argv).unwrap()
        else {
            panic!("scaffold parses to a scaffold call");
        };
        assert_eq!(name, "rome_debate");
        assert_eq!(input.template, Some(Template::Workflow));
        assert_eq!(input.language.as_deref(), Some("js"));
        let bad: Vec<String> = ["x", "--template", "nope"]
            .into_iter()
            .map(str::to_owned)
            .collect();
        let error = match parse_authoring_args("scaffold", &bad) {
            Err(error) => error.to_string(),
            Ok(_) => panic!("an unknown template is refused"),
        };
        assert!(
            error.contains("workflow"),
            "the refusal lists every template: {error}"
        );
    }

    #[test]
    fn add_parses_each_part_and_names_what_it_accepts() {
        let argv = |words: &[&str]| {
            words
                .iter()
                .map(|word| (*word).to_owned())
                .collect::<Vec<_>>()
        };
        let AuthoringCall::Add { dir, addition } = parse_authoring_args(
            "add",
            &argv(&["task", "review", "--agent", "worker", "--dir", "p"]),
        )
        .unwrap_or_else(|error| panic!("{error:#}")) else {
            panic!("add parses to an add call");
        };
        assert_eq!(dir.as_deref(), Some("p"));
        assert!(matches!(addition, PackAddition::Task { .. }));
        for (words, expected) in [
            (vec!["task", "review"], "requires options.agent"),
            (vec!["page", "board", "--slot", "x"], "does not take --slot"),
            (vec!["gizmo", "board"], "agent, task, trigger"),
        ] {
            let error = match parse_authoring_args("add", &argv(&words)) {
                Err(error) => format!("{error:#}"),
                Ok(_) => panic!("{words:?} is refused"),
            };
            assert!(error.contains(expected), "{words:?}: {error}");
        }
    }

    #[test]
    fn the_caller_path_never_expands_authority() {
        let root = tempfile::tempdir().unwrap();
        let context =
            crate::tools::native::ToolContext::new(root.path().to_path_buf(), false).unwrap();
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
        use crate::tools::surface::FileToolMode;
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
            problems,
            unverified,
            unbuilt: Vec::new(),
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
