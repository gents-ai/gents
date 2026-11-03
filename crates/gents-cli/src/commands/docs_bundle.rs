//! The per-release reference bundle docs.gents.xyz renders: the CLI command
//! tree and the authored configuration schemas, both read from this build.

mod archive;
#[cfg(test)]
mod authored_input_tests;
mod cli_reference;
mod config_reference;
#[cfg(test)]
mod tests;

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result};
use serde::Serialize;

use crate::DocsBundleArgs;

/// Bumped only when a consumer of the bundle must change how it reads it.
const BUNDLE_SCHEMA: u32 = 1;

/// The release the bundle documents. The tag is the source of truth for
/// which docs version a reader sees, so it must name this binary's version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct BundleRelease {
    pub(crate) tag: String,
    pub(crate) version: String,
    pub(crate) commit: String,
}

impl BundleRelease {
    /// Explicit values win over the build stamp so a bundle can be produced
    /// for a tag from a binary built after it.
    pub(crate) fn resolve(tag: Option<String>, commit: Option<String>) -> Result<Self> {
        let build = crate::http::version::version_response().build;
        let version = env!("CARGO_PKG_VERSION").to_owned();
        if commit.is_none() && build.git_dirty == Some(true) {
            anyhow::bail!(
                "this binary was built from a modified checkout; pass --commit to name the commit the bundle documents"
            );
        }
        let tag = tag
            .or_else(|| build.git_tag.map(str::to_owned))
            .with_context(|| {
                format!("this binary was not built at a release tag; pass --tag v{version}")
            })?;
        anyhow::ensure!(
            tag == format!("v{version}"),
            "release tag {tag} does not name this binary's version {version}"
        );
        let commit = commit
            .or_else(|| build.git_sha.map(str::to_owned))
            .context("this binary has no build commit; pass --commit")?;
        anyhow::ensure!(
            commit.len() == 40
                && commit
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
            "commit {commit:?} is not a full lowercase 40-hex SHA"
        );
        Ok(Self {
            tag,
            version,
            commit,
        })
    }
}

#[derive(Serialize)]
struct Manifest<'a> {
    schema: u32,
    #[serde(flatten)]
    release: &'a BundleRelease,
}

/// Bundle files keyed by their path relative to the bundle root.
pub(crate) type BundleFiles = BTreeMap<String, Vec<u8>>;

pub(crate) fn docs_bundle(args: DocsBundleArgs) -> Result<()> {
    let release = BundleRelease::resolve(args.tag, args.commit)?;
    let files = bundle_files(&release)?;
    write_bundle_dir(&args.out, &files)?;
    if let Some(path) = args.archive {
        let bytes = archive::tar_gz(&files)?;
        fs::write(&path, bytes).with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(())
}

pub(crate) fn bundle_files(release: &BundleRelease) -> Result<BundleFiles> {
    let mut files = BundleFiles::new();
    files.insert(
        "manifest.json".into(),
        json_bytes(&Manifest {
            schema: BUNDLE_SCHEMA,
            release,
        })?,
    );
    files.insert(
        "cli.json".into(),
        json_bytes(&cli_reference::cli_reference(&release.version))?,
    );
    let config = config_reference::config_reference();
    let mut index = Vec::with_capacity(config.len());
    for document in config {
        let entry = document.index_entry();
        files.insert(entry.schema.clone(), json_bytes(&document.schema)?);
        index.push(entry);
    }
    files.insert("config/index.json".into(), json_bytes(&index)?);
    Ok(files)
}

fn json_bytes<T>(value: &T) -> Result<Vec<u8>>
where
    T: Serialize + ?Sized,
{
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// Refuses a directory with entries so a stale file from another build can
/// never be published as part of this one.
fn write_bundle_dir(out: &Path, files: &BundleFiles) -> Result<()> {
    if out.exists() {
        let mut entries =
            fs::read_dir(out).with_context(|| format!("reading {}", out.display()))?;
        anyhow::ensure!(
            entries.next().is_none(),
            "output directory {} is not empty",
            out.display()
        );
    }
    for (relative, bytes) in files {
        let path = out.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| format!("creating {}", parent.display()))?;
        }
        fs::write(&path, bytes).with_context(|| format!("writing {}", path.display()))?;
    }
    Ok(())
}
