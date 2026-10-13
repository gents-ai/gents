//! Scaffold a pack from a template, the shared-library owner behind
//! `gents pack new` and `gents pack init`.
//!
//! The manifest is generated from the files the template writes, so its
//! asset list cannot drift from the directory. A plugin is scaffolded as
//! its own Afterburner package under `plugins/<name>/` and
//! `gents pack build` compiles it. Every template passes
//! `gents pack check` as written and builds unchanged.
//!
//! Canonical with `crates/gents-cli/src/commands/pack/scaffold.rs`; two
//! steps differ by owner and both are preserved:
//!
//! - A graph template's post-scaffold step runs this crate's own graph
//!   compilation and topology writer (`super::build::build_graph_plans`,
//!   `super::check::write_topology`) rather than the CLI's, over the same
//!   owners.
//! - The scaffold never compiles a plugin; the artifact stays missing until
//!   [`super::build::build_pack`] runs, exactly as the CLI leaves it.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::{json, Value};

use super::names::Names;
use super::template::{Kind, Template};

/// Everything that decides a scaffold, as the CLI's `PackScaffoldArgs`
/// carries it.
#[derive(Clone, Debug)]
pub struct ScaffoldInput {
    /// The registry namespace the pack publishes under.
    pub namespace: String,
    /// The template to write; absent, `kind` picks it.
    pub template: Option<Template>,
    /// The kind whose default template is used when `template` is absent.
    pub kind: Option<Kind>,
    /// Plugin language for the plugin-tool template; defaults to rust.
    pub language: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ScaffoldReport {
    pub pack: String,
    pub namespace: String,
    /// The manifest `"kind"` the template wrote.
    pub kind: &'static str,
    /// The template that was written, named the way the CLI prints it
    /// (kebab-case).
    pub template: &'static str,
    pub dir: PathBuf,
    pub files: Vec<String>,
}

impl Template {
    /// The CLI's kebab-case spelling, what a report prints.
    pub fn as_slug(self) -> &'static str {
        match self {
            Template::Minimal => "minimal",
            Template::Automation => "automation",
            Template::Graph => "graph",
            Template::PluginTool => "plugin-tool",
            Template::Assets => "assets",
        }
    }
}

/// Writes the template into `dir`, which must not already hold a pack.
///
/// The template is chosen from `input.template`, or from `input.kind` when
/// absent, exactly as the CLI's `scaffold` chooses it.
pub fn scaffold(dir: &Path, name: &str, input: &ScaffoldInput) -> Result<ScaffoldReport> {
    anyhow::ensure!(
        !dir.join("manifest.json").exists(),
        "{} already holds a pack",
        dir.display()
    );
    let names = Names::new(name, &input.namespace)?;
    let template = input
        .template
        .unwrap_or_else(|| Template::for_kind(input.kind));
    std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;

    let mut files: Vec<(String, String)> = vec![("README.md".into(), readme(&names, template))];
    let mut manifest = json!({
        "manifest_version": 1,
        "name": names.name,
        "version": "0.1.0",
        "description": format!("{} pack.", names.title),
        "authors": [format!("{} contributors", names.namespace)],
        "tags": [],
        "namespace": names.namespace,
        "dependencies": [],
    });
    match template {
        Template::Assets => manifest["kind"] = json!(template.manifest_kind()),
        Template::Minimal | Template::Automation | Template::Graph => {
            manifest["kind"] = json!(template.manifest_kind());
            manifest["config"] = json!("pack_config.json");
            manifest["inference_slots"] = json!([{
                "name": "worker",
                "description": "Does the pack's work. Any capable profile.",
                "agents": [format!("{}-worker", names.kebab)],
            }]);
            files.push((
                "agents/worker/system_prompt.md".into(),
                format!(
                    "You are the {} worker. Do the task you are given and nothing else.\n",
                    names.title
                ),
            ));
            files.push((
                "tasks/worker_task/prompt.md".into(),
                task_prompt(&names, template),
            ));
            let config = config(&names, template);
            if template != Template::Minimal {
                files.push((
                    format!("schemas/{}_job.graphql", names.name),
                    format!(
                        "type {}Job {{\n  run_id: String @index(unique: true) @immutable\n  request: String\n}}\n",
                        names.title
                    ),
                ));
                manifest["schemas"] = json!([format!("schemas/{}_job.graphql", names.name)]);
            }
            if template == Template::Graph {
                files.push((
                    format!("schemas/{}_result.graphql", names.name),
                    format!(
                        "type {}Result {{\n  run_id: String @index(unique: true) @immutable\n  summary: String\n}}\n",
                        names.title
                    ),
                ));
                manifest["schemas"]
                    .as_array_mut()
                    .context("schemas list")?
                    .push(json!(format!("schemas/{}_result.graphql", names.name)));
                manifest["compiler_version"] = json!(crate::graph_pipeline::COMPILER_VERSION);
            }
            files.push((
                "pack_config.json".into(),
                serde_json::to_string_pretty(&config)? + "\n",
            ));
        }
        Template::PluginTool => {
            manifest["kind"] = json!(template.manifest_kind());
            let language = scaffold_plugin(dir, &names, input.language.as_deref())?;
            let (instructions, markdown) = tool_markdown(&names.name);
            manifest["plugins"] = json!([{
                "name": names.name,
                "description": format!("{} tool.", names.title),
                "artifact": format!("plugins/{}.afb", names.name),
                "source": format!("plugins/{}", names.name),
                "language": language,
                "input_schema": {"type": "object"},
                "instructions": instructions,
            }]);
            files.push((instructions, markdown));
        }
    }

    for (path, contents) in &files {
        let target = dir.join(path);
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        std::fs::write(&target, contents)
            .with_context(|| format!("writing {}", target.display()))?;
    }
    let mut assets: Vec<String> = files.iter().map(|(path, _)| path.clone()).collect();
    if template == Template::PluginTool {
        assets.push(format!("plugins/{}.afb", names.name));
    }
    assets.sort();
    manifest["assets"] = json!(assets);
    std::fs::write(
        dir.join("manifest.json"),
        serde_json::to_string_pretty(&manifest)? + "\n",
    )
    .context("writing manifest.json")?;
    if template == Template::Graph {
        let manifest: crate::pack::PackManifest =
            serde_json::from_slice(&std::fs::read(dir.join("manifest.json"))?)?;
        super::build::build_graph_plans(dir, &manifest)?;
        super::check::write_topology(dir)?;
    }

    let mut written = Vec::new();
    list_files(dir, dir, &mut written)?;
    written.sort();
    Ok(ScaffoldReport {
        pack: names.name,
        namespace: names.namespace,
        kind: template.manifest_kind(),
        template: template.as_slug(),
        dir: dir.to_path_buf(),
        files: written,
    })
}

fn list_files(root: &Path, dir: &Path, out: &mut Vec<String>) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            list_files(root, &entry.path(), out)?;
        } else if let Ok(relative) = entry.path().strip_prefix(root) {
            out.push(relative.to_string_lossy().replace('\\', "/"));
        }
    }
    Ok(())
}

/// Where plugin `name`'s `TOOL.md` lives, and a starter for it: the markdown
/// a model reads to use the plugin as a tool.
pub fn tool_markdown(name: &str) -> (String, String) {
    (
        format!("plugins/{name}/TOOL.md"),
        format!(
            "# {name}\n\n\
             Use this tool to <what it does, and when a model should reach for it>.\n\n\
             ## Input\n\n\
             One JSON object matching the tool's input schema. <Describe each field.>\n\n\
             ## Output\n\n\
             One JSON value. <Describe what comes back, and what an error looks like.>\n"
        ),
    )
}

/// Writes the plugin's source for the plugin-tool template.
fn scaffold_plugin(dir: &Path, names: &Names, language: Option<&str>) -> Result<String> {
    write_plugin_source(
        &dir.join("plugins").join(&names.name),
        &names.name,
        language,
    )
}

/// Writes a plugin's source into `source`: a program that returns its JSON
/// input, in `language` (rust when absent), at the entry the build compiles.
/// Returns the language.
pub fn write_plugin_source(
    source: &Path,
    name: &str,
    language: Option<&str>,
) -> Result<String> {
    let language = language.unwrap_or("rust").trim().to_ascii_lowercase();
    let entry = super::build::plugin_entry(&language).with_context(|| {
        format!(
            "gents cannot build {language:?} plugins; use one of {}",
            crate::pack::SUPPORTED_PLUGIN_LANGUAGES.join(", ")
        )
    })?;
    anyhow::ensure!(!source.exists(), "{} already exists", source.display());
    let mut files = vec![
        (entry.to_owned(), echo_source(entry)),
        (
            "tests/echo.json".to_owned(),
            "{\"input\": {\"hello\": \"world\"}, \"expect\": {\"hello\": \"world\"}}\n".to_owned(),
        ),
    ];
    if entry == "source/main.rs" {
        files.push((
            "Cargo.toml".into(),
            format!(
                "[workspace]\n\n[package]\nname = \"{name}\"\nversion = \"0.1.0\"\nedition = \"2021\"\n\n\
                 [[bin]]\nname = \"{name}\"\npath = \"source/main.rs\"\n"
            ),
        ));
    }
    for (path, contents) in files {
        let target = source.join(path);
        std::fs::create_dir_all(target.parent().context("plugin source parent")?)?;
        std::fs::write(&target, contents)
            .with_context(|| format!("writing {}", target.display()))?;
    }
    Ok(language)
}

/// The template program for an entry file: read the JSON arguments from
/// standard input and write them back as the result.
fn echo_source(entry: &str) -> String {
    match entry {
        "source/main.rs" => "use std::io::{Read, Write};\n\n\
            fn main() -> std::io::Result<()> {\n    let mut input = Vec::new();\n    \
            std::io::stdin().read_to_end(&mut input)?;\n    std::io::stdout().write_all(&input)\n}\n",
        "source/main.go" => "package main\n\nimport (\n\t\"io\"\n\t\"os\"\n)\n\n\
            func main() {\n\tinput, err := io.ReadAll(os.Stdin)\n\tif err != nil {\n\t\tos.Exit(1)\n\t}\n\
            \tos.Stdout.Write(input)\n}\n",
        "source/main.c" => "#include <stdio.h>\n\nint main(void) {\n    char buffer[4096];\n    size_t n;\n    \
            while ((n = fread(buffer, 1, sizeof buffer, stdin)) > 0) {\n        \
            if (fwrite(buffer, 1, n, stdout) != n) return 1;\n    }\n    return ferror(stdin) ? 1 : 0;\n}\n",
        "source/main.cpp" => "#include <iostream>\n\nint main() {\n    std::cout << std::cin.rdbuf();\n    \
            return std::cout.good() ? 0 : 1;\n}\n",
        "source/main.js" | "source/main.ts" => "const chunk = new Uint8Array(4096);\nconst parts = [];\nfor (;;) {\n  \
            const n = Javy.IO.readSync(0, chunk);\n  if (n === 0) break;\n  parts.push(chunk.slice(0, n));\n}\n\
            for (const part of parts) Javy.IO.writeSync(1, part);\n",
        "source/main.py" => "import sys\n\nsys.stdout.write(sys.stdin.read())\n",
        "source/main.rb" => "$stdout.write($stdin.read)\n",
        _ => "",
    }
    .to_owned()
}

fn config(names: &Names, template: Template) -> Value {
    let kebab = &names.kebab;
    let agent = format!("{kebab}-worker");
    let mut config = json!({
        "node": {},
        "agents": [{
            "agent_id": agent,
            "display_name": format!("{} worker", names.title),
            "context_id": format!("{kebab}-worker-context"),
            "inference_profile_id": "gents:inference-slot:worker",
        }],
        "contexts": [{
            "context_id": format!("{kebab}-worker-context"),
            "display_name": format!("{} worker", names.title),
            "system_prompt": "./agents/worker/system_prompt.md",
            "tools_id": format!("{kebab}-worker-tools"),
        }],
        "tools": [{
            "tools_id": format!("{kebab}-worker-tools"),
            "display_name": format!("{} worker tools", names.title),
        }],
        "tasks": [{
            "task_id": format!("{kebab}-worker-task"),
            "display_name": format!("{} worker task", names.title),
            "agent_id": agent,
            "prompt_template": "./tasks/worker_task/prompt.md",
        }],
    });
    match template {
        Template::Automation => {
            config["event_sources"] = json!([{
                "event_source_id": format!("{kebab}-job"),
                "display_name": format!("{} job created", names.title),
                "source_collection": format!("{}Job", names.title),
                "event_kind": "created",
            }]);
            config["triggers"] = json!([{
                "trigger_id": format!("{kebab}-job"),
                "display_name": format!("Run the {} worker on a new job", names.title),
                "task_id": format!("{kebab}-worker-task"),
                "source": {"kind": "event", "event_source_id": format!("{kebab}-job")},
                "concurrency": "serial",
            }]);
        }
        Template::Graph => {
            let job = format!("{}Job", names.title);
            let result = format!("{}Result", names.title);
            config["tools"][0]["datastore"] =
                json!({"datastore_tool_surface_ids": [format!("{kebab}-worker-writes")]});
            config["datastore_tool_surfaces"] = json!([{
                "surface_id": format!("{kebab}-worker-writes"),
                "display_name": format!("{} result writes", names.title),
                "entries": [{
                    "tool_name": "write_result",
                    "collection": result,
                    "description": "Write the one result for this run; run_id is filled by the runtime.",
                    "output_obligation": {"scope": "trigger", "minimum_writes": 1},
                    "fields": [
                        {"name": "run_id", "required": false, "fill": "correlation"},
                        {"name": "summary", "required": true},
                    ],
                }],
            }]);
            config["graphs"] = json!([{ "graph_id": kebab }]);
            config["graph_capabilities"] = json!([{
                "capability_id": format!("{kebab}-worker"),
                "allowed_callers": ["${GENTS_PACK_NODE_DID}"],
                "revision": "v1",
                "target": {"kind": "task", "task_id": format!("{kebab}-worker-task")},
                "input_ports": [{
                    "name": "job", "collection": job, "schema": format!("{job}/v1"),
                    "correlation_field": "run_id", "cardinality": "one", "required": true,
                }],
                "output_ports": [{
                    "name": "result", "collection": result, "schema": format!("{result}/v1"),
                    "correlation_field": "run_id", "cardinality": "one",
                }],
            }]);
            config["graph_intents"] = json!([{
                "graph_id": kebab,
                "nodes": [{
                    "node_id": "worker",
                    "capability_id": format!("{kebab}-worker"),
                    "capability_revision": "v1",
                }],
                "entries": [{
                    "name": "start", "collection": job, "schema": format!("{job}/v1"),
                    "input_contract": format!("{kebab}-input/v1"),
                    "to": {"node_id": "worker", "port": "job"},
                }],
                "results": [{
                    "name": "result",
                    "from": {"node_id": "worker", "port": "result"},
                    "cardinality": {"kind": "exactly", "count": 1},
                    "terminal": true,
                }],
                "limits": {
                    "max_nodes": 4, "max_edges": 4, "max_depth": 4, "max_fan_out": 1,
                    "max_total_invocations": 4, "max_runtime_secs": 1800,
                },
            }]);
        }
        Template::Minimal | Template::PluginTool | Template::Assets => {}
    }
    config
}

fn task_prompt(names: &Names, template: Template) -> String {
    match template {
        Template::Graph => format!(
            "Read the {}Job for this run and write exactly one result with `write_result`.\n",
            names.title
        ),
        Template::Automation => format!(
            "A {}Job was created: {{{{ doc.request }}}}\n\nDo what it asks.\n",
            names.title
        ),
        _ => "Describe the task this pack's worker performs.\n".into(),
    }
}

fn readme(names: &Names, template: Template) -> String {
    let (what, io) = match template {
        Template::Assets => ("carries files and no configuration", "None."),
        Template::Minimal => ("installs one agent and one task", "The task's prompt."),
        Template::Automation => (
            "runs its worker whenever a job document is created",
            "Input: a `Job` document. Output: whatever the task writes.",
        ),
        Template::Graph => (
            "runs a one-stage graph from a job to one result",
            "Input: a `Job` document at the `start` entry. Output: one `Result`.",
        ),
        Template::PluginTool => (
            "ships one plugin tool",
            "The plugin's JSON input and output.",
        ),
    };
    format!(
        "# {title}\n\n{title} {what}.\n\n\
         ## Installation\n\n```sh\ngents pack install ./{name}\n```\n\n\
         ## Bindings and prerequisites\n\nBind the `worker` inference slot when installing, if the pack declares one.\n\n\
         ## Authority\n\nNo host files, commands or network access.\n\n\
         ## Inputs and outputs\n\n{io}\n\n\
         ## Completion and failure\n\nDescribe what finishing and failing look like.\n\n\
         ## Validation\n\n```sh\ngents pack check ./{name}\n```\n\n\
         ## Operational history\n\nNone yet.\n",
        title = names.title,
        name = names.name,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(template: Template) -> ScaffoldInput {
        ScaffoldInput {
            kind: None,
            namespace: "acme".into(),
            template: Some(template),
            language: None,
        }
    }

    /// The report names every file the template wrote, sorted, and the
    /// manifest's identity matches the scaffold input.
    #[test]
    fn the_report_names_every_written_file() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("review_toolkit");
        let report = scaffold(&dir, "review_toolkit", &input(Template::Minimal)).unwrap();
        assert_eq!(report.pack, "review_toolkit");
        assert_eq!(report.namespace, "acme");
        assert_eq!(report.kind, "documents");
        assert_eq!(report.template, "minimal");
        assert!(report.files.contains(&"manifest.json".to_owned()));
        assert!(report.files.contains(&"pack_config.json".to_owned()));
        assert!(report.files.contains(&"agents/worker/system_prompt.md".to_owned()));
        assert!(report.files.contains(&"tasks/worker_task/prompt.md".to_owned()));
        assert_eq!(report.files, {
            let mut sorted = report.files.clone();
            sorted.sort();
            sorted
        });
    }

    /// The generated manifest parses as the real `PackManifest` and passes
    /// the pack validator's own rules for every document template — the
    /// same property the CLI's `every_document_template_passes_the_check`
    /// test exercises end to end, bounded here to what this crate owns
    /// (the manifest rules; the config loader and embedded-node filter
    /// check stay with the CLI's integration).
    #[test]
    fn every_document_template_manifest_is_valid() {
        for template in [
            Template::Assets,
            Template::Minimal,
            Template::Automation,
            Template::Graph,
        ] {
            let root = tempfile::tempdir().unwrap();
            let dir = root.path().join("review_toolkit");
            let report = scaffold(&dir, "review_toolkit", &input(template)).unwrap();
            assert!(report.files.contains(&"manifest.json".to_owned()));
            let manifest: crate::pack::PackManifest = serde_json::from_slice(
                &std::fs::read(dir.join("manifest.json")).unwrap(),
            )
            .unwrap_or_else(|error| panic!("{template:?}: {error:#}"));
            crate::pack::validate_manifest("review_toolkit", &manifest)
                .unwrap_or_else(|error| panic!("{template:?}: {error:#}"));
            if template == Template::Graph {
                assert_eq!(manifest.compiler_version.as_deref(), Some(crate::graph_pipeline::COMPILER_VERSION));
                assert!(
                    dir.join("graphs/review_toolkit.plan.json").is_file(),
                    "a graph template ships its compiled plan"
                );
                assert!(
                    std::fs::read_to_string(dir.join("README.md"))
                        .unwrap()
                        .contains("pack-topology:start"),
                    "a graph template's README carries the topology diagram"
                );
            }
        }
    }

    /// A scaffolded plugin's manifest declares the artifact as an asset and
    /// the source's own case file; the artifact itself is only written by a
    /// build.
    #[test]
    fn the_plugin_template_declares_an_unbuilt_artifact() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("format_check");
        scaffold(&dir, "format_check", &input(Template::PluginTool)).unwrap();
        let manifest: crate::pack::PackManifest =
            serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).unwrap()).unwrap();
        crate::pack::validate_manifest("format_check", &manifest).unwrap();
        assert!(!dir.join("plugins/format_check.afb").exists());
        assert!(dir.join("plugins/format_check/source/main.rs").is_file());
        assert!(dir.join("plugins/format_check/tests/echo.json").is_file());
        assert!(dir.join("plugins/format_check/TOOL.md").is_file());
    }

    /// A language the build cannot compile is refused by name at scaffold
    /// time, against the pack validator's own supported list.
    #[test]
    fn an_unknown_plugin_language_is_refused_by_name() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("format_check");
        let error = scaffold(
            &dir,
            "format_check",
            &ScaffoldInput {
                language: Some("haskell".into()),
                ..input(Template::PluginTool)
            },
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("haskell"), "{error:#}");
    }

    #[test]
    fn a_directory_that_holds_a_pack_or_a_bad_name_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("review_toolkit");
        scaffold(&dir, "review_toolkit", &input(Template::Minimal)).unwrap();
        let again = scaffold(&dir, "review_toolkit", &input(Template::Minimal)).unwrap_err();
        assert!(
            format!("{again:#}").contains("already holds a pack"),
            "{again:#}"
        );

        let error = scaffold(
            &root.path().join("x"),
            "ReviewToolkit",
            &input(Template::Minimal),
        )
        .unwrap_err();
        assert!(
            format!("{error:#}").contains("\"review_toolkit\""),
            "{error:#}"
        );
    }

    #[test]
    fn tool_markdown_lives_at_the_plugin_convention() {
        let (path, markdown) = tool_markdown("format_check");
        assert_eq!(path, "plugins/format_check/TOOL.md");
        assert!(markdown.starts_with("# format_check\n"));
        assert!(markdown.contains("## Input"));
        assert!(markdown.contains("## Output"));
    }
}
