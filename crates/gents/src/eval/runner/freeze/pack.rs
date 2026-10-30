use std::collections::BTreeMap;
use std::path::Path;

use anyhow::{Context, Result};

use super::documents::inference_binding;
use super::{refused, CellRequest, CellSource, FrozenCell, RunRequest};
use crate::config_client::ConfigAccess;
use crate::documents::PackConfig;
use crate::eval::runner::executor::Isolation;
use crate::eval::{CellSpec, SubjectRef};
use crate::pack::{declared_paths, digest_declared_assets, PackInstallOptions, PackManifest};
use crate::tools::surface::BashMode;

pub(crate) struct LoadedPack {
    pub(crate) digest: String,
    pub(crate) config: PackConfig,
    pub(crate) manifest: PackManifest,
    pub(crate) files: BTreeMap<String, Vec<u8>>,
}

pub(crate) fn load_pack(source: &CellSource, owner: &str) -> Result<LoadedPack> {
    let options = PackInstallOptions {
        node_did: owner.to_owned(),
    };
    let CellSource::Directory(root) = source;
    let manifest: PackManifest = serde_json::from_slice(
        &std::fs::read(root.join("manifest.json"))
            .with_context(|| format!("reading {}/manifest.json", root.display()))?,
    )
    .context("parsing pack manifest")?;
    let present = copyable_files(root)?;
    let asset = |path: &str| {
        present
            .get(path)
            .with_context(|| format!("pack has no asset {path:?}"))
    };
    let digest = digest_declared_assets(&manifest, |path| asset(path).map(Vec::as_slice))?;
    let config = crate::pack::load_pack_config(
        &manifest,
        &options,
        &|path| asset(path).cloned(),
        &|name| std::env::var(name).ok(),
    )?;
    let files = declared_paths(&manifest)
        .into_iter()
        .map(|path| {
            let bytes = asset(&path)?.clone();
            Ok((path, bytes))
        })
        .collect::<Result<BTreeMap<_, _>>>()?;
    Ok(LoadedPack {
        digest,
        config,
        manifest,
        files,
    })
}

fn copyable_files(root: &Path) -> Result<BTreeMap<String, Vec<u8>>> {
    let mut files = BTreeMap::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir)
            .with_context(|| format!("reading pack directory {}", dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            let kind = entry.file_type()?;
            anyhow::ensure!(
                !kind.is_symlink(),
                "pack entry {} is a symlink; a pack carries its own bytes",
                path.display()
            );
            if kind.is_dir() {
                pending.push(path);
                continue;
            }
            let relative = path
                .strip_prefix(root)
                .with_context(|| format!("{} is outside the pack", path.display()))?
                .to_str()
                .with_context(|| format!("pack path {} is not UTF-8", path.display()))?
                .to_owned();
            files.insert(
                relative,
                std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?,
            );
        }
    }
    Ok(files)
}

pub(super) async fn validate_cell(
    access: &ConfigAccess,
    request: &RunRequest,
    cell: &CellRequest,
    isolation: Isolation,
    run_dir: &Path,
) -> Result<(FrozenCell, LoadedPack)> {
    let pack = load_pack(&cell.source, &request.owner)
        .map_err(|error| refused(format!("cell {:?} pack: {error:#}", cell.cell_id)))?;

    if !pack
        .config
        .agents
        .iter()
        .any(|behavior| behavior.agent_id == cell.agent_id)
    {
        return Err(refused(format!(
            "cell {:?} pack has no agent {:?}",
            cell.cell_id, cell.agent_id
        )));
    }

    let inference = inference_binding(
        access,
        &request.owner,
        &cell.cell_id,
        &cell.inference_profile_id,
    )
    .await?;

    let unrestricted = refuse_host_bash(&pack.config, isolation)?;

    Ok((
        FrozenCell {
            spec: CellSpec {
                cell_id: cell.cell_id.clone(),
                label: cell.label.clone(),
                subject: SubjectRef {
                    pack_digest: pack.digest.clone(),
                    agent_id: cell.agent_id.clone(),
                },
                inference_profile_id: cell.inference_profile_id.clone(),
            },
            pack_dir: run_dir.join("cells").join(&cell.cell_id).join("pack"),
            inference,
            tools_unrestricted_bash: unrestricted.is_some(),
        },
        pack,
    ))
}

/// Under [`Isolation::Embedded`] the only host bash mode a pack may grant is
/// [`BashMode::Off`]: an embedded trial shares this host, and `ReadOnly` is an
/// argv allowlist with this host's network and no confining root, not a
/// boundary the executor can enforce. Freezing and resuming both go through
/// here, so a run frozen under a sandboxed executor cannot be resumed into an
/// embedded one and quietly lose the guarantee it was frozen with.
pub(super) fn refuse_host_bash(
    config: &PackConfig,
    isolation: Isolation,
) -> Result<Option<String>> {
    if isolation == Isolation::Embedded {
        if let Some((tools_id, mode)) = host_bash(config) {
            return Err(refused(format!(
                "tools {tools_id:?} grants host bash (mode {mode:?}); an embedded trial shares \
                 this host, so only Off is allowed"
            )));
        }
    }
    Ok(unrestricted_bash(config))
}

fn host_bash(config: &PackConfig) -> Option<(String, BashMode)> {
    config.tools.iter().find_map(|tools| {
        let bash = tools.host.as_ref().and_then(|host| host.bash.as_ref())?;
        (bash.mode != BashMode::Off).then(|| (tools.tools_id.clone(), bash.mode))
    })
}

fn unrestricted_bash(config: &PackConfig) -> Option<String> {
    config
        .tools
        .iter()
        .find(|tools| {
            tools
                .host
                .as_ref()
                .and_then(|host| host.bash.as_ref())
                .is_some_and(|bash| bash.mode == BashMode::Unrestricted)
        })
        .map(|tools| tools.tools_id.clone())
}

pub(super) fn materialize_pack(cell: &FrozenCell, pack: &LoadedPack) -> Result<()> {
    if cell.pack_dir.join("manifest.json").exists() {
        return Ok(());
    }
    let cell_dir = cell
        .pack_dir
        .parent()
        .with_context(|| format!("{} has no cell directory", cell.pack_dir.display()))?;
    let staging = cell_dir.join("pack.tmp");
    for stale in [&staging, &cell.pack_dir] {
        if stale.exists() {
            std::fs::remove_dir_all(stale)
                .with_context(|| format!("removing {}", stale.display()))?;
        }
    }
    std::fs::create_dir_all(&staging).with_context(|| format!("creating {}", staging.display()))?;
    write_pack_files(&staging, &pack.files)?;
    std::fs::rename(&staging, &cell.pack_dir).with_context(|| {
        format!(
            "renaming {} to {}",
            staging.display(),
            cell.pack_dir.display()
        )
    })
}

pub(crate) fn write_pack_files(dir: &Path, files: &BTreeMap<String, Vec<u8>>) -> Result<()> {
    for (path, bytes) in files {
        let destination = dir.join(path);
        if let Some(parent) = destination.parent() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("creating {}", parent.display()))?;
        }
        std::fs::write(&destination, bytes)
            .with_context(|| format!("writing {}", destination.display()))?;
    }
    Ok(())
}
