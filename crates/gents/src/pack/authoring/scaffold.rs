use std::path::{Path, PathBuf};

use anyhow::Context;
use serde::Serialize;
use serde_json::{json, Value};

use super::names::Names;
use super::template::{Kind, Template};

#[derive(Debug, thiserror::Error)]
pub enum ScaffoldError {
    #[error("{} already exists", path.display())]
    SourceExists { path: PathBuf },
    #[error("gents cannot build {language:?} plugins; use one of {}", crate::pack::SUPPORTED_PLUGIN_LANGUAGES.join(", "))]
    UnsupportedLanguage { language: String },
    #[error("plugin source parent")]
    SourceParent,
    #[error("reading {}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("writing {}", path.display())]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

type Result<T, E = ScaffoldError> = std::result::Result<T, E>;

#[derive(Clone, Debug)]
pub struct ScaffoldInput {
    pub namespace: String,
    pub template: Option<Template>,
    pub kind: Option<Kind>,
    pub language: Option<String>,
    /// also scaffold a UX plugin (`ux/<name>/plugin.js`) and declare it under `ux`
    pub ux: bool,
}

#[derive(Debug, Serialize)]
pub struct ScaffoldReport {
    pub pack: String,
    pub namespace: String,
    pub kind: &'static str,
    pub template: &'static str,
    pub dir: PathBuf,
    pub files: Vec<String>,
}

impl Template {
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

pub fn scaffold(dir: &Path, name: &str, input: &ScaffoldInput) -> anyhow::Result<ScaffoldReport> {
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

    // Resolve generated paths through the rooted owner before writing, refusing nested symlink escapes.
    let context = crate::toolset::ToolContext::new(dir.to_path_buf(), false)?;
    let mut output_paths = vec!["README.md".to_owned(), "manifest.json".to_owned()];
    if input.ux {
        output_paths.push(format!("ux/{}/plugin.js", names.name));
    }
    match template {
        Template::Assets => {}
        Template::Minimal | Template::Automation | Template::Graph => {
            output_paths.extend([
                "agents/worker/system_prompt.md".to_owned(),
                "tasks/worker_task/prompt.md".to_owned(),
                "pack_config.json".to_owned(),
            ]);
            if template != Template::Minimal {
                output_paths.push(format!("schemas/{}_job.graphql", names.name));
            }
            if template == Template::Graph {
                output_paths.push(format!("schemas/{}_result.graphql", names.name));
                output_paths.push(format!("graphs/{}.plan.json", names.kebab));
            }
        }
        Template::PluginTool => {
            let language = input
                .language
                .as_deref()
                .unwrap_or("rust")
                .trim()
                .to_ascii_lowercase();
            let entry = super::build::plugin_entry(&language).with_context(|| {
                format!(
                    "gents cannot build {language:?} plugins; use one of {}",
                    crate::pack::SUPPORTED_PLUGIN_LANGUAGES.join(", ")
                )
            })?;
            output_paths.extend([
                format!("plugins/{}/{}", names.name, entry),
                format!("plugins/{}/tests/echo.json", names.name),
                format!("plugins/{}/TOOL.md", names.name),
            ]);
            if entry == "source/main.rs" {
                output_paths.push(format!("plugins/{}/Cargo.toml", names.name));
            }
        }
    }
    for path in output_paths {
        context.resolve_path_allow_create(&path)?;
    }

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

    if input.ux {
        let (path, source) = ux_plugin_source(&names);
        let directive = names.name.replace('_', "-");
        manifest["ux"] = json!([{
            "name": names.name,
            "description": format!("{} page, nav row and ::{directive} directive.", names.title),
            "entry": path,
            "contributes": {
                "areas": ["nav", "plugin.pages", "transcript.directives"],
                "directives": [directive],
            },
        }]);
        files.push((path, source));
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
    let entries = std::fs::read_dir(dir).map_err(|source| ScaffoldError::Read {
        path: dir.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry?;
        if entry.file_type()?.is_dir() {
            list_files(root, &entry.path(), out)?;
        } else if let Ok(relative) = entry.path().strip_prefix(root) {
            out.push(relative.to_string_lossy().replace('\\', "/"));
        }
    }
    Ok(())
}

/// The scaffolded UX plugin: a plain ESM file (no build) that lands a nav
/// row, the plugin's own page and a transcript directive, each in the
/// smallest shape that works: the row and the chip are data the shell
/// draws, the page renders in the plugin's frame. Returns its
/// pack-relative path and source.
fn ux_plugin_source(names: &Names) -> (String, String) {
    let path = format!("ux/{}/plugin.js", names.name);
    let directive = names.name.replace('_', "-");
    let source = format!(
        r#"// {title}: a UX plugin the desktop loads from this pack into a frame of
// its own. Plain ESM, no build step; only `@gents/ux-sdk`, `react` and
// `react/jsx-runtime` may be imported. Everything it does goes through
// `ctx`, so disabling the plugin undoes all of it. Run `gents pack check`
// to lint, `gents ux module` to see what the desktop receives. To read and
// write the node's documents, declare a surface under `ux[].data` in the
// manifest and use `useQuery` / `useWrite` from the SDK.
import {{ NAV_AREA, PLUGIN_PAGE_AREA, TRANSCRIPT_DIRECTIVE_AREA, Group, Row }} from '@gents/ux-sdk'
import {{ jsx }} from 'react/jsx-runtime'

/* the plugin's own page, where its nav row goes; a configuration page
   would be an AGENT_SECTIONS_AREA contribution instead */
function Page() {{
  return jsx(Group, {{ title: '{title}', children:
    jsx(Row, {{ label: 'Page', description: 'the plugin\'s own page', children:
      jsx('code', {{ className: 'font-mono text-xs', children: '/plugins/{name}' }}) }}) }})
}}

export default {{
  id: '{name}',
  name: '{title}',
  description: '{title} page, nav row and ::{directive} directive.',
  register(ctx) {{
    ctx.registerMany([
      {{ id: 'nav', area: NAV_AREA, order: 80, data: {{
        id: '{name}:nav', label: '{title}', icon: 'puzzle',
        to: {{ name: 'plugin', pluginId: '{name}' }},
        placement: 'footer', order: 80 }} }},
      {{ id: 'page', area: PLUGIN_PAGE_AREA, render: () => jsx(Page, {{}}) }},
      /* the ::{directive}{{id="..."}} chip the model can write; the shell draws
         the label from the attributes, the click lands here */
      {{ id: 'directive', area: TRANSCRIPT_DIRECTIVE_AREA, data: {{
        name: '{directive}', label: '{title}: {{id}}',
        onClick: () => ctx.shell.navigate({{ name: 'plugin', pluginId: '{name}' }}) }} }},
    ])
  }},
}}
"#,
        title = names.title,
        name = names.name,
        directive = directive,
    );
    (path, source)
}

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

fn scaffold_plugin(dir: &Path, names: &Names, language: Option<&str>) -> Result<String> {
    write_plugin_source(
        &dir.join("plugins").join(&names.name),
        &names.name,
        language,
    )
}

pub fn write_plugin_source(source: &Path, name: &str, language: Option<&str>) -> Result<String> {
    let language = language.unwrap_or("rust").trim().to_ascii_lowercase();
    let entry = super::build::plugin_entry(&language).ok_or_else(|| {
        ScaffoldError::UnsupportedLanguage {
            language: language.clone(),
        }
    })?;
    if source.exists() {
        return Err(ScaffoldError::SourceExists {
            path: source.to_path_buf(),
        });
    }
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
        std::fs::create_dir_all(target.parent().ok_or(ScaffoldError::SourceParent)?)?;
        std::fs::write(&target, contents).map_err(|source| ScaffoldError::Write {
            path: target.clone(),
            source,
        })?;
    }
    Ok(language)
}

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
    use gents_protocol::error_chain::ErrorChain;

    fn input(template: Template) -> ScaffoldInput {
        ScaffoldInput {
            kind: None,
            namespace: "acme".into(),
            template: Some(template),
            language: None,
            ux: false,
        }
    }

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
            let manifest: crate::pack::PackManifest =
                serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).unwrap())
                    .unwrap_or_else(|error| panic!("{template:?}: {}", ErrorChain(&error)));
            crate::pack::validate_manifest("review_toolkit", &manifest)
                .unwrap_or_else(|error| panic!("{template:?}: {}", ErrorChain(&error)));
            if template == Template::Graph {
                assert_eq!(
                    manifest.compiler_version.as_deref(),
                    Some(crate::graph_pipeline::COMPILER_VERSION)
                );
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

    /// `pack new --ux` on a plugins template leaves the .afb unbuilt; the
    /// first `pack check` must still lint the UX module, and the manifest
    /// must name the directive the module actually claims.
    #[tokio::test]
    async fn a_ux_scaffold_is_linted_before_its_plugin_is_built_and_names_its_directive() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("review_toolkit");
        let args = ScaffoldInput {
            ux: true,
            ..input(Template::PluginTool)
        };
        scaffold(&dir, "review_toolkit", &args).unwrap();
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).unwrap()).unwrap();
        let description = manifest["ux"][0]["description"].as_str().unwrap();
        assert!(description.contains("::review-toolkit"), "{description}");
        assert_eq!(
            manifest["ux"][0]["contributes"]["directives"][0],
            "review-toolkit"
        );

        let check = super::super::check::check_dir(&dir).await;
        assert!(check.problems.is_empty(), "{:#?}", check.problems);
        assert_eq!(check.digest, None, "the plugin is not built yet");

        let entry = dir.join(manifest["ux"][0]["entry"].as_str().unwrap());
        std::fs::write(
            &entry,
            "export default { id: 'review_toolkit', register() { eval('1') } }\n",
        )
        .unwrap();
        let check = super::super::check::check_dir(&dir).await;
        assert!(
            check
                .problems
                .iter()
                .any(|p| p.contains("refused by the static lint") && p.contains("dynamic code")),
            "{:#?}",
            check.problems
        );
    }

    struct NoPlugins;

    impl super::super::build::PluginCompiler for NoPlugins {
        fn load_and_validate(&self, _owner: &str, _dir: &Path) -> anyhow::Result<()> {
            unreachable!("an assets pack has no plugin to build")
        }
        fn compile(&self, _owner: &str, _dir: &Path, _out_path: &Path) -> anyhow::Result<()> {
            unreachable!("an assets pack has no plugin to build")
        }
    }

    /// `pack build` refuses a file-produced UX module the webview would
    /// refuse, so a built archive is lint-clean by construction.
    #[tokio::test]
    async fn build_refuses_a_ux_module_the_lint_refuses() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("review_toolkit");
        let args = ScaffoldInput {
            ux: true,
            ..input(Template::Assets)
        };
        scaffold(&dir, "review_toolkit", &args).unwrap();
        let check = super::super::check::check_dir(&dir).await;
        assert!(check.problems.is_empty(), "{:#?}", check.problems);
        let built = super::super::build::build_pack(&dir, &NoPlugins, None).unwrap();
        assert_eq!(Some(built.digest), check.digest);

        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dir.join("manifest.json")).unwrap()).unwrap();
        let entry = dir.join(manifest["ux"][0]["entry"].as_str().unwrap());
        std::fs::write(&entry, "import x from 'https://evil/s.js'\nexport default { id: 'review_toolkit', register() {} }\n").unwrap();
        let error = super::super::build::build_pack(&dir, &NoPlugins, None).unwrap_err();
        assert!(
            format!("{}", ErrorChain(error.as_ref())).contains("static lint refuses")
                && format!("{}", ErrorChain(error.as_ref())).contains("non-sdk import"),
            "{}",
            ErrorChain(error.as_ref())
        );
    }

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

    #[cfg(unix)]
    #[test]
    fn plugin_scaffold_rejects_nested_symlink_escape_before_writing() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let dir = root.path().join("new_pack");
        std::fs::create_dir_all(&dir).unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.join("plugins")).unwrap();

        let result = scaffold(
            &dir,
            "new_pack",
            &ScaffoldInput {
                namespace: "acme".into(),
                template: Some(Template::PluginTool),
                kind: None,
                language: None,
                ux: false,
            },
        );

        assert!(result.is_err(), "nested symlink must be refused");
        assert!(std::fs::read_dir(outside.path()).unwrap().next().is_none());
        assert!(!dir.join("manifest.json").exists());
    }
}
