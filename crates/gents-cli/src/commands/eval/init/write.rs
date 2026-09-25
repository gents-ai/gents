//! A validated draft as a definition pack: written to a temporary directory,
//! loaded back through the real pack loader and installed into a scratch
//! embedded home (validation step 7), and only then moved to `--out`.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use gents::config_client::{
    apply_desired_state_plan, read_desired_state_record_in_txn, DesiredStateApplyPlan,
};
use gents::document_config::{EvalDefinition, EvalSplit};
use gents::eval::runner::embedded::EmbeddedHome;
use gents::pack::{
    interpolate, is_valid_pack_name, load_pack_config, PackInstallOptions, PackManifest,
};
use gents::{Collection, ConfigAccess};
use serde_json::{json, Value};
use tempfile::TempDir;

use super::dossier::{read_pack, Dossier};
use super::validate::Assembled;

const CONFIG: &str = "pack_config.json";
const README: &str = "README.md";
/// The manifest author of every pack `write_files` writes; `--force`
/// replaces only such a pack.
const GENERATED_BY: &str = "gents eval init";

/// What a pilot left for the README to record: one run per populated split,
/// and whether the draft was revised after it.
pub(crate) struct PilotNote {
    pub(crate) run_ids: Vec<String>,
    pub(crate) revised: bool,
}

/// A definition pack that landed at `out`.
#[derive(Debug)]
pub(crate) struct Written {
    pub(crate) out: PathBuf,
    pub(crate) pack_name: String,
}

/// A definition pack that passed the round trip, still in its temporary
/// directory.
pub(crate) struct Staged {
    dir: TempDir,
    pack_name: String,
    /// The definition as the loader read it back.
    #[cfg_attr(not(test), allow(dead_code))]
    definition: EvalDefinition,
}

impl Staged {
    #[cfg(test)]
    pub(crate) fn definition(&self) -> &EvalDefinition {
        &self.definition
    }
}

/// Validation step 7: write the pack to a temporary directory, load it
/// through the real loader and install it into a scratch embedded home.
/// Every failure is a message for the author.
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
    let loaded = round_trip(dir.path(), definition).await.map_err(|error| {
        vec![format!(
            "the definition pack does not load and install: {error:#}"
        )]
    })?;
    Ok(Staged {
        dir,
        pack_name,
        definition: loaded,
    })
}

/// Move a staged pack to `out`. An existing `out` is refused unless `force`,
/// which replaces it.
pub(crate) fn commit(
    staged: Staged,
    out: &Path,
    force: bool,
    gents_home: &Path,
) -> Result<Written> {
    let replaced = if out.symlink_metadata().is_ok() {
        anyhow::ensure!(
            force,
            "{} already exists; pass --force to replace it",
            out.display()
        );
        // $HOME first, then the password database, as `gents init` does.
        let user_home = std::env::home_dir();
        let cwd = std::env::current_dir().context("resolving the working directory")?;
        Some(replaceable_out(
            out,
            user_home.as_deref(),
            gents_home,
            &cwd,
        )?)
    } else {
        None
    };
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
    // leaves `out` absent, with both packs in hidden siblings of it.
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

/// Move the pack set aside in `holder` back to `existing`. If that fails the
/// holder is kept, never removed, so the old pack survives where the error
/// names it.
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

/// A hidden directory beside `path`, created exclusively for one step of
/// replacing it and removed with everything in it when dropped.
fn private_sibling(path: &Path, step: &str) -> Result<TempDir> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    tempfile::Builder::new()
        .prefix(&format!(".{name}.{step}-"))
        .tempdir_in(parent)
        .with_context(|| format!("creating a directory beside {}", path.display()))
}

/// The directory `--force` may replace at `out`, resolved canonically: never
/// the filesystem root, the user home or an ancestor of it (refused as
/// `gents init --dangerously-overwrite` refuses them, and when the user home
/// cannot be resolved), a Gents home or an ancestor of one, or an ancestor of
/// the working directory; and only a pack `gents eval init` wrote.
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

/// The first entry under `dir` that is neither a declared pack path nor a
/// directory leading to one; a symbolic link always counts. Replacing
/// `--out` removes the whole tree, so anything placed in a generated pack
/// afterwards (notes, another Gents home) must stop the replacement.
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

/// Whether `dir` loads through the pack loader as a pack `write_files`
/// wrote: its manifest names `gents eval init` as the author and its
/// configuration holds an eval definition.
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
                agent_did: "did:key:eval-init-replace-check".to_owned(),
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

/// Refuses an `--out` that is the subject's directory, lies inside it, or
/// contains it: `--force` replaces `--out` wholesale, and the pack goes into
/// it, so either way the subject would be overwritten. Both paths are
/// compared canonically; a `--out` that does not exist yet is resolved
/// through its nearest existing ancestor.
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

/// `path` made absolute and canonical as far as it exists; the missing tail
/// is appended with `.` dropped and `..` popped.
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
            // `..` or the root: nothing left to strip.
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

/// [`stage`] then [`commit`], with the stage's messages as one error.
#[cfg(test)]
pub(crate) async fn write_pack(
    assembled: &Assembled,
    interview_summary: &str,
    subject: &Dossier,
    out: &Path,
    force: bool,
) -> Result<Written> {
    let staged = stage(assembled, interview_summary, subject, None)
        .await
        .map_err(|messages| anyhow::anyhow!(messages.join("\n")))?;
    commit(staged, out, force, Path::new("/nonexistent/gents-home"))
}

/// The pack's README: what was drafted, for which subject, from what
/// interview, which cases, and the pilot when there was one.
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
        "Eval definition `{}` (comparability version {}), drafted by `gents eval init` on {} for behavior `{}` of pack `{}` ({}), bound to inference slot {}.\n",
        definition.definition_id,
        definition.comparability_version,
        chrono::Utc::now().format("%Y-%m-%d"),
        subject.behavior_id,
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

/// The definition's cases as a markdown table: id, split, stage count and
/// the checks they name. The README holds it; the command prints it.
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

/// Replace the README of the pack at `out` with one that records `pilot`.
/// The manifest already lists README.md, so nothing else changes.
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

/// The pack name a definition id gives: `-` becomes `_`, and the result
/// must be a pack name.
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

/// The sidecar a case is written to. Pack asset names are snake_case, so a
/// kebab-case id's `-` becomes `_`.
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
    // The config's strings are interpolated at install; the author's text
    // is escaped so it installs as written. Sidecars are never interpolated.
    let mut entry = json!({
        "definition_id": definition.definition_id,
        "comparability_version": definition.comparability_version,
        "subject": serde_json::to_value(&definition.subject)?,
    });
    if let Some(title) = &definition.title {
        entry["title"] = json!(title);
    }
    escape_strings(&mut entry);
    entry["cases"] = json!(references);
    let config = json!({"agent_principal": {}, "eval_definitions": [entry]});
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

fn escape_strings(value: &mut Value) {
    match value {
        Value::String(text) => *text = interpolate::escape(text),
        Value::Array(values) => values.iter_mut().for_each(escape_strings),
        Value::Object(values) => values.values_mut().for_each(escape_strings),
        _ => {}
    }
}

/// Load the pack at `dir` as any directory pack loads, check it holds
/// exactly `expected`, install it into a scratch embedded home and read the
/// definition back from there. Returns the definition as loaded.
async fn round_trip(dir: &Path, expected: &EvalDefinition) -> Result<EvalDefinition> {
    let (manifest, assets) = read_pack(dir)?;
    let home = EmbeddedHome::create_temp("eval-init-roundtrip").await?;
    let result = install_and_read_back(&home, &manifest, &assets, expected).await;
    // A dropped node tears itself down unawaited; close the store cleanly on
    // every path before its directory goes. Shutdown reports no error, so
    // the body's result is what returns.
    home.node.shutdown().await;
    result
}

/// [`round_trip`]'s work inside the scratch `home`.
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
            agent_did: owner.clone(),
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
    as_drafted.agent_did = owner.clone();
    anyhow::ensure!(
        *loaded == as_drafted,
        "the loaded definition differs from the validated draft"
    );

    gents::ensure_agent_principal(home.node.as_ref(), &owner).await?;
    let access = ConfigAccess::Local(home.node.clone());
    let plan = DesiredStateApplyPlan::from_pack_config(&config)?;
    access
        .transact("eval.init.round_trip", |txn| {
            let plan = &plan;
            Box::pin(async move { apply_desired_state_plan(txn, plan).await.map(|_| ()) })
        })
        .await
        .context("installing the definition pack into a scratch home")?;
    let definition_id = loaded.definition_id.as_str();
    let installed = access
        .transact("eval.init.round_trip_read", |txn| {
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

#[cfg(test)]
mod tests {
    use gents::eval::checks::CheckRegistry;
    use gents::pack::PackKind;
    use serde_json::json;

    use super::*;
    use crate::commands::eval::init::validate::tests::{dossier, good, OWNER, SLOT};
    use crate::commands::eval::init::validate::{assemble, validate, Floors};

    const SUMMARY: &str = "The canary must record exactly one item.";

    /// A draft that passed validation steps 1 to 6.
    fn validated(draft: &crate::commands::eval::init::draft::Draft) -> Assembled {
        let assembled = assemble(draft, None, OWNER, SLOT).unwrap();
        validate(
            &assembled,
            &CheckRegistry::builtin(),
            &dossier(),
            &Floors { validation_min: 1 },
        )
        .unwrap();
        assembled
    }

    fn files_under(root: &Path) -> Vec<String> {
        let mut files = Vec::new();
        let mut dirs = vec![root.to_path_buf()];
        while let Some(dir) = dirs.pop() {
            for entry in std::fs::read_dir(dir).unwrap() {
                let path = entry.unwrap().path();
                if path.is_dir() {
                    dirs.push(path);
                } else {
                    let relative = path.strip_prefix(root).unwrap();
                    files.push(relative.to_string_lossy().replace('\\', "/"));
                }
            }
        }
        files.sort();
        files
    }

    fn load(dir: &Path) -> EvalDefinition {
        let (manifest, assets) = read_pack(dir).unwrap();
        let config = load_pack_config(
            &manifest,
            &PackInstallOptions {
                agent_did: OWNER.into(),
            },
            &|path| Ok(assets[path].clone()),
            &|_| None,
        )
        .unwrap();
        config.eval_definitions.into_iter().next().unwrap()
    }

    #[tokio::test]
    async fn a_validated_draft_lands_as_exactly_its_pack() {
        let assembled = validated(&good());
        let root = tempfile::tempdir().unwrap();
        let out = root.path().join("nested/canary_eval");

        let written = write_pack(&assembled, SUMMARY, &dossier(), &out, false)
            .await
            .unwrap();
        assert_eq!(written.out, out);
        assert_eq!(written.pack_name, "canary_quality");

        let cases = ["cases/ho_a.json", "cases/train_a.json", "cases/val_a.json"];
        let mut expected: Vec<String> = cases.iter().map(|path| (*path).to_owned()).collect();
        expected.extend(["README.md", "manifest.json", "pack_config.json"].map(String::from));
        expected.sort();
        assert_eq!(files_under(&out), expected);

        let manifest: PackManifest =
            serde_json::from_slice(&std::fs::read(out.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(manifest.name, "canary_quality");
        assert_eq!(manifest.version, "1.0.0");
        assert_eq!(manifest.metadata.kind, PackKind::Documents);
        assert!(manifest.metadata.inference_slots.is_empty());
        let mut assets = expected.clone();
        assets.retain(|path| path != "manifest.json");
        assert_eq!(manifest.metadata.assets, assets);

        let config: serde_json::Value =
            serde_json::from_slice(&std::fs::read(out.join("pack_config.json")).unwrap()).unwrap();
        assert_eq!(config["agent_principal"], json!({}));
        assert_eq!(
            config["eval_definitions"][0]["cases"],
            json!([
                "./cases/train_a.json",
                "./cases/val_a.json",
                "./cases/ho_a.json"
            ])
        );

        let loaded = load(&out);
        assert_eq!(loaded.cases, assembled.definition.cases);
        assert_eq!(loaded.subject.inference_slots, vec![SLOT.to_owned()]);

        let readme = std::fs::read_to_string(out.join("README.md")).unwrap();
        assert!(readme.contains(SUMMARY), "{readme}");
        assert!(
            readme.contains("| val-a | validation | 1 | captured_rows_count |"),
            "{readme}"
        );
        assert!(!readme.contains("## Pilot"), "{readme}");
        assert!(
            readme.contains("for behavior `canary` of pack `eval_canary` (sha256:canary)"),
            "{readme}"
        );
    }

    #[tokio::test]
    async fn an_existing_out_is_replaced_only_with_force_and_only_when_eval_init_wrote_it() {
        let assembled = validated(&good());
        let root = tempfile::tempdir().unwrap();
        let out = root.path().join("canary_eval");
        std::fs::create_dir_all(&out).unwrap();
        std::fs::write(out.join("notes.txt"), "mine").unwrap();

        let error = write_pack(&assembled, SUMMARY, &dossier(), &out, false)
            .await
            .unwrap_err()
            .to_string();
        assert!(error.contains("--force"), "{error}");
        assert!(out.join("notes.txt").exists(), "a refusal leaves out alone");

        let error = format!(
            "{:#}",
            write_pack(&assembled, SUMMARY, &dossier(), &out, true)
                .await
                .unwrap_err()
        );
        assert!(error.contains("not a definition pack"), "{error}");
        assert_eq!(files_under(&out), vec!["notes.txt".to_owned()]);

        std::fs::remove_dir_all(&out).unwrap();
        write_pack(&assembled, SUMMARY, &dossier(), &out, false)
            .await
            .unwrap();
        // Anything placed in a generated pack afterwards stops the
        // replacement, another Gents home above all.
        for extra in ["cases/stale.json", "other-home/init.json"] {
            let path = out.join(extra);
            std::fs::create_dir_all(path.parent().unwrap()).unwrap();
            std::fs::write(&path, "{}").unwrap();
            let error = format!(
                "{:#}",
                write_pack(&assembled, SUMMARY, &dossier(), &out, true)
                    .await
                    .unwrap_err()
            );
            assert!(error.contains("is not part of the pack"), "{error}");
            assert!(path.exists(), "a refusal leaves out alone");
            std::fs::remove_file(&path).unwrap();
        }
        std::fs::remove_dir(out.join("other-home")).unwrap();
        #[cfg(unix)]
        {
            let elsewhere = root.path().join("elsewhere");
            std::fs::create_dir_all(&elsewhere).unwrap();
            std::fs::write(elsewhere.join("keep.txt"), "mine").unwrap();
            std::os::unix::fs::symlink(&elsewhere, out.join("cases/linked")).unwrap();
            let error = format!(
                "{:#}",
                write_pack(&assembled, SUMMARY, &dossier(), &out, true)
                    .await
                    .unwrap_err()
            );
            assert!(error.contains("is not part of the pack"), "{error}");
            std::fs::remove_file(out.join("cases/linked")).unwrap();
            assert!(elsewhere.join("keep.txt").exists());
            std::fs::remove_dir_all(&elsewhere).unwrap();
        }
        // A leftover of an interrupted replace is not this call's to reuse or
        // remove.
        let leftover = root
            .path()
            .join(format!(".canary_eval.new-{}", std::process::id()));
        std::fs::create_dir_all(&leftover).unwrap();
        std::fs::write(leftover.join("theirs.txt"), "theirs").unwrap();
        write_pack(&assembled, SUMMARY, &dossier(), &out, true)
            .await
            .unwrap();
        assert!(out.join("manifest.json").exists());
        assert_eq!(files_under(&leftover), vec!["theirs.txt".to_owned()]);
        std::fs::remove_dir_all(&leftover).unwrap();
        let siblings: Vec<_> = std::fs::read_dir(root.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(siblings, vec![std::ffi::OsString::from("canary_eval")]);
    }

    #[tokio::test]
    async fn force_never_replaces_a_home_the_working_directory_or_their_ancestors() {
        let root = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(root.path()).unwrap();
        let user = root.join("user");
        let gents_home = root.join("state/gents");
        let cwd = root.join("work/project");
        for dir in [&user, &gents_home, &cwd] {
            std::fs::create_dir_all(dir).unwrap();
        }
        // Even a generated pack is refused where one of them lies inside it.
        let pack = root.join("state");
        let staged = stage(&validated(&good()), SUMMARY, &dossier(), None)
            .await
            .unwrap();
        copy_tree(staged.dir.path(), &pack).unwrap();
        let refused = |out: &Path, user_home: Option<&Path>, expected: &str| {
            let error = format!(
                "{:#}",
                replaceable_out(out, user_home, &gents_home, &cwd).unwrap_err()
            );
            assert!(error.contains(expected), "{}: {error}", out.display());
            assert!(out.exists(), "{} left in place", out.display());
        };
        refused(&user, Some(&user), "user home");
        refused(&root, Some(&user), "user home");
        refused(Path::new("/"), Some(&user), "filesystem root");
        refused(&root.join("work"), None, "cannot be determined");
        refused(&pack, Some(&user), "Gents home");
        refused(&gents_home, Some(&user), "Gents home");
        refused(&root.join("work"), Some(&user), "working directory");
        refused(&cwd, Some(&user), "working directory");
    }

    #[test]
    fn an_out_that_overlaps_the_subject_is_refused() {
        let root = tempfile::tempdir().unwrap();
        let subject = root.path().join("packs/subject");
        std::fs::create_dir_all(subject.join("agent_behaviors")).unwrap();
        let refused = |out: &Path| {
            let error = refuse_subject_overlap(out, &subject)
                .expect_err("an overlapping --out is refused")
                .to_string();
            assert!(error.contains("subject"), "{error}");
        };
        // The subject itself, through a detour the canonical path removes.
        refused(&subject);
        refused(&root.path().join("packs/../packs/subject"));
        // Inside it, existing or not.
        refused(&subject.join("agent_behaviors"));
        refused(&subject.join("evals/new"));
        // Containing it.
        refused(&root.path().join("packs"));
        refused(root.path());
        // Beside it is fine, existing or not.
        refuse_subject_overlap(&root.path().join("packs/subject-eval"), &subject).unwrap();
        refuse_subject_overlap(&root.path().join("evals/subject"), &subject).unwrap();
    }

    #[tokio::test]
    async fn the_written_pack_installs_and_reads_back() {
        let mut draft = good();
        // Interpolation markers in the author's text install as written.
        draft.title = Some("Canary ${HOME} quality".into());
        let assembled = validated(&draft);
        let staged = stage(&assembled, SUMMARY, &dossier(), None).await.unwrap();
        assert_eq!(
            staged.definition().title.as_deref(),
            Some("Canary ${HOME} quality")
        );
        let root = tempfile::tempdir().unwrap();
        let out = root.path().join("canary_eval");
        commit(staged, &out, false, Path::new("/nonexistent/gents-home")).unwrap();

        let home = EmbeddedHome::create_temp("init-write").await.unwrap();
        let owner = home.did().to_owned();
        // Every step reports rather than panics, so the node is shut down
        // before any assertion can fail.
        let found = async {
            gents::ensure_agent_principal(home.node.as_ref(), &owner).await?;
            let (manifest, assets) = read_pack(&out)?;
            let config = load_pack_config(
                &manifest,
                &PackInstallOptions {
                    agent_did: owner.clone(),
                },
                &|path| Ok(assets[path].clone()),
                &|_| None,
            )?;
            let plan = DesiredStateApplyPlan::from_pack_config(&config)?;
            let access = ConfigAccess::Local(home.node.clone());
            access
                .transact("cli.eval.init.test_install", |txn| {
                    let plan = &plan;
                    Box::pin(async move { apply_desired_state_plan(txn, plan).await.map(|_| ()) })
                })
                .await?;
            access
                .transact("cli.eval.init.test_read", |txn| {
                    let owner = owner.as_str();
                    Box::pin(async move {
                        read_desired_state_record_in_txn(
                            txn,
                            Collection::EvalDefinition,
                            owner,
                            "canary-quality",
                        )
                        .await
                    })
                })
                .await
        }
        .await;
        home.node.shutdown().await;
        let found = found.unwrap();
        let (_, value) = found.expect("the definition installed");
        assert_eq!(value["definition_id"], "canary-quality");
    }

    #[tokio::test]
    async fn a_round_trip_failure_is_messages_for_the_author() {
        // Validation steps 1 to 6 hold no opinion on the id's spelling; the
        // pack name it gives is the writer's.
        let mut draft = good();
        draft.definition_id = Some("Canary.Quality".into());
        let messages = stage(&validated(&draft), SUMMARY, &dossier(), None)
            .await
            .err()
            .unwrap();
        assert_eq!(messages.len(), 1, "{messages:?}");
        assert!(messages[0].contains("Canary.Quality"), "{messages:?}");

        // A case id that cannot name an asset, if validation were skipped:
        // the writer refuses it before writing anything.
        let mut draft = good();
        draft.cases[0]["case_id"] = json!("Train-A");
        let assembled = assemble(&draft, None, OWNER, SLOT).unwrap();
        let messages = stage(&assembled, SUMMARY, &dossier(), None)
            .await
            .err()
            .unwrap();
        assert_eq!(messages.len(), 1, "{messages:?}");
        assert!(
            messages[0].contains("not a pack asset path")
                && messages[0].contains("cases/Train_A.json"),
            "{messages:?}"
        );
    }

    #[test]
    fn a_case_id_that_escapes_the_pack_writes_nothing() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path().join("staging").join("pack");
        std::fs::create_dir_all(&dir).unwrap();
        let mut draft = good();
        draft.cases[0]["case_id"] = json!("../../../escape");
        let assembled = assemble(&draft, None, OWNER, SLOT).unwrap();
        let error = write_files(
            &dir,
            "canary_quality",
            &assembled,
            SUMMARY,
            &dossier(),
            None,
        )
        .unwrap_err();
        assert!(
            format!("{error:#}").contains("not a pack asset path"),
            "{error:#}"
        );
        assert_eq!(files_under(root.path()), Vec::<String>::new());
    }

    #[test]
    fn the_readme_records_the_pilot_runs_and_a_revision() {
        let assembled = validated(&good());
        let pilot = PilotNote {
            run_ids: vec![
                "canary-quality-pilot-1-train".into(),
                "canary-quality-pilot-1-validation".into(),
                "canary-quality-pilot-1-held_out".into(),
            ],
            revised: false,
        };
        let kept = readme(&assembled, SUMMARY, &dossier(), Some(&pilot));
        assert!(kept.contains("## Pilot"), "{kept}");
        for run_id in &pilot.run_ids {
            assert!(kept.contains(run_id.as_str()), "{kept}");
        }
        assert!(!kept.contains("revised"), "{kept}");

        let revised = readme(
            &assembled,
            SUMMARY,
            &dossier(),
            Some(&PilotNote {
                revised: true,
                ..pilot
            }),
        );
        assert!(revised.contains("revised after the pilot"), "{revised}");
    }
}
