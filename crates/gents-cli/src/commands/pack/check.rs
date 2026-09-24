//! `gents pack check` and `gents pack graph`: every validation an install
//! runs, for a pack directory, writing nothing; and the README's generated
//! topology diagram.
//!
//! The rules are the installer's own owners, called the way install calls
//! them: the pack writer and reader for files, paths and the digest, the
//! configuration loader and apply plan, the graph compiler with a
//! placeholder owner, and DefraDB itself for every event-source filter
//! against the runtime and pack schemas. Every problem is reported by name,
//! not only the first.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use gents::pack::{declared_paths, PackKind, PackManifest};
use gents::pack_archive::{pack_dir, PackArchive};
use serde::Serialize;
use serde_json::json;

use crate::cli::{PackCheckArgs, PackGraphArgs};

/// The owner a checked pack is bound to. Never written anywhere.
const PLACEHOLDER_OWNER: &str = "did:key:zPackCheckPlaceholderOwner";
const TOPOLOGY_START: &str = "<!-- pack-topology:start -->";
const TOPOLOGY_END: &str = "<!-- pack-topology:end -->";

#[derive(Debug, Serialize)]
pub(crate) struct CheckReport {
    pub(crate) dir: PathBuf,
    pub(crate) pack: Option<String>,
    pub(crate) digest: Option<String>,
    pub(crate) graphs: Vec<String>,
    pub(crate) problems: Vec<String>,
}

pub(crate) async fn check(args: PackCheckArgs) -> Result<()> {
    let dirs = if args.dirs.is_empty() {
        vec![PathBuf::from(".")]
    } else {
        args.dirs
    };
    let mut reports = Vec::with_capacity(dirs.len());
    for dir in &dirs {
        reports.push(check_dir(dir).await);
    }
    let failed = reports
        .iter()
        .filter(|report| !report.problems.is_empty())
        .count();
    crate::print_json(&json!({ "packs": reports }))?;
    anyhow::ensure!(
        failed == 0,
        "{failed} of {} packs failed the check",
        reports.len()
    );
    Ok(())
}

/// Checks the pack directory `dir`.
pub(crate) async fn check_dir(dir: &Path) -> CheckReport {
    let mut report = CheckReport {
        dir: dir.to_path_buf(),
        pack: None,
        digest: None,
        graphs: Vec::new(),
        problems: Vec::new(),
    };
    let manifest = match read_manifest(dir) {
        Ok(manifest) => manifest,
        Err(error) => {
            report.problems.push(format!("{error:#}"));
            return report;
        }
    };
    report.pack = Some(manifest.name.clone());
    let unbuilt = check_files(dir, &manifest, &mut report.problems);
    if !report.problems.is_empty() {
        return report;
    }
    if !unbuilt.is_empty() {
        // `gents pack build` compiles these; the digest exists only after it.
        if let Err(error) = gents::pack::validate_manifest(&manifest.name, &manifest) {
            report.problems.push(format!("{error:#}"));
        }
        if manifest.metadata.kind != PackKind::Plugins {
            report.problems.push(format!(
                "build the plugins ({}) with gents pack build before checking its configuration",
                unbuilt.join(", ")
            ));
        }
        check_readme(dir, &mut report.problems);
        return report;
    }

    let archive = match pack_dir(dir).and_then(|(bytes, _)| PackArchive::from_bytes(&bytes)) {
        Ok(archive) => archive,
        Err(error) => {
            report.problems.push(format!("{error:#}"));
            return report;
        }
    };
    report.digest = Some(archive.digest().to_owned());

    if matches!(
        manifest.metadata.kind,
        PackKind::Documents | PackKind::Graph
    ) {
        match load_config(&archive) {
            Ok(config) => {
                if manifest.metadata.kind == PackKind::Graph {
                    match gents::graph_package::check_graph_pack(&archive, PLACEHOLDER_OWNER) {
                        Ok(graphs) => report.graphs = graphs,
                        Err(error) => report.problems.push(format!("{error:#}")),
                    }
                    check_topology(dir, &config, &mut report.problems);
                }
                if let Err(error) = check_filters(dir, &config, &mut report.problems).await {
                    report.problems.push(format!("{error:#}"));
                }
            }
            Err(error) => report.problems.push(format!("{error:#}")),
        }
    }
    check_readme(dir, &mut report.problems);
    report
}

fn read_manifest(dir: &Path) -> Result<PackManifest> {
    let path = dir.join("manifest.json");
    let bytes = std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", path.display()))
}

/// Declared files that are missing, and present files that are not declared.
/// Plugin sources are built into their artifacts and never travel, and
/// directories a pack may never carry (`runs/`, `target/`, dotfiles) are
/// skipped. Returns the plugins whose artifact is missing but has a source
/// to build it from.
fn check_files(dir: &Path, manifest: &PackManifest, problems: &mut Vec<String>) -> Vec<String> {
    let declared = declared_paths(manifest);
    let mut unbuilt = Vec::new();
    for path in &declared {
        if dir.join(path).is_file() {
            continue;
        }
        match manifest
            .metadata
            .plugins
            .iter()
            .find(|plugin| &plugin.artifact == path && plugin.source.is_some())
        {
            Some(plugin) => unbuilt.push(plugin.name.clone()),
            None => problems.push(format!("declares {path}, which is missing")),
        }
    }
    let sources: Vec<String> = manifest
        .metadata
        .plugins
        .iter()
        .filter_map(|plugin| plugin.source.as_ref())
        .map(|source| format!("{}/", source.trim_end_matches('/')))
        .collect();
    let mut present = Vec::new();
    if let Err(error) = walk(dir, dir, &mut present) {
        problems.push(format!("{error:#}"));
    }
    for path in present {
        if declared.binary_search(&path).is_err()
            && !sources
                .iter()
                .any(|source| path.starts_with(source.as_str()))
        {
            problems.push(format!("{path} is present but not declared in assets"));
        }
    }
    unbuilt
}

fn walk(root: &Path, dir: &Path, out: &mut Vec<String>) -> Result<()> {
    for entry in std::fs::read_dir(dir).with_context(|| format!("reading {}", dir.display()))? {
        let entry = entry?;
        let path = entry.path();
        let relative = path
            .strip_prefix(root)
            .context("walked outside the pack")?
            .to_string_lossy()
            .replace('\\', "/");
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.')
            || matches!(
                name.as_str(),
                "runs" | "target" | "node_modules" | "__pycache__"
            )
        {
            continue;
        }
        if entry.file_type()?.is_dir() {
            walk(root, &path, out)?;
        } else {
            out.push(relative);
        }
    }
    Ok(())
}

fn load_config(archive: &PackArchive) -> Result<gents::document_config::PackConfig> {
    let config = gents::pack::load_pack_config(
        archive.manifest(),
        &gents::pack::PackInstallOptions {
            agent_did: PLACEHOLDER_OWNER.into(),
        },
        &|path| archive.asset(path).map(Vec::from),
        &|_| None,
    )?;
    gents::config_client::DesiredStateApplyPlan::from_pack_config(&config)?;
    Ok(config)
}

/// Runs every event-source filter as a query against the runtime schemas and
/// the pack's own, in a throwaway in-memory node.
async fn check_filters(
    dir: &Path,
    config: &gents::document_config::PackConfig,
    problems: &mut Vec<String>,
) -> Result<()> {
    if config
        .event_sources
        .iter()
        .all(|source| source.filter.is_none())
    {
        return Ok(());
    }
    let node = std::sync::Arc::new(gents::defra_node::EmbeddedNode::builder().build().await?);
    gents::ensure_runtime_schemas(node.as_ref()).await?;
    let access = gents::config_client::ConfigAccess::Local(node.clone());
    crate::commands::schema::apply_pack_schemas_if_present(&access, dir).await?;
    for source in &config.event_sources {
        let Some(filter) = source.filter.as_deref() else {
            continue;
        };
        let query = format!(
            "{{ {}(filter: {filter}, limit: 1) {{ _docID }} }}",
            source.source_collection
        );
        if let Err(error) = access.execute(&query).await {
            problems.push(format!(
                "event source {} filter is not valid on {}: {error:#}",
                source.event_source_id, source.source_collection
            ));
        }
    }
    Ok(())
}

fn check_readme(dir: &Path, problems: &mut Vec<String>) {
    match std::fs::read_to_string(dir.join("README.md")) {
        Ok(text) if text.contains("## ") => {}
        Ok(_) => problems.push("README.md needs sections on configuration and usage".into()),
        Err(_) => problems.push("README.md is missing".into()),
    }
}

fn check_topology(
    dir: &Path,
    config: &gents::document_config::PackConfig,
    problems: &mut Vec<String>,
) {
    let block = topology_block(config);
    let current = std::fs::read_to_string(dir.join("README.md")).unwrap_or_default();
    if !current.contains(&block) {
        problems.push(
            "README.md topology diagram is stale or missing; run gents pack graph --write-readme"
                .into(),
        );
    }
}

/// The README's generated diagram: every graph's nodes and port edges.
pub(crate) fn topology_block(config: &gents::document_config::PackConfig) -> String {
    let mut labels: Vec<&str> = Vec::new();
    fn id<'a>(labels: &mut Vec<&'a str>, label: &'a str) -> usize {
        labels
            .iter()
            .position(|known| *known == label)
            .unwrap_or_else(|| {
                labels.push(label);
                labels.len() - 1
            })
    }
    let mut edges = Vec::new();
    for graph in &config.graph_intents {
        for node in &graph.nodes {
            id(&mut labels, &node.node_id);
        }
        for edge in &graph.edges {
            let from = id(&mut labels, &edge.from.node_id);
            let to = id(&mut labels, &edge.to.node_id);
            let label = format!("{} \u{2192} {}", edge.from.port, edge.to.port);
            edges.push(format!("    n{from} -->|{}| n{to}", json!(label)));
        }
    }
    let nodes = labels
        .iter()
        .enumerate()
        .map(|(index, label)| format!("    n{index}[{}]", json!(label)))
        .collect::<Vec<_>>();
    format!(
        "{TOPOLOGY_START}\n```mermaid\nflowchart LR\n{}\n{}\n```\n{TOPOLOGY_END}",
        nodes.join("\n"),
        edges.join("\n")
    )
}

/// `gents pack graph`: prints the topology diagram, or writes it into the
/// README in place of the previous one.
pub(crate) fn graph(args: PackGraphArgs) -> Result<()> {
    let dir = args.dir.unwrap_or_else(|| PathBuf::from("."));
    let (bytes, _) = pack_dir(&dir).with_context(|| format!("packing {}", dir.display()))?;
    let archive = PackArchive::from_bytes(&bytes)?;
    anyhow::ensure!(
        archive.manifest().metadata.kind == PackKind::Graph,
        "{} is not a graph pack",
        archive.manifest().name
    );
    if !args.write_readme {
        println!("{}", topology_block(&load_config(&archive)?));
        return Ok(());
    }
    write_topology(&dir)
}

/// Writes the graph pack at `dir`'s topology diagram into its README, in
/// place of the previous one or as a new section at the end.
pub(crate) fn write_topology(dir: &Path) -> Result<()> {
    let (bytes, _) = pack_dir(dir).with_context(|| format!("packing {}", dir.display()))?;
    let block = topology_block(&load_config(&PackArchive::from_bytes(&bytes)?)?);
    let readme = dir.join("README.md");
    let text = std::fs::read_to_string(&readme)
        .with_context(|| format!("reading {}", readme.display()))?;
    let updated = match (text.find(TOPOLOGY_START), text.find(TOPOLOGY_END)) {
        (Some(start), Some(end)) if start < end => {
            format!(
                "{}{block}{}",
                &text[..start],
                &text[end + TOPOLOGY_END.len()..]
            )
        }
        _ => format!(
            "{}\n## Declared topology\n\nCompiled capability edges.\n\n{block}\n",
            text.trim_end_matches('\n')
        ),
    };
    std::fs::write(&readme, updated).with_context(|| format!("writing {}", readme.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every pack in this repository passes the check: the CI gate for packs.
    #[tokio::test]
    async fn every_pack_in_the_repository_passes_the_check() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packs");
        let mut dirs: Vec<PathBuf> = std::fs::read_dir(&root)
            .unwrap()
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.join("manifest.json").is_file())
            .collect();
        dirs.sort();
        assert!(!dirs.is_empty());
        for dir in dirs {
            let report = check_dir(&dir).await;
            assert!(
                report.problems.is_empty(),
                "{}: {:#?}",
                dir.display(),
                report.problems
            );
        }
    }

    fn copy_pack(name: &str) -> tempfile::TempDir {
        let pack = gents::pack::resolve_pack(name).unwrap();
        let dir = tempfile::tempdir().unwrap();
        for path in declared_paths(&pack.manifest) {
            let target = dir.path().join(&path);
            std::fs::create_dir_all(target.parent().unwrap()).unwrap();
            std::fs::write(&target, pack.asset(&path).unwrap()).unwrap();
        }
        dir
    }

    #[tokio::test]
    async fn missing_and_undeclared_files_are_each_named() {
        let dir = copy_pack("mailbox");
        let manifest = read_manifest(dir.path()).unwrap();
        let dropped = manifest.metadata.assets[0].clone();
        std::fs::remove_file(dir.path().join(&dropped)).unwrap();
        std::fs::write(dir.path().join("notes.md"), "stray").unwrap();
        std::fs::create_dir_all(dir.path().join("runs/job")).unwrap();
        std::fs::write(dir.path().join("runs/job/log.txt"), "ignored").unwrap();

        let report = check_dir(dir.path()).await;
        assert!(
            report
                .problems
                .contains(&format!("declares {dropped}, which is missing")),
            "{:#?}",
            report.problems
        );
        assert!(
            report
                .problems
                .contains(&"notes.md is present but not declared in assets".to_owned()),
            "{:#?}",
            report.problems
        );
        assert!(
            !report
                .problems
                .iter()
                .any(|problem| problem.contains("runs/")),
            "{:#?}",
            report.problems
        );
    }

    #[tokio::test]
    async fn an_invalid_filter_is_reported_with_its_event_source() {
        let dir = copy_pack("repo_maintenance");
        let config_path = dir.path().join("pack_config.json");
        let text = std::fs::read_to_string(&config_path).unwrap();
        std::fs::write(
            &config_path,
            text.replacen("{ binding_id: {", "{ no_such_field: {", 1),
        )
        .unwrap();
        let report = check_dir(dir.path()).await;
        assert!(
            report.problems.iter().any(|problem| problem
                .starts_with("event source maintenance-execute filter is not valid")),
            "{:#?}",
            report.problems
        );
    }

    #[tokio::test]
    async fn a_stale_topology_diagram_is_reported_and_rewritten() {
        let name = gents::pack::pack_catalog()
            .unwrap()
            .into_iter()
            .find(|manifest| manifest.metadata.kind == PackKind::Graph)
            .expect("a bundled graph pack")
            .name;
        let dir = copy_pack(&name);
        let readme = dir.path().join("README.md");
        let text = std::fs::read_to_string(&readme).unwrap();
        let start = text.find(TOPOLOGY_START).unwrap();
        std::fs::write(&readme, &text[..start]).unwrap();
        assert!(check_dir(dir.path())
            .await
            .problems
            .iter()
            .any(|problem| problem.contains("topology diagram is stale")));

        graph(PackGraphArgs {
            dir: Some(dir.path().to_path_buf()),
            write_readme: true,
        })
        .unwrap();
        let report = check_dir(dir.path()).await;
        assert!(report.problems.is_empty(), "{:#?}", report.problems);
    }
}
