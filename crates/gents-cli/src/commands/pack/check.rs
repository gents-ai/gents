use gents_protocol::error_chain::ErrorChain;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use gents::pack_archive::{pack_dir, PackArchive};
use gents::NodeIdentity;
use serde::Serialize;
use serde_json::json;

use crate::cli::{PackCheckArgs, PackGraphArgs};

#[cfg(test)]
const TOPOLOGY_START: &str = "<!-- pack-topology:start -->";

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

pub(crate) async fn check_dir(dir: &Path) -> CheckReport {
    let shared = gents::pack::authoring::check_dir(dir).await;
    let mut report = CheckReport {
        dir: shared.dir,
        pack: shared.pack,
        digest: shared.digest,
        graphs: shared.graphs,
        problems: shared.problems,
    };
    if !report.problems.is_empty() || report.digest.is_none() {
        return report;
    }

    let manifest = match read_manifest(dir) {
        Ok(manifest) => manifest,
        Err(error) => {
            report.problems.push(ErrorChain(error.as_ref()).to_string());
            return report;
        }
    };
    let archive = match pack_dir(dir)
        .map_err(gents::pack_archive::PackArchiveError::from)
        .and_then(|(bytes, _)| PackArchive::from_bytes(&bytes))
    {
        Ok(archive) => archive,
        Err(error) => {
            report.problems.push(ErrorChain(&error).to_string());
            return report;
        }
    };
    if matches!(
        manifest.metadata.kind,
        gents::pack::PackKind::Documents | gents::pack::PackKind::Graph
    ) {
        match load_config(&archive) {
            Ok(config) => {
                if let Err(error) = check_filters(dir, &config, &mut report.problems).await {
                    report.problems.push(ErrorChain(error.as_ref()).to_string());
                }
            }
            Err(error) => report.problems.push(ErrorChain(error.as_ref()).to_string()),
        }
    }
    if dir.join("experiment.json").is_file() {
        if let Err(error) = super::scenario::validate_scenario_defaults(dir) {
            report.problems.push(ErrorChain(error.as_ref()).to_string());
        }
    }
    report
}

fn read_manifest(dir: &Path) -> Result<gents::pack::PackManifest> {
    Ok(gents::pack::authoring::read_manifest(dir)?)
}

fn load_config(archive: &PackArchive) -> Result<gents::document_config::PackConfig> {
    Ok(gents::pack::authoring::load_config(archive)?)
}

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
    let directory = tempfile::tempdir()?;
    let identity = gents::KeyIdentity::load_or_create(directory.path().join("node.key"), None)?;
    let node = std::sync::Arc::new(
        gents::defra_node::EmbeddedNode::builder()
            .with_node_identity_did(identity.did())
            .build()
            .await?,
    );
    gents::ensure_runtime_schemas(node.as_ref()).await?;
    let access = gents::config_client::ConfigAccess::local(node.clone());
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
                "event source {} filter is not valid on {}: {}",
                source.event_source_id,
                source.source_collection,
                ErrorChain(&error)
            ));
        }
    }
    Ok(())
}

pub(crate) fn topology_block(config: &gents::document_config::PackConfig) -> String {
    gents::pack::authoring::topology_block(config)
}

pub(crate) fn graph(args: PackGraphArgs) -> Result<()> {
    let dir = args.dir.unwrap_or_else(|| PathBuf::from("."));
    let (bytes, _) = pack_dir(&dir).with_context(|| format!("packing {}", dir.display()))?;
    let archive = PackArchive::from_bytes(&bytes)?;
    anyhow::ensure!(
        archive.manifest().metadata.kind == gents::pack::PackKind::Graph,
        "{} is not a graph pack",
        archive.manifest().name
    );
    if !args.write_readme {
        println!("{}", topology_block(&load_config(&archive)?));
        return Ok(());
    }
    write_topology(&dir)
}

pub(crate) fn write_topology(dir: &Path) -> Result<()> {
    Ok(gents::pack::authoring::write_topology(dir)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn event_filters_validate_with_owner_bound_runtime_schemas() {
        let directory = super::super::test_support::fixture_dir("documents_fixture");
        let (bytes, _) = pack_dir(&directory).unwrap();
        let archive = PackArchive::from_bytes(&bytes).unwrap();
        let config = load_config(&archive).unwrap();
        let mut problems = Vec::new();
        check_filters(&directory, &config, &mut problems)
            .await
            .unwrap();
        assert!(problems.is_empty(), "{problems:#?}");
    }

    #[tokio::test]
    async fn every_fixture_pack_passes_the_check() {
        let dirs = super::super::test_support::every_fixture_dir();
        assert!(!dirs.is_empty());
        for dir in dirs {
            let copy = copy_pack(&dir);
            let manifest = read_manifest(copy.path()).unwrap();
            super::super::test_support::build_unbuilt_plugins(copy.path(), &manifest);
            let report = check_dir(copy.path()).await;
            assert!(
                report.problems.is_empty(),
                "{}: {:#?}",
                dir.display(),
                report.problems
            );
        }
    }

    fn copy_pack(dir: &Path) -> tempfile::TempDir {
        let copy = tempfile::tempdir().unwrap();
        super::super::test_support::copy_tree(dir, copy.path()).unwrap();
        copy
    }

    #[tokio::test]
    async fn missing_and_undeclared_files_are_each_named() {
        let dir = copy_pack(&super::super::test_support::fixture_dir(
            "documents_fixture",
        ));
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
        let dir = copy_pack(&super::super::test_support::fixture_dir(
            "documents_fixture",
        ));
        let config_path = dir.path().join("pack_config.json");
        let text = std::fs::read_to_string(&config_path).unwrap();
        std::fs::write(
            &config_path,
            text.replacen("{ status: {", "{ no_such_field: {", 1),
        )
        .unwrap();
        let report = check_dir(dir.path()).await;
        assert!(
            report
                .problems
                .iter()
                .any(|problem| problem
                    .starts_with("event source fixture-worker filter is not valid")),
            "{:#?}",
            report.problems
        );
    }

    #[tokio::test]
    async fn a_stale_topology_diagram_is_reported_and_rewritten() {
        let dir = copy_pack(&super::super::test_support::fixture_dir("review_graph"));
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

    #[tokio::test]
    async fn a_scenario_that_fails_validation_fails_the_check() {
        let dir = copy_pack(&super::super::test_support::fixture_dir(
            "documents_fixture",
        ));
        let experiment_path = dir.path().join("experiment.json");
        let text = std::fs::read_to_string(&experiment_path).unwrap();
        std::fs::write(
            &experiment_path,
            text.replacen(
                "\"backend_preset\": \"vllm\"",
                "\"backend_preset\": \"vllm\", \"tool_package\": \"no_such_package\"",
                1,
            ),
        )
        .unwrap();
        let report = check_dir(dir.path()).await;
        assert!(!report.problems.is_empty(), "{:#?}", report.problems);
    }

    #[tokio::test]
    async fn allowed_folders_without_a_tool_root_fail_the_check() {
        let dir = copy_pack(&super::super::test_support::fixture_dir(
            "documents_fixture",
        ));
        let experiment_path = dir.path().join("experiment.json");
        let text = std::fs::read_to_string(&experiment_path).unwrap();
        assert!(
            !text.contains("tool_root"),
            "the fixture declares no tool_root"
        );
        std::fs::write(
            &experiment_path,
            text.replacen('{', r#"{"allowed_folders": [{"path": "/tmp"}],"#, 1),
        )
        .unwrap();
        let report = check_dir(dir.path()).await;
        assert!(
            report
                .problems
                .iter()
                .any(|problem| problem.contains("allowed_folders needs init.tool_root")),
            "{:#?}",
            report.problems
        );
    }
}
