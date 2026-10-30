use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Serialize;

use crate::pack::archive::{pack_dir, PackArchive};
use crate::pack::{declared_paths, PackKind, PackManifest};

pub const PLACEHOLDER_OWNER: &str = "did:key:zPackCheckPlaceholderOwner";

#[derive(Debug, Serialize)]
pub struct CheckReport {
    pub dir: PathBuf,
    pub pack: Option<String>,
    pub digest: Option<String>,
    pub problems: Vec<String>,
    pub unverified: Vec<&'static str>,
}

pub async fn check_dir(dir: &Path) -> CheckReport {
    let mut report = CheckReport {
        dir: dir.to_path_buf(),
        pack: None,
        digest: None,
        problems: Vec::new(),
        unverified: Vec::new(),
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
    // Before the unbuilt early return: a scaffolded pack is linted on its
    // first `check`, not only once its .afb plugins have been built.
    check_ux_plugins(dir, &manifest, &mut report.problems);
    if !unbuilt.is_empty() {
        if let Err(error) = crate::pack::validate_manifest(&manifest.name, &manifest) {
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

    if manifest.metadata.kind == PackKind::Documents {
        match load_config(&archive) {
            Ok(config) => {
                if config
                    .event_sources
                    .iter()
                    .any(|source| source.filter.is_some())
                {
                    report.unverified.push("event_source_filters");
                }
            }
            Err(error) => report.problems.push(format!("{error:#}")),
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
    let mut file = crate::pack::open_pack_asset(dir, "manifest.json")
        .with_context(|| format!("reading {}", path.display()))?;
    let mut bytes = Vec::new();
    std::io::Read::read_to_end(&mut file, &mut bytes)
        .with_context(|| format!("reading {}", path.display()))?;
    serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", path.display()))
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

pub fn load_config(archive: &PackArchive) -> Result<crate::documents::PackConfig> {
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

fn read_pack_text_asset(dir: &Path, path: &str) -> Result<String> {
    let mut file = crate::pack::open_pack_asset(dir, path)?;
    let mut text = String::new();
    std::io::Read::read_to_string(&mut file, &mut text)
        .with_context(|| format!("reading {path}"))?;
    Ok(text)
}

/// The static lint every file-produced UX plugin must pass: what the
/// webview would refuse at load, reported here at the pack instead.
fn check_ux_plugins(dir: &Path, manifest: &PackManifest, problems: &mut Vec<String>) {
    if manifest.metadata.ux.is_empty() {
        return;
    }
    match crate::plugin::ux::lint_pack_dir(dir, &manifest.metadata.ux) {
        Ok(findings) => {
            for (name, findings) in findings {
                if !findings.is_empty() {
                    problems.push(format!(
                        "ux plugin {name:?} is refused by the static lint:\n{}",
                        crate::plugin::ux::lint::describe(&findings)
                    ));
                }
            }
        }
        Err(error) => problems.push(format!("{error:#}")),
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

    #[tokio::test]
    async fn a_scaffolded_minimal_pack_checks_clean_with_a_digest() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("review_toolkit");
        scaffold_minimal(&dir, "review_toolkit");
        let report = check_dir(&dir).await;
        assert!(report.problems.is_empty(), "{:#?}", report.problems);
        assert!(report.unverified.is_empty(), "{:#?}", report.unverified);
        assert!(report.digest.is_some());
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
            crate::pack::archive::pack_dir(&dir).is_err(),
            "pack writer must reject the same external manifest link"
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
