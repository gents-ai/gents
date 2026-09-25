//! The baseline subject pack and the candidate packs derived from it.
//!
//! A candidate is the baseline pack with exactly one field changed: the system
//! prompt of the context the subject behavior names. When the pack keeps that
//! prompt in a sidecar asset — the shape every pack in this repository uses —
//! the change is one file's bytes and nothing else, which is what makes the
//! structural gate's "only the target moved" check a file comparison.
//!
//! Both packs are ordinary directory packs, so the runner takes them through
//! `CellSource::Directory` with no special case. They are loaded and digested
//! by the runner's own pack loader, so the digest the optimizer records is the
//! digest the runner freezes.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde_json::Value;

use crate::document_config::PackConfig;
use crate::eval::runner::freeze::{load_pack, write_pack_files};
use crate::eval::runner::CellSource;
use crate::pack::{declared_paths, interpolate, PackManifest};

/// The canonical config bundle a sidecar reference or an inline prompt lives in.
const CONFIG_ASSET: &str = "pack_config.json";

/// One arm's pack: where it is, what it digests to, what it declares, and the
/// bytes that digest covers.
#[derive(Clone, Debug)]
pub struct MaterializedPack {
    pub dir: PathBuf,
    pub digest: String,
    pub config: PackConfig,
    pub manifest: PackManifest,
    /// Every declared asset, keyed by its path relative to `dir`.
    pub files: BTreeMap<String, Vec<u8>>,
    /// The context the subject behavior names.
    pub context_id: String,
    /// The declared asset the context reads its system prompt from, when the
    /// pack stores it as a sidecar rather than inline.
    pub prompt_asset: Option<String>,
}

/// Read the pack at `dir` as the subject of `behavior_id`.
pub fn materialize_pack(dir: &Path, owner: &str, behavior_id: &str) -> Result<MaterializedPack> {
    let pack = load_pack(&CellSource::Directory(dir.to_path_buf()), owner)
        .with_context(|| format!("loading pack {}", dir.display()))?;

    let context_id = pack
        .config
        .agent_behaviors
        .iter()
        .find(|behavior| behavior.behavior_id == behavior_id)
        .with_context(|| format!("pack declares no behavior {behavior_id:?}"))?
        .context_id
        .clone()
        .with_context(|| format!("behavior {behavior_id:?} names no context to optimize"))?;

    let prompt_asset = sidecar_prompt_asset(&pack.manifest, &pack.files, &context_id)?;

    Ok(MaterializedPack {
        dir: dir.to_path_buf(),
        digest: pack.digest,
        config: pack.config,
        manifest: pack.manifest,
        files: pack.files,
        context_id,
        prompt_asset,
    })
}

/// The declared asset `context_id`'s `system_prompt` points at, when it points
/// at one. The raw `pack_config.json` is read rather than the loaded
/// [`PackConfig`], because the loader resolves a sidecar reference into the
/// text it holds and the reference itself is what has to be rewritten.
fn sidecar_prompt_asset(
    manifest: &PackManifest,
    files: &BTreeMap<String, Vec<u8>>,
    context_id: &str,
) -> Result<Option<String>> {
    let Some(bytes) = files.get(CONFIG_ASSET) else {
        return Ok(None);
    };
    let raw: Value = serde_json::from_slice(bytes).context("parsing pack_config.json")?;
    let Some(reference) = raw["contexts"]
        .as_array()
        .into_iter()
        .flatten()
        .find(|context| context["context_id"].as_str() == Some(context_id))
        .and_then(|context| context["system_prompt"].as_str())
    else {
        return Ok(None);
    };
    let normalized = reference.trim_start_matches("./");
    Ok(declared_paths(manifest)
        .into_iter()
        .find(|path| path == normalized))
}

/// The subject behavior's current system prompt.
pub fn baseline_text(pack: &MaterializedPack) -> Result<String> {
    if let Some(path) = &pack.prompt_asset {
        let bytes = pack
            .files
            .get(path)
            .with_context(|| format!("pack has no asset {path:?}"))?;
        return String::from_utf8(bytes.clone())
            .with_context(|| format!("asset {path:?} is not UTF-8"));
    }
    Ok(pack
        .config
        .contexts
        .iter()
        .find(|context| context.context_id == pack.context_id)
        .and_then(|context| context.system_prompt.clone())
        .unwrap_or_default())
}

/// Write the baseline's declared assets into `dir` with the subject behavior's
/// system prompt replaced by `text`, and read the result back as a pack.
///
/// A sidecar asset holds `text` byte for byte, since sidecars are never
/// interpolated. An inline prompt is written in its escaped form
/// ([`interpolate::escape`]) so that the loader reads back exactly `text`.
///
/// `dir` is created; an existing one is refused rather than written over, so a
/// round can never evaluate a directory another round left behind.
pub fn materialize_candidate(
    baseline: &MaterializedPack,
    owner: &str,
    text: &str,
    dir: &Path,
) -> Result<MaterializedPack> {
    anyhow::ensure!(
        !dir.exists(),
        "candidate directory {} already exists",
        dir.display()
    );
    let mut files = baseline.files.clone();
    match &baseline.prompt_asset {
        Some(path) => {
            files.insert(path.clone(), text.as_bytes().to_vec());
        }
        None => {
            let bytes = files
                .get(CONFIG_ASSET)
                .with_context(|| format!("pack has no asset {CONFIG_ASSET:?}"))?;
            let mut raw: Value =
                serde_json::from_slice(bytes).context("parsing pack_config.json")?;
            let context = raw["contexts"]
                .as_array_mut()
                .into_iter()
                .flatten()
                .find(|context| {
                    context["context_id"].as_str() == Some(baseline.context_id.as_str())
                })
                .with_context(|| format!("pack declares no context {:?}", baseline.context_id))?;
            // The loader interpolates every string in pack_config.json, so
            // the text is written escaped to be read back as exactly itself.
            context["system_prompt"] = Value::String(interpolate::escape(text));
            files.insert(CONFIG_ASSET.to_owned(), serde_json::to_vec_pretty(&raw)?);
        }
    }
    write_pack_files(dir, &files)?;
    let behavior_id = baseline
        .config
        .agent_behaviors
        .iter()
        .find(|behavior| behavior.context_id.as_deref() == Some(baseline.context_id.as_str()))
        .map(|behavior| behavior.behavior_id.clone())
        .with_context(|| format!("no behavior names context {:?}", baseline.context_id))?;
    materialize_pack(dir, owner, &behavior_id)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use serde_json::json;

    const OWNER: &str = "did:key:subject-owner";
    pub(crate) use crate::eval::runner::freeze::tests::FIXTURE_PROMPT;

    /// The eval runner's own fixture pack: a manifest, a README, the canonical
    /// config bundle and one behavior sidecar holding [`FIXTURE_PROMPT`].
    pub(crate) fn write_fixture_pack(root: &Path) {
        crate::eval::runner::freeze::tests::write_fixture_pack(root, "Off");
    }

    #[test]
    fn a_candidate_is_the_baseline_pack_with_one_file_rewritten() {
        let dirs = tempfile::tempdir().unwrap();
        let baseline_dir = dirs.path().join("baseline");
        write_fixture_pack(&baseline_dir);
        let baseline = materialize_pack(&baseline_dir, OWNER, "monitor").unwrap();

        assert_eq!(baseline.context_id, "monitor-context");
        assert_eq!(
            baseline.prompt_asset.as_deref(),
            Some("agent_behaviors/monitor/system_prompt.md")
        );
        assert_eq!(baseline_text(&baseline).unwrap(), FIXTURE_PROMPT);
        assert!(baseline.digest.starts_with("sha256:"));

        let candidate_dir = dirs.path().join("candidate");
        let candidate = materialize_candidate(
            &baseline,
            OWNER,
            "Watch the mailbox, and say why.\n",
            &candidate_dir,
        )
        .unwrap();

        assert_eq!(
            baseline.files.keys().collect::<Vec<_>>(),
            candidate.files.keys().collect::<Vec<_>>(),
            "a candidate declares exactly the baseline's assets"
        );
        let differing: Vec<&String> = baseline
            .files
            .iter()
            .filter(|(path, bytes)| candidate.files.get(*path) != Some(*bytes))
            .map(|(path, _)| path)
            .collect();
        assert_eq!(differing, vec!["agent_behaviors/monitor/system_prompt.md"]);
        assert_eq!(
            baseline_text(&candidate).unwrap(),
            "Watch the mailbox, and say why.\n"
        );
        assert_ne!(
            candidate.digest, baseline.digest,
            "one changed byte is a new pack"
        );
        assert!(candidate_dir.join("manifest.json").exists());
    }

    #[test]
    fn the_same_text_materializes_to_the_same_digest() {
        let dirs = tempfile::tempdir().unwrap();
        let baseline_dir = dirs.path().join("baseline");
        write_fixture_pack(&baseline_dir);
        let baseline = materialize_pack(&baseline_dir, OWNER, "monitor").unwrap();

        let one = materialize_candidate(&baseline, OWNER, "New text.\n", &dirs.path().join("one"))
            .unwrap();
        let two = materialize_candidate(&baseline, OWNER, "New text.\n", &dirs.path().join("two"))
            .unwrap();
        assert_eq!(
            one.digest, two.digest,
            "the digest is over content, not location"
        );

        let same =
            materialize_candidate(&baseline, OWNER, FIXTURE_PROMPT, &dirs.path().join("same"))
                .unwrap();
        assert_eq!(
            same.digest, baseline.digest,
            "rewriting the prompt with its own text is the baseline pack"
        );
    }

    #[test]
    fn a_behavior_the_pack_does_not_declare_is_an_error() {
        let dirs = tempfile::tempdir().unwrap();
        let baseline_dir = dirs.path().join("baseline");
        write_fixture_pack(&baseline_dir);
        let error = materialize_pack(&baseline_dir, OWNER, "no-such-behavior").unwrap_err();
        assert!(
            format!("{error:#}").contains("no-such-behavior"),
            "{error:#}"
        );
    }

    /// The same pack with the prompt inline in `pack_config.json` and no
    /// sidecar asset, for the structural gate's inline branch.
    pub(crate) fn write_inline_fixture_pack(root: &Path) {
        write_fixture_pack(root);
        std::fs::remove_file(root.join("agent_behaviors/monitor/system_prompt.md")).unwrap();
        let manifest_path = root.join("manifest.json");
        let mut manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&manifest_path).unwrap()).unwrap();
        manifest["assets"] = json!(["README.md", "pack_config.json"]);
        std::fs::write(
            &manifest_path,
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        let config_path = root.join("pack_config.json");
        let mut config: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
        config["contexts"][0]["system_prompt"] = json!(FIXTURE_PROMPT);
        std::fs::write(&config_path, serde_json::to_vec_pretty(&config).unwrap()).unwrap();
    }

    #[test]
    fn an_inline_prompt_is_rewritten_in_the_config_and_nowhere_else() {
        let dirs = tempfile::tempdir().unwrap();
        write_inline_fixture_pack(&dirs.path().join("baseline"));
        let baseline = materialize_pack(&dirs.path().join("baseline"), OWNER, "monitor").unwrap();
        assert_eq!(baseline.prompt_asset, None);
        assert_eq!(baseline_text(&baseline).unwrap(), FIXTURE_PROMPT);
        let candidate =
            materialize_candidate(&baseline, OWNER, "New text.\n", &dirs.path().join("c")).unwrap();
        assert_eq!(baseline_text(&candidate).unwrap(), "New text.\n");
        assert_ne!(candidate.digest, baseline.digest);
    }

    /// An inline prompt is written into `pack_config.json`, which the loader
    /// interpolates; the candidate must still evaluate exactly the text.
    #[test]
    fn an_inline_prompt_with_dollar_signs_loads_back_as_itself() {
        let text = "Home is ${HOME}, pay $$5, and ${GENTS_T21_SURELY_UNSET}.\n";
        assert!(std::env::var("GENTS_T21_SURELY_UNSET").is_err());
        let dirs = tempfile::tempdir().unwrap();
        write_inline_fixture_pack(&dirs.path().join("baseline"));
        let baseline = materialize_pack(&dirs.path().join("baseline"), OWNER, "monitor").unwrap();
        let candidate =
            materialize_candidate(&baseline, OWNER, text, &dirs.path().join("c")).unwrap();
        assert_eq!(baseline_text(&candidate).unwrap(), text);

        let loaded = load_pack(&CellSource::Directory(candidate.dir.clone()), OWNER).unwrap();
        let prompt = loaded
            .config
            .contexts
            .iter()
            .find(|context| context.context_id == candidate.context_id)
            .and_then(|context| context.system_prompt.clone());
        assert_eq!(prompt.as_deref(), Some(text));
    }
}
