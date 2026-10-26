use gents_protocol::error_chain::ErrorChain;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::json;

use crate::pack::{declared_paths, PackAssetError, PackKind, PackManifest};
use crate::pack_archive::{pack_dir, PackArchive};

#[derive(Debug, thiserror::Error)]
pub enum CheckError {
    #[error(transparent)]
    Asset(#[from] PackAssetError),
    #[error("reading {}", path.display())]
    OpenAsset {
        path: PathBuf,
        #[source]
        source: PackAssetError,
    },
    #[error("reading {}", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("parsing {}", path.display())]
    Parse {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("walked outside the pack")]
    OutsidePack,
    #[error("packing {}", path.display())]
    Pack {
        path: PathBuf,
        #[source]
        source: crate::pack_archive::format::PackFormatError,
    },
    #[error("writing {}", path.display())]
    Write {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Loader(#[from] crate::pack::LoaderError),
    #[error(transparent)]
    DesiredState(#[from] crate::config_client::DesiredStateError),
    #[error(transparent)]
    Archive(#[from] crate::pack_archive::PackArchiveError),
}

type Result<T, E = CheckError> = std::result::Result<T, E>;

pub const PLACEHOLDER_OWNER: &str = "did:key:zPackCheckPlaceholderOwner";
const TOPOLOGY_START: &str = "<!-- pack-topology:start -->";
const TOPOLOGY_END: &str = "<!-- pack-topology:end -->";

#[derive(Debug, Serialize)]
pub struct CheckReport {
    pub dir: PathBuf,
    pub pack: Option<String>,
    pub digest: Option<String>,
    pub graphs: Vec<String>,
    pub problems: Vec<String>,
    pub unverified: Vec<&'static str>,
}

pub async fn check_dir(dir: &Path) -> CheckReport {
    let mut report = CheckReport {
        dir: dir.to_path_buf(),
        pack: None,
        digest: None,
        graphs: Vec::new(),
        problems: Vec::new(),
        unverified: Vec::new(),
    };
    let manifest = match read_manifest(dir) {
        Ok(manifest) => manifest,
        Err(error) => {
            report.problems.push(format!("{}", ErrorChain(&error)));
            return report;
        }
    };
    report.pack = Some(manifest.name.clone());
    let unbuilt = check_files(dir, &manifest, &mut report.problems);
    if !report.problems.is_empty() {
        return report;
    }
    // Before the unbuilt early return: a scaffolded pack is linted on its
    // first `check`, not only once its .afb plugins have been built.
    check_ux_plugins(dir, &manifest, &mut report.problems);
    if !unbuilt.is_empty() {
        if let Err(error) = crate::pack::validate_manifest(&manifest.name, &manifest) {
            report.problems.push(format!("{}", ErrorChain(&error)));
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

    let archive = match pack_dir(dir)
        .map_err(crate::pack_archive::PackArchiveError::from)
        .and_then(|(bytes, _)| PackArchive::from_bytes(&bytes))
    {
        Ok(archive) => archive,
        Err(error) => {
            report.problems.push(format!("{}", ErrorChain(&error)));
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
                    match crate::graph_package::check_graph_pack(&archive, PLACEHOLDER_OWNER) {
                        Ok(plans) => {
                            report.graphs = plans.into_iter().map(|plan| plan.graph_id).collect()
                        }
                        Err(error) => report.problems.push(format!("{}", ErrorChain(&error))),
                    }
                    check_topology(dir, &config, &mut report.problems);
                }
                if config
                    .event_sources
                    .iter()
                    .any(|source| source.filter.is_some())
                {
                    report.unverified.push("event_source_filters");
                }
            }
            Err(error) => report.problems.push(format!("{}", ErrorChain(&error))),
        }
    }
    if dir.join("experiment.json").is_file() {
        report.unverified.push("experiment_scenario");
    }
    check_readme(dir, &mut report.problems);
    report
}

pub fn read_manifest(dir: &Path) -> Result<PackManifest> {
    let path = dir.join("manifest.json");
    let mut file = crate::pack::open_pack_asset(dir, "manifest.json").map_err(|source| {
        CheckError::OpenAsset {
            path: path.clone(),
            source,
        }
    })?;
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut file, &mut bytes).map_err(|source| CheckError::Read {
        path: path.clone(),
        source,
    })?;
    serde_json::from_slice(&bytes).map_err(|source| CheckError::Parse { path, source })
}

pub fn check_files(dir: &Path, manifest: &PackManifest, problems: &mut Vec<String>) -> Vec<String> {
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
        problems.push(format!("{}", ErrorChain(&error)));
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
    let entries = std::fs::read_dir(dir).map_err(|source| CheckError::Read {
        path: dir.to_path_buf(),
        source,
    })?;
    for entry in entries {
        let entry = entry?;
        let path = entry.path();
        let relative = path
            .strip_prefix(root)
            .map_err(|_| CheckError::OutsidePack)?
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

pub fn load_config(archive: &PackArchive) -> Result<crate::document_config::PackConfig> {
    let config = crate::pack::load_pack_config(
        archive.manifest(),
        &crate::pack::PackInstallOptions {
            node_did: PLACEHOLDER_OWNER.into(),
        },
        &|path| archive.asset(path).map(Vec::from),
        &|_| None,
    )?;
    crate::config_client::DesiredStateApplyPlan::from_pack_config(&config)?;
    Ok(config)
}

pub fn check_readme(dir: &Path, problems: &mut Vec<String>) {
    match read_pack_text_asset(dir, "README.md") {
        Ok(text) if text.contains("## ") => {}
        Ok(_) => problems.push("README.md needs sections on configuration and usage".into()),
        Err(_) => problems.push("README.md is missing".into()),
    }
}

fn check_topology(
    dir: &Path,
    config: &crate::document_config::PackConfig,
    problems: &mut Vec<String>,
) {
    let block = topology_block(config);
    let current = read_pack_text_asset(dir, "README.md").unwrap_or_default();
    if !current.contains(&block) {
        problems.push(
            "README.md topology diagram is stale or missing; run gents pack graph --write-readme"
                .into(),
        );
    }
}

fn read_pack_text_asset(dir: &Path, path: &str) -> Result<String> {
    let mut file = crate::pack::open_pack_asset(dir, path)?;
    let mut text = String::new();
    std::io::Read::read_to_string(&mut file, &mut text).map_err(|source| CheckError::Read {
        path: PathBuf::from(path),
        source,
    })?;
    Ok(text)
}

pub fn topology_block(config: &crate::document_config::PackConfig) -> String {
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

pub fn write_topology(dir: &Path) -> Result<()> {
    let (bytes, _) = pack_dir(dir).map_err(|source| CheckError::Pack {
        path: dir.to_path_buf(),
        source,
    })?;
    let block = topology_block(&load_config(&PackArchive::from_bytes(&bytes)?)?);
    let readme = dir.join("README.md");
    let text = read_pack_text_asset(dir, "README.md")?;
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
    std::fs::write(&readme, updated).map_err(|source| CheckError::Write {
            path: readme.clone(),
            source,
        })
}

/// The static lint every file-produced UX plugin must pass: what the
/// webview would refuse at load, reported here at the pack instead.
fn check_ux_plugins(dir: &Path, manifest: &PackManifest, problems: &mut Vec<String>) {
    if manifest.metadata.ux.is_empty() {
        return;
    }
    match crate::ux_plugin::lint_pack_dir(dir, &manifest.metadata.ux) {
        Ok(findings) => {
            for (name, findings) in findings {
                if !findings.is_empty() {
                    problems.push(format!(
                        "ux plugin {name:?} is refused by the static lint:\n{}",
                        crate::ux_plugin::lint::describe(&findings)
                    ));
                }
            }
        }
        Err(error) => problems.push(format!("{}", ErrorChain(&error))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scaffold_minimal(dir: &Path, name: &str) {
        super::super::scaffold::scaffold(
            dir,
            name,
            &super::super::scaffold::ScaffoldInput {
                kind: None,
                namespace: "acme".into(),
                template: Some(super::super::template::Template::Minimal),
                language: None,
                ux: false,
            },
        )
        .unwrap();
    }

    fn scaffold_graph(dir: &Path, name: &str) {
        super::super::scaffold::scaffold(
            dir,
            name,
            &super::super::scaffold::ScaffoldInput {
                kind: None,
                namespace: "acme".into(),
                template: Some(super::super::template::Template::Graph),
                language: None,
                ux: false,
            },
        )
        .unwrap();
    }

    #[tokio::test]
    async fn a_scaffolded_minimal_pack_checks_clean_with_a_digest() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("review_toolkit");
        scaffold_minimal(&dir, "review_toolkit");
        let report = check_dir(&dir).await;
        assert!(report.problems.is_empty(), "{:#?}", report.problems);
        assert!(report.unverified.is_empty(), "{:#?}", report.unverified);
        assert!(report.digest.is_some());
        assert_eq!(report.graphs, Vec::<String>::new());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn native_check_and_pack_writer_refuse_manifest_symlinks_outside_the_pack() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let dir = root.path().join("review_toolkit");
        scaffold_minimal(&dir, "review_toolkit");
        let manifest = std::fs::read(dir.join("manifest.json")).unwrap();
        let sentinel = outside.path().join("manifest.json");
        std::fs::write(&sentinel, manifest).unwrap();
        std::fs::remove_file(dir.join("manifest.json")).unwrap();
        std::os::unix::fs::symlink(&sentinel, dir.join("manifest.json")).unwrap();

        let report = check_dir(&dir).await;
        assert!(report.pack.is_none(), "native check must reject the link");
        assert!(!report.problems.is_empty());
        assert!(
            crate::pack_archive::pack_dir(&dir).is_err(),
            "pack writer must reject the same external manifest link"
        );
    }

    #[tokio::test]
    async fn a_scaffolded_graph_pack_checks_clean_and_names_its_graph() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("review_toolkit");
        scaffold_graph(&dir, "review_toolkit");
        let report = check_dir(&dir).await;
        assert!(report.problems.is_empty(), "{:#?}", report.problems);
        assert_eq!(report.graphs, vec!["review-toolkit".to_owned()]);
    }

    #[tokio::test]
    async fn a_shipped_plan_that_differs_from_compilation_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("review_toolkit");
        scaffold_graph(&dir, "review_toolkit");
        let plan_path = dir.join("graphs/review_toolkit.plan.json");
        let mut plan: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&plan_path).unwrap()).unwrap();
        plan["digest"] = "sha256:0".into();
        std::fs::write(&plan_path, serde_json::to_vec_pretty(&plan).unwrap()).unwrap();
        let report = check_dir(&dir).await;
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
    async fn missing_and_undeclared_files_are_each_named() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("review_toolkit");
        scaffold_minimal(&dir, "review_toolkit");
        let manifest = read_manifest(&dir).unwrap();
        let dropped = manifest.metadata.assets[0].clone();
        std::fs::remove_file(dir.join(&dropped)).unwrap();
        std::fs::write(dir.join("notes.md"), "stray").unwrap();
        std::fs::create_dir_all(dir.join("runs/job")).unwrap();
        std::fs::write(dir.join("runs/job/log.txt"), "ignored").unwrap();

        let report = check_dir(&dir).await;
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
    async fn an_experiment_json_is_reported_unverified() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("review_toolkit");
        scaffold_minimal(&dir, "review_toolkit");
        std::fs::write(dir.join("experiment.json"), b"{}").unwrap();
        let manifest_path = dir.join("manifest.json");
        let text = std::fs::read_to_string(&manifest_path).unwrap();
        let updated = text.replace("\"assets\": [", "\"assets\": [\n    \"experiment.json\",");
        std::fs::write(&manifest_path, updated).unwrap();
        let report = check_dir(&dir).await;
        assert!(
            report.unverified.contains(&"experiment_scenario"),
            "{:#?}",
            report.unverified
        );
    }

    #[tokio::test]
    async fn a_missing_readme_is_reported_and_topology_rewrite_fixes_a_graph_pack() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("review_toolkit");
        scaffold_graph(&dir, "review_toolkit");
        let readme = dir.join("README.md");
        let text = std::fs::read_to_string(&readme).unwrap();
        let start = text.find(TOPOLOGY_START).unwrap();
        std::fs::write(&readme, &text[..start]).unwrap();
        assert!(check_dir(&dir)
            .await
            .problems
            .iter()
            .any(|problem| problem.contains("topology diagram is stale")));
        write_topology(&dir).unwrap();
        let report = check_dir(&dir).await;
        assert!(report.problems.is_empty(), "{:#?}", report.problems);
    }

    #[test]
    fn the_topology_block_renders_the_scaffolded_graph() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("review_toolkit");
        scaffold_graph(&dir, "review_toolkit");
        let (bytes, _) = pack_dir(&dir).unwrap();
        let mut config = load_config(&PackArchive::from_bytes(&bytes).unwrap()).unwrap();
        let block = topology_block(&config);
        assert!(block.contains(TOPOLOGY_START));
        assert!(block.contains("flowchart LR"));
        assert!(block.contains("n0[\"worker\"]"));
        assert!(!block.contains(" -->|"));
        let graph = &mut config.graph_intents[0];
        let mut next = graph.nodes[0].clone();
        next.node_id = "reviewer".into();
        graph.nodes.push(next);
        graph.edges.push(
            serde_json::from_value(json!({
                "from": {"node_id": "worker", "port": "result"},
                "to": {"node_id": "reviewer", "port": "job"},
            }))
            .unwrap(),
        );
        let block = topology_block(&config);
        assert!(block.contains("n1[\"reviewer\"]"));
        assert!(block.contains("n0 -->|\"result → job\"| n1"));
    }

    #[tokio::test]
    async fn a_directory_that_is_not_a_pack_reports_the_manifest_error() {
        let root = tempfile::tempdir().unwrap();
        let report = check_dir(&root.path().join("empty")).await;
        assert!(
            report
                .problems
                .iter()
                .any(|problem| problem.contains("manifest.json")),
            "{:#?}",
            report.problems
        );
        assert_eq!(report.pack, None);
    }
}
