//! `gents pack new` and `gents pack init`: scaffold a pack from a template
//! embedded in the binary.
//!
//! The manifest is generated from the files the template writes, so its
//! asset list cannot drift from the directory. A plugin is scaffolded as its
//! own Afterburner package under `plugins/<name>/` through Afterburner's
//! library, and `gents pack build` compiles it. Every template passes
//! `gents pack check` as written and builds unchanged.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;
use serde_json::{json, Value};

use crate::cli::{PackKindArg, PackNewArgs, PackScaffoldArgs, PackTemplate};

#[derive(Debug, Serialize)]
pub(crate) struct ScaffoldReport {
    pub(crate) pack: String,
    pub(crate) namespace: String,
    pub(crate) template: PackTemplate,
    pub(crate) dir: PathBuf,
    pub(crate) files: Vec<String>,
}

/// The names a template is filled with, all derived from the pack name.
struct Names {
    name: String,
    namespace: String,
    /// `review_toolkit` -> `review-toolkit`, for document ids.
    kebab: String,
    /// `review_toolkit` -> `ReviewToolkit`, for collection names.
    title: String,
}

impl Names {
    fn new(name: &str, namespace: &str) -> Result<Self> {
        let snake = to_snake_case(name);
        anyhow::ensure!(
            snake == name && !name.is_empty(),
            "pack names are snake_case; use {snake:?}"
        );
        Ok(Self {
            name: name.to_owned(),
            namespace: namespace.to_owned(),
            kebab: name.replace('_', "-"),
            title: name
                .split('_')
                .map(|part| {
                    let mut chars = part.chars();
                    chars
                        .next()
                        .map(|first| first.to_ascii_uppercase().to_string() + chars.as_str())
                        .unwrap_or_default()
                })
                .collect(),
        })
    }
}

fn to_snake_case(name: &str) -> String {
    let mut out = String::new();
    for (index, ch) in name.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if index > 0 && !out.ends_with('_') {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else if ch.is_ascii_alphanumeric() {
            out.push(ch);
        } else if !out.ends_with('_') && !out.is_empty() {
            out.push('_');
        }
    }
    out.trim_end_matches('_').to_owned()
}

/// `gents pack new <name>`: scaffolds into a new `./<name>/`.
pub(crate) fn new(args: PackNewArgs) -> Result<()> {
    let dir = PathBuf::from(&args.name);
    if dir.exists() {
        anyhow::ensure!(
            std::fs::read_dir(&dir)?.next().is_none(),
            "{} already exists and is not empty",
            dir.display()
        );
    }
    let report = scaffold(&dir, &args.name, &args.options)?;
    crate::print_json(&serde_json::to_value(report)?)
}

/// `gents pack init`: scaffolds into the current directory, named after it.
pub(crate) fn init(args: PackScaffoldArgs) -> Result<()> {
    let dir = std::env::current_dir().context("reading the current directory")?;
    let name = dir
        .file_name()
        .and_then(|name| name.to_str())
        .context("the current directory has no usable name")?
        .to_owned();
    let report = scaffold(&dir, &name, &args)?;
    crate::print_json(&serde_json::to_value(report)?)
}

/// Writes the template into `dir`, which must not already hold a pack.
pub(crate) fn scaffold(dir: &Path, name: &str, args: &PackScaffoldArgs) -> Result<ScaffoldReport> {
    anyhow::ensure!(
        !dir.join("manifest.json").exists(),
        "{} already holds a pack",
        dir.display()
    );
    let names = Names::new(name, &args.namespace)?;
    let template = args.template.unwrap_or(match args.kind {
        Some(PackKindArg::Graph) => PackTemplate::Graph,
        Some(PackKindArg::Plugins) => PackTemplate::PluginTool,
        Some(PackKindArg::Assets) => PackTemplate::Assets,
        Some(PackKindArg::Documents) | None => PackTemplate::Minimal,
    });
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
        PackTemplate::Assets => manifest["kind"] = json!("assets"),
        PackTemplate::Minimal | PackTemplate::Automation | PackTemplate::Graph => {
            manifest["kind"] = json!(if template == PackTemplate::Graph {
                "graph"
            } else {
                "documents"
            });
            manifest["config"] = json!("pack_config.json");
            manifest["inference_slots"] = json!([{
                "name": "worker",
                "description": "Does the pack's work. Any capable profile.",
                "behaviors": [format!("{}-worker", names.kebab)],
            }]);
            files.push((
                "agent_behaviors/worker/system_prompt.md".into(),
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
            if template != PackTemplate::Minimal {
                files.push((
                    format!("schemas/{}_job.graphql", names.name),
                    format!(
                        "type {}Job {{\n  run_id: String @index(unique: true) @immutable\n  request: String\n}}\n",
                        names.title
                    ),
                ));
                manifest["schemas"] = json!([format!("schemas/{}_job.graphql", names.name)]);
            }
            if template == PackTemplate::Graph {
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
                manifest["compiler_version"] = json!(gents::graph_pipeline::COMPILER_VERSION);
            }
            files.push((
                "pack_config.json".into(),
                serde_json::to_string_pretty(&config)? + "\n",
            ));
        }
        PackTemplate::PluginTool => {
            manifest["kind"] = json!("plugins");
            let language = scaffold_plugin(dir, &names, args.language.as_deref())?;
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
    if template == PackTemplate::PluginTool {
        assets.push(format!("plugins/{}.afb", names.name));
    }
    assets.sort();
    manifest["assets"] = json!(assets);
    std::fs::write(
        dir.join("manifest.json"),
        serde_json::to_string_pretty(&manifest)? + "\n",
    )
    .context("writing manifest.json")?;
    if template == PackTemplate::Graph {
        let manifest: gents::pack::PackManifest =
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
        template,
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

/// Where plugin `name`'s `TOOL.md` lives, and a starter for it: the markdown a
/// model reads to use the plugin as a tool.
pub(crate) fn tool_markdown(name: &str) -> (String, String) {
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
pub(crate) fn write_plugin_source(
    source: &Path,
    name: &str,
    language: Option<&str>,
) -> Result<String> {
    let language = language.unwrap_or("rust").trim().to_ascii_lowercase();
    let entry = super::build::plugin_entry(&language).with_context(|| {
        format!(
            "gents cannot build {language:?} plugins; use one of {}",
            gents::pack::SUPPORTED_PLUGIN_LANGUAGES.join(", ")
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

fn config(names: &Names, template: PackTemplate) -> Value {
    let kebab = &names.kebab;
    let behavior = format!("{kebab}-worker");
    let mut config = json!({
        "agent_principal": {},
        "agent_behaviors": [{
            "behavior_id": behavior,
            "display_name": format!("{} worker", names.title),
            "context_id": format!("{kebab}-worker-context"),
            "inference_profile_id": "gents:inference-slot:worker",
        }],
        "contexts": [{
            "context_id": format!("{kebab}-worker-context"),
            "display_name": format!("{} worker", names.title),
            "system_prompt": "./agent_behaviors/worker/system_prompt.md",
            "tools_id": format!("{kebab}-worker-tools"),
        }],
        "tools": [{
            "tools_id": format!("{kebab}-worker-tools"),
            "display_name": format!("{} worker tools", names.title),
        }],
        "tasks": [{
            "task_id": format!("{kebab}-worker-task"),
            "display_name": format!("{} worker task", names.title),
            "behavior_id": behavior,
            "prompt_template": "./tasks/worker_task/prompt.md",
        }],
    });
    match template {
        PackTemplate::Automation => {
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
        PackTemplate::Graph => {
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
                "allowed_callers": ["${GENTS_PACK_AGENT_DID}"],
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
        PackTemplate::Minimal | PackTemplate::PluginTool | PackTemplate::Assets => {}
    }
    config
}

fn task_prompt(names: &Names, template: PackTemplate) -> String {
    match template {
        PackTemplate::Graph => format!(
            "Read the {}Job for this run and write exactly one result with `write_result`.\n",
            names.title
        ),
        PackTemplate::Automation => format!(
            "A {}Job was created: {{{{ doc.request }}}}\n\nDo what it asks.\n",
            names.title
        ),
        _ => "Describe the task this pack's worker performs.\n".into(),
    }
}

fn readme(names: &Names, template: PackTemplate) -> String {
    let (what, io) = match template {
        PackTemplate::Assets => ("carries files and no configuration", "None."),
        PackTemplate::Minimal => ("installs one behavior and one task", "The task's prompt."),
        PackTemplate::Automation => (
            "runs its worker whenever a job document is created",
            "Input: a `Job` document. Output: whatever the task writes.",
        ),
        PackTemplate::Graph => (
            "runs a one-stage graph from a job to one result",
            "Input: a `Job` document at the `start` entry. Output: one `Result`.",
        ),
        PackTemplate::PluginTool => (
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

    fn options(template: PackTemplate) -> PackScaffoldArgs {
        PackScaffoldArgs {
            kind: None,
            namespace: "acme".into(),
            template: Some(template),
            language: None,
        }
    }

    #[tokio::test]
    async fn every_document_template_passes_the_check_and_builds_unchanged() {
        for template in [
            PackTemplate::Assets,
            PackTemplate::Minimal,
            PackTemplate::Automation,
            PackTemplate::Graph,
        ] {
            let root = tempfile::tempdir().unwrap();
            let dir = root.path().join("review_toolkit");
            let report = scaffold(&dir, "review_toolkit", &options(template)).unwrap();
            assert!(report.files.contains(&"manifest.json".to_owned()));

            let check = super::super::check::check_dir(&dir).await;
            assert!(
                check.problems.is_empty(),
                "{template:?}: {:#?}",
                check.problems
            );
            let built = super::super::build::build_pack(&dir, None).unwrap();
            assert_eq!(Some(built.digest), check.digest, "{template:?}");
            if template == PackTemplate::Graph {
                assert_eq!(check.graphs, vec!["review-toolkit".to_owned()]);
            }
        }
    }

    /// A shipped plan that is not what this build compiles is refused by
    /// name, the same way install refuses it.
    #[tokio::test]
    async fn a_shipped_plan_that_differs_from_compilation_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("review_toolkit");
        scaffold(&dir, "review_toolkit", &options(PackTemplate::Graph)).unwrap();
        let plan_path = dir.join("graphs/review_toolkit.plan.json");
        let mut plan: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&plan_path).unwrap()).unwrap();
        plan["digest"] = "sha256:0".into();
        std::fs::write(&plan_path, serde_json::to_vec_pretty(&plan).unwrap()).unwrap();
        let report = super::super::check::check_dir(&dir).await;
        assert!(
            report
                .problems
                .iter()
                .any(|problem| problem.contains("rebuild the pack")),
            "{:#?}",
            report.problems
        );
    }

    #[tokio::test]
    async fn the_plugin_template_builds_and_then_passes_the_check() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("format_check");
        scaffold(&dir, "format_check", &options(PackTemplate::PluginTool)).unwrap();
        let before = super::super::check::check_dir(&dir).await;
        assert!(before.problems.is_empty(), "{:#?}", before.problems);
        {
            let _guard = crate::commands::afterburner_build::compile_lock()
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            super::super::build::build_pack(&dir, None).unwrap();
        }
        let after = super::super::check::check_dir(&dir).await;
        assert!(after.problems.is_empty(), "{:#?}", after.problems);
        assert!(after.digest.is_some());
    }

    #[test]
    fn a_directory_that_holds_a_pack_or_a_bad_name_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("review_toolkit");
        scaffold(&dir, "review_toolkit", &options(PackTemplate::Minimal)).unwrap();
        let again = scaffold(&dir, "review_toolkit", &options(PackTemplate::Minimal)).unwrap_err();
        assert!(
            format!("{again:#}").contains("already holds a pack"),
            "{again:#}"
        );

        let error = scaffold(
            &root.path().join("x"),
            "ReviewToolkit",
            &options(PackTemplate::Minimal),
        )
        .unwrap_err();
        assert!(
            format!("{error:#}").contains("\"review_toolkit\""),
            "{error:#}"
        );
    }
}
