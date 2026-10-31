use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use gents::documents::{EvalDefinition, EvalSplit};
use gents::eval::runner::embedded::EmbeddedHome;
use gents::pack::{
    interpolate, is_valid_pack_name, load_pack_config, PackInstallOptions, PackManifest,
};
use gents::store::config_client::{
    apply_desired_state_plan, read_desired_state_record_in_txn, DesiredStateApplyPlan,
};
use gents::{Collection, ConfigAccess};
use serde_json::json;
use tempfile::TempDir;

use super::dossier::{read_pack, Dossier};
use super::validate::Assembled;

mod recovery;
pub(crate) use recovery::recover_output;

const CONFIG: &str = "pack_config.json";
const README: &str = "README.md";
const GENERATED_BY: &str = "gents eval init";

pub(crate) struct PilotNote {
    pub(crate) run_ids: Vec<String>,
    pub(crate) revised: bool,
}

#[derive(Debug)]
pub(crate) struct Written {
    pub(crate) out: PathBuf,
    pub(crate) pack_name: String,
}

pub(crate) struct Staged {
    dir: TempDir,
    pack_name: String,
}

pub(crate) async fn stage(
    assembled: &Assembled,
    interview_summary: &str,
    subject: &Dossier,
    pilot: Option<&PilotNote>,
) -> Result<Staged, Vec<String>> {
    let definition = &assembled.definition;
    let pack_name = pack_name(&definition.definition_id).map_err(|message| vec![message])?;
    let dir = tempfile::Builder::new()
        .prefix("gents-eval-init-")
        .tempdir()
        .map_err(|error| vec![format!("creating a temporary directory: {error}")])?;
    write_files(
        dir.path(),
        &pack_name,
        assembled,
        interview_summary,
        subject,
        pilot,
    )
    .map_err(|error| vec![format!("writing the definition pack: {error:#}")])?;
    round_trip(dir.path(), definition).await.map_err(|error| {
        vec![format!(
            "the definition pack does not load and install: {error:#}"
        )]
    })?;
    Ok(Staged { dir, pack_name })
}

pub(crate) fn commit(
    staged: Staged,
    out: &Path,
    force: bool,
    gents_home: &Path,
) -> Result<Written> {
    replaceable_destination(out, force, gents_home)?;
    let _output_lock = recovery::lock_output(out)?;
    recovery::recover_locked(out, gents_home)?;
    let replaced = replaceable_destination(out, force, gents_home)?;
    if let Some(parent) = out.parent().filter(|parent| !parent.as_os_str().is_empty()) {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    // The new pack lands complete beside `out` first, inside a directory this
    // call creates exclusively (a copy when the staging directory is on
    // another filesystem), so publishing it is a rename on one filesystem and
    // never exposes a partial pack at `out`. Each step's directory is owned by
    // this call alone and removes itself, so no cleanup can reach a path
    // another process created.
    let landing = private_sibling(out, "new")?;
    let new_pack = landing.path().join("pack");
    if std::fs::rename(staged.dir.path(), &new_pack).is_err() {
        copy_tree(staged.dir.path(), &new_pack)
            .with_context(|| format!("copying the definition pack to {}", new_pack.display()))?;
    }
    // The old pack moves aside, is checked again where no other path names
    // it, and is removed only once the new one is in place; an error between
    // the renames restores it. Only a process killed between the two renames
    // leaves `out` absent; the next writer restores the validated old pack
    // under the same output lock before checking replacement eligibility.
    let aside = match &replaced {
        Some(existing) => {
            let holder = private_sibling(existing, "replaced")?;
            let old_pack = holder.path().join("pack");
            std::fs::rename(existing, &old_pack)
                .with_context(|| format!("moving {} aside", existing.display()))?;
            // Whatever is aside now is what would be deleted: it must still be
            // a pack gents eval init wrote, with nothing else in it.
            let changed = if !is_generated_eval_pack(&old_pack) {
                Some(anyhow::anyhow!(
                    "refusing to replace --out {}: it stopped being a definition pack gents eval init wrote while replacing it",
                    out.display()
                ))
            } else {
                match undeclared_entry(&old_pack) {
                    Ok(None) => None,
                    Ok(Some(extra)) => Some(anyhow::anyhow!(
                        "refusing to replace --out {}: {} appeared while replacing it",
                        out.display(),
                        extra
                            .strip_prefix(&old_pack)
                            .map(|relative| existing.join(relative))
                            .unwrap_or(extra)
                            .display()
                    )),
                    Err(error) => Some(error),
                }
            };
            if let Some(error) = changed {
                restore(
                    existing,
                    holder,
                    &old_pack,
                    "it changed while being replaced",
                )?;
                return Err(error);
            }
            Some((existing, holder, old_pack))
        }
        None => None,
    };
    if let Err(error) = std::fs::rename(&new_pack, out) {
        if let Some((existing, holder, old_pack)) = aside {
            restore(existing, holder, &old_pack, "the new pack failed to land")?;
        }
        return Err(error).with_context(|| {
            format!(
                "moving the definition pack from {} to {}",
                new_pack.display(),
                out.display()
            )
        });
    }
    if let Some((_, holder, _)) = aside {
        let path = holder.path().to_path_buf();
        holder
            .close()
            .with_context(|| format!("removing the replaced pack at {}", path.display()))?;
    }
    Ok(Written {
        out: out.to_path_buf(),
        pack_name: staged.pack_name,
    })
}

fn replaceable_destination(out: &Path, force: bool, gents_home: &Path) -> Result<Option<PathBuf>> {
    match out.symlink_metadata() {
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error).context("inspecting eval output"),
        Ok(_) => {}
    }
    anyhow::ensure!(
        force,
        "{} already exists; pass --force to replace it",
        out.display()
    );
    let user_home = std::env::home_dir();
    let cwd = std::env::current_dir().context("resolving the working directory")?;
    replaceable_out(out, user_home.as_deref(), gents_home, &cwd).map(Some)
}

fn restore(existing: &Path, holder: TempDir, old_pack: &Path, why: &str) -> Result<()> {
    std::fs::rename(old_pack, existing).map_err(|error| {
        let kept = holder.keep();
        anyhow::Error::new(error).context(format!(
            "restoring {} after {why}; the old pack is kept at {}",
            existing.display(),
            kept.join("pack").display()
        ))
    })
}

fn private_sibling(path: &Path, step: &str) -> Result<TempDir> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let prefix = recovery::sibling_name(path, &format!(".{step}-"))?;
    tempfile::Builder::new()
        .prefix(&prefix)
        .tempdir_in(parent)
        .with_context(|| format!("creating a directory beside {}", path.display()))
}

fn replaceable_out(
    out: &Path,
    user_home: Option<&Path>,
    gents_home: &Path,
    cwd: &Path,
) -> Result<PathBuf> {
    anyhow::ensure!(
        !out.symlink_metadata()?.file_type().is_symlink(),
        "refusing to replace --out {}: it is a symbolic link",
        out.display()
    );
    let resolved = crate::overwritable_home(out, user_home)
        .map_err(|error| anyhow::anyhow!("refusing to replace --out: {error:#}"))?
        .with_context(|| format!("{} does not exist", out.display()))?;
    let contains =
        |path: &Path| std::fs::canonicalize(path).is_ok_and(|path| path.starts_with(&resolved));
    anyhow::ensure!(
        !contains(gents_home) && crate::home_state::read_init_config(&resolved)?.is_none(),
        "refusing to replace --out {}: it is or contains a Gents home",
        out.display()
    );
    anyhow::ensure!(
        !contains(cwd),
        "refusing to replace --out {}: it is or contains the working directory",
        out.display()
    );
    anyhow::ensure!(
        is_generated_eval_pack(&resolved),
        "refusing to replace --out {}: it is not a definition pack gents eval init wrote; choose another --out or remove it yourself",
        out.display()
    );
    if let Some(extra) = undeclared_entry(&resolved)? {
        anyhow::bail!(
            "refusing to replace --out {}: {} is not part of the pack gents eval init wrote; move it out or remove --out yourself",
            out.display(),
            extra.display()
        );
    }
    Ok(resolved)
}

fn undeclared_entry(dir: &Path) -> Result<Option<PathBuf>> {
    let manifest: PackManifest = serde_json::from_slice(
        &std::fs::read(dir.join("manifest.json")).context("reading the pack manifest")?,
    )
    .context("parsing the pack manifest")?;
    let declared: std::collections::BTreeSet<PathBuf> = gents::pack::declared_paths(&manifest)
        .into_iter()
        .map(PathBuf::from)
        .collect();
    let mut pending = vec![PathBuf::new()];
    while let Some(relative) = pending.pop() {
        let listed = dir.join(&relative);
        for entry in
            std::fs::read_dir(&listed).with_context(|| format!("listing {}", listed.display()))?
        {
            let entry = entry?;
            let path = relative.join(entry.file_name());
            let file_type = entry.file_type()?;
            if file_type.is_dir() && declared.iter().any(|asset| asset.starts_with(&path)) {
                pending.push(path);
            } else if !(file_type.is_file() && declared.contains(&path)) {
                return Ok(Some(dir.join(path)));
            }
        }
    }
    Ok(None)
}

fn is_generated_eval_pack(dir: &Path) -> bool {
    let Ok((manifest, assets)) = read_pack(dir) else {
        return false;
    };
    manifest
        .metadata
        .authors
        .iter()
        .any(|author| author == GENERATED_BY)
        && load_pack_config(
            &manifest,
            &PackInstallOptions {
                node_did: "did:key:eval-init-replace-check".to_owned(),
            },
            &|path| {
                assets
                    .get(path)
                    .cloned()
                    .with_context(|| format!("pack has no asset {path:?}"))
            },
            &|_| None,
        )
        .is_ok_and(|config| !config.eval_definitions.is_empty())
}

pub(crate) fn refuse_subject_overlap(out: &Path, subject_dir: &Path) -> Result<()> {
    let subject = subject_dir
        .canonicalize()
        .with_context(|| format!("resolving the subject directory {}", subject_dir.display()))?;
    let out_resolved = resolve_through_existing_ancestor(out)
        .with_context(|| format!("resolving --out {}", out.display()))?;
    anyhow::ensure!(
        !out_resolved.starts_with(&subject) && !subject.starts_with(&out_resolved),
        "--out {} overlaps the subject pack at {}; write the eval pack somewhere outside the subject",
        out.display(),
        subject.display()
    );
    Ok(())
}

fn resolve_through_existing_ancestor(path: &Path) -> std::io::Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut existing = absolute.as_path();
    let mut missing = Vec::new();
    loop {
        if existing.symlink_metadata().is_ok() {
            break;
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                missing.push(name.to_owned());
                existing = parent;
            }
            _ => break,
        }
    }
    let mut resolved = existing.canonicalize()?;
    for name in missing.into_iter().rev() {
        match name.to_str() {
            Some(".") => {}
            Some("..") => {
                resolved.pop();
            }
            _ => resolved.push(name),
        }
    }
    Ok(resolved)
}

pub(crate) fn readme(
    assembled: &Assembled,
    interview_summary: &str,
    subject: &Dossier,
    pilot: Option<&PilotNote>,
) -> String {
    let definition = &assembled.definition;
    let mut text = String::new();
    let _ = writeln!(
        text,
        "# {}\n",
        definition
            .title
            .as_deref()
            .unwrap_or(&definition.definition_id)
    );
    let _ = writeln!(
        text,
        "Eval definition `{}` (comparability version {}), drafted by `gents eval init` on {} for agent `{}` of pack `{}` ({}), bound to inference slot {}.\n",
        definition.definition_id,
        definition.comparability_version,
        chrono::Utc::now().format("%Y-%m-%d"),
        subject.agent_id,
        subject.pack_name,
        subject.pack_digest,
        definition
            .subject
            .inference_slots
            .iter()
            .map(|slot| format!("`{slot}`"))
            .collect::<Vec<_>>()
            .join(", ")
    );
    text.push_str("## What the author was told\n\n");
    text.push_str(interview_summary.trim_end());
    text.push_str("\n\n## Cases\n\n");
    text.push_str(&case_table(definition));
    if let Some(pilot) = pilot {
        text.push_str("\n## Pilot\n\n");
        let _ = writeln!(
            text,
            "Piloted once at one trial per case, one run per populated split: {}.",
            pilot
                .run_ids
                .iter()
                .map(|run_id| format!("`{run_id}`"))
                .collect::<Vec<_>>()
                .join(", ")
        );
        if pilot.revised {
            text.push_str(
                "\nThe draft was revised after the pilot; the revised cases were not piloted again.\n",
            );
        }
    }
    text
}

pub(crate) fn case_table(definition: &EvalDefinition) -> String {
    let mut text = String::from("| case | split | stages | checks |\n|---|---|---|---|\n");
    for case in &definition.cases {
        let mut checks: Vec<&str> = case
            .stages
            .iter()
            .flat_map(|stage| stage.checks.iter().map(|check| check.check.as_str()))
            .collect();
        checks.sort_unstable();
        checks.dedup();
        let _ = writeln!(
            text,
            "| {} | {} | {} | {} |",
            case.case_id,
            split_name(case.split),
            case.stages.len(),
            checks.join(", ")
        );
    }
    text
}

pub(crate) fn rewrite_readme(
    out: &Path,
    assembled: &Assembled,
    interview_summary: &str,
    subject: &Dossier,
    pilot: &PilotNote,
) -> Result<()> {
    let path = out.join(README);
    std::fs::write(
        &path,
        readme(assembled, interview_summary, subject, Some(pilot)),
    )
    .with_context(|| format!("writing {}", path.display()))
}

fn pack_name(definition_id: &str) -> Result<String, String> {
    let name = definition_id.replace('-', "_");
    if is_valid_pack_name(&name) {
        Ok(name)
    } else {
        Err(format!(
            "definition_id {definition_id:?} does not name a pack: with `-` read as `_`, it must be a lowercase letter followed by lowercase letters, digits and underscores"
        ))
    }
}

fn case_asset(case_id: &str) -> String {
    format!("cases/{}.json", case_id.replace('-', "_"))
}

pub(crate) fn split_name(split: EvalSplit) -> String {
    serde_json::to_value(split)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn write_files(
    dir: &Path,
    pack_name: &str,
    assembled: &Assembled,
    interview_summary: &str,
    subject: &Dossier,
    pilot: Option<&PilotNote>,
) -> Result<()> {
    let definition = &assembled.definition;
    let mut files: BTreeMap<String, Vec<u8>> = BTreeMap::new();
    let mut references = Vec::new();
    for case in &definition.cases {
        let path = case_asset(&case.case_id);
        anyhow::ensure!(
            !files.contains_key(&path),
            "cases {:?} and another share the sidecar {path}; case ids must differ after `-` is read as `_`",
            case.case_id
        );
        let mut bytes = serde_json::to_vec_pretty(case)?;
        bytes.push(b'\n');
        files.insert(path.clone(), bytes);
        references.push(format!("./{path}"));
    }
    let mut entry = json!({
        "definition_id": definition.definition_id,
        "comparability_version": definition.comparability_version,
        "subject": serde_json::to_value(&definition.subject)?,
    });
    if let Some(title) = &definition.title {
        entry["title"] = json!(title);
    }
    interpolate::escape_values(&mut entry);
    entry["cases"] = json!(references);
    let config = json!({"node": {}, "eval_definitions": [entry]});
    let mut bytes = serde_json::to_vec_pretty(&config)?;
    bytes.push(b'\n');
    files.insert(CONFIG.to_owned(), bytes);
    files.insert(
        README.to_owned(),
        readme(assembled, interview_summary, subject, pilot).into_bytes(),
    );

    let manifest = json!({
        "manifest_version": 1,
        "name": pack_name,
        "version": "1.0.0",
        "description": format!(
            "Eval definition {}, drafted by gents eval init.",
            definition.definition_id
        ),
        "authors": [GENERATED_BY],
        "tags": ["eval"],
        "kind": "documents",
        "assets": files.keys().collect::<Vec<_>>(),
        "config": CONFIG,
    });
    let mut bytes = serde_json::to_vec_pretty(&manifest)?;
    bytes.push(b'\n');
    files.insert("manifest.json".to_owned(), bytes);

    // Validation already refuses case ids that could not name an asset; this
    // keeps every write inside `dir` whatever produced the name.
    if let Some(path) = files
        .keys()
        .find(|path| !gents::pack::is_distributable_asset_path(path))
    {
        anyhow::bail!("refusing to write {path:?}: it is not a pack asset path");
    }
    for (path, bytes) in files {
        let file = dir.join(&path);
        if let Some(parent) = file.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&file, bytes).with_context(|| format!("writing {path}"))?;
    }
    Ok(())
}

async fn round_trip(dir: &Path, expected: &EvalDefinition) -> Result<EvalDefinition> {
    let (manifest, assets) = read_pack(dir)?;
    let home = EmbeddedHome::create_temp("eval-init-roundtrip").await?;
    let result = install_and_read_back(&home, &manifest, &assets, expected).await;
    home.node.shutdown().await;
    result
}

async fn install_and_read_back(
    home: &EmbeddedHome,
    manifest: &PackManifest,
    assets: &BTreeMap<String, Vec<u8>>,
    expected: &EvalDefinition,
) -> Result<EvalDefinition> {
    let owner = home.did().to_owned();
    let config = load_pack_config(
        manifest,
        &PackInstallOptions {
            node_did: owner.clone(),
        },
        &|path| {
            assets
                .get(path)
                .cloned()
                .with_context(|| format!("pack has no asset {path:?}"))
        },
        &|_name| None,
    )?;
    let [loaded] = config.eval_definitions.as_slice() else {
        anyhow::bail!(
            "the pack loads {} eval definitions, not one",
            config.eval_definitions.len()
        );
    };
    let mut as_drafted = expected.clone();
    as_drafted.node_did = owner.clone();
    anyhow::ensure!(
        *loaded == as_drafted,
        "the loaded definition differs from the validated draft"
    );

    gents::ensure_node(&home.node, &owner).await?;
    let access = ConfigAccess::local(home.node.clone());
    let plan = DesiredStateApplyPlan::from_pack_config(&config)?;
    access
        .transact_write("eval.init.round_trip", |txn| {
            let plan = &plan;
            Box::pin(async move { apply_desired_state_plan(txn, plan).await.map(|_| ()) })
        })
        .await
        .context("installing the definition pack into a scratch home")?;
    let definition_id = loaded.definition_id.as_str();
    let installed = access
        .transact_write("eval.init.round_trip_read", |txn| {
            let owner = owner.as_str();
            Box::pin(async move {
                read_desired_state_record_in_txn(
                    txn,
                    Collection::EvalDefinition,
                    owner,
                    definition_id,
                )
                .await
            })
        })
        .await?
        .with_context(|| format!("the scratch home has no eval definition {definition_id:?}"))?;
    let installed: EvalDefinition =
        serde_json::from_value(installed.1).context("decoding the installed definition")?;
    installed.validate()?;
    anyhow::ensure!(
        installed.cases == loaded.cases,
        "the installed definition's cases differ from the pack's"
    );
    Ok(loaded.clone())
}

fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let target = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            std::fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}
