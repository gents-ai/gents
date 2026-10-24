use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;

use crate::cli::{PackKindArg, PackNewArgs, PackScaffoldArgs, PackTemplate};

#[derive(Debug, Serialize)]
pub(crate) struct ScaffoldReport {
    pub(crate) pack: String,
    pub(crate) namespace: String,
    pub(crate) template: PackTemplate,
    pub(crate) dir: PathBuf,
    pub(crate) files: Vec<String>,
}

fn shared_template(template: PackTemplate) -> gents::pack::authoring::Template {
    use gents::pack::authoring::Template;
    match template {
        PackTemplate::Minimal => Template::Minimal,
        PackTemplate::Automation => Template::Automation,
        PackTemplate::Graph => Template::Graph,
        PackTemplate::PluginTool => Template::PluginTool,
        PackTemplate::Assets => Template::Assets,
    }
}

fn shared_kind(kind: Option<PackKindArg>) -> Option<gents::pack::authoring::Kind> {
    use gents::pack::authoring::Kind;
    kind.map(|kind| match kind {
        PackKindArg::Documents => Kind::Documents,
        PackKindArg::Graph => Kind::Graph,
        PackKindArg::Assets => Kind::Assets,
        PackKindArg::Plugins => Kind::Plugins,
    })
}

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
    Ok(crate::print_json(&serde_json::to_value(report)?)?)
}

pub(crate) fn init(args: PackScaffoldArgs) -> Result<()> {
    let dir = std::env::current_dir().context("reading the current directory")?;
    let name = dir
        .file_name()
        .and_then(|name| name.to_str())
        .context("the current directory has no usable name")?
        .to_owned();
    let report = scaffold(&dir, &name, &args)?;
    Ok(crate::print_json(&serde_json::to_value(report)?)?)
}

pub(crate) fn scaffold(dir: &Path, name: &str, args: &PackScaffoldArgs) -> Result<ScaffoldReport> {
    let template = args.template.map(shared_template);
    let report = gents::pack::authoring::scaffold(
        dir,
        name,
        &gents::pack::authoring::ScaffoldInput {
            namespace: args.namespace.clone(),
            template,
            kind: shared_kind(args.kind),
            language: args.language.clone(),
            ux: args.ux,
        },
    )?;
    let template = match report.template {
        "minimal" => PackTemplate::Minimal,
        "automation" => PackTemplate::Automation,
        "graph" => PackTemplate::Graph,
        "plugin-tool" => PackTemplate::PluginTool,
        "assets" => PackTemplate::Assets,
        other => anyhow::bail!("shared scaffold returned unknown template {other:?}"),
    };
    Ok(ScaffoldReport {
        pack: report.pack,
        namespace: report.namespace,
        template,
        dir: report.dir,
        files: report.files,
    })
}

pub(crate) fn tool_markdown(name: &str) -> (String, String) {
    gents::pack::authoring::tool_markdown(name)
}

pub(crate) fn write_plugin_source(
    source: &Path,
    name: &str,
    language: Option<&str>,
) -> Result<String> {
    gents::pack::authoring::write_plugin_source(source, name, language)
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
            ux: false,
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
}
