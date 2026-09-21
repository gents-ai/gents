//! A pack that is not part of this build and has not been published: a
//! directory being authored somewhere, or a `.tar.gz` sitting on disk.
//!
//! Without this, an out-of-tree pack could only be installed by publishing
//! it to a registry and downloading it back, which makes the registry a
//! required step in authoring a pack rather than a way to distribute one.
//!
//! Nothing here relaxes what a pack has to be. A directory is packed and
//! read back through [`PackArchive`], so a pack installed from a path is
//! held to exactly the rules a downloaded pack is held to and carries the
//! digest `gents pack build` would have given it. Where it came from is
//! reported, never trusted.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use gents::pack_archive::PackArchive;

/// A pack read from the filesystem, with the path it was read from kept for
/// reporting. `PackArchive`'s own `Debug` names the pack and what it carries,
/// never its bytes.
#[derive(Debug)]
pub(crate) struct LocalPack {
    pub(crate) archive: PackArchive,
    pub(crate) digest: String,
    pub(crate) path: PathBuf,
}

/// Whether `package` names a place on the filesystem rather than a pack to
/// look up.
///
/// A bundled name is bare snake_case (`code_review`) and a registry
/// coordinate is `namespace/name`, so a path has to announce itself to keep
/// those three unambiguous. It does when it is an archive by extension, is
/// absolute, is explicitly relative (`./`, `../`), or is a directory that
/// actually holds a `manifest.json`. Anything else is a name, and a name
/// that happens to match a directory with no manifest in the working
/// directory is still a name.
pub(crate) fn local_pack_argument(package: &str) -> Option<PathBuf> {
    classify(std::env::current_dir().ok().as_deref(), package)
}

/// The rule itself, with the directory the bare-name check is made against
/// passed in, so it can be exercised without moving the process.
fn classify(base: Option<&Path>, package: &str) -> Option<PathBuf> {
    let path = Path::new(package);
    let announced = package.ends_with(".tar.gz")
        || package.ends_with(".tgz")
        || path.is_absolute()
        || package.starts_with("./")
        || package.starts_with("../")
        || package == "."
        || package == "..";
    let names_a_pack = || {
        let resolved = base.map_or_else(|| path.to_path_buf(), |base| base.join(path));
        resolved.is_dir() && resolved.join("manifest.json").is_file()
    };
    if announced || names_a_pack() {
        Some(path.to_path_buf())
    } else {
        None
    }
}

/// Reads the pack at `path`, whether it is an unpacked directory or a built
/// `.tar.gz`.
///
/// A path that names nothing is refused here rather than falling through to
/// a registry lookup: someone who typed a path meant that path, and a
/// "pack not found in the registry" error would hide the typo.
pub(crate) fn load(path: &Path) -> Result<LocalPack> {
    let archive = if path.is_dir() {
        PackArchive::read_dir(path)?
    } else {
        anyhow::ensure!(
            path.is_file(),
            "no pack at {}; expected a pack directory or a built .tar.gz",
            path.display()
        );
        let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
        PackArchive::from_bytes(&bytes).with_context(|| format!("reading {}", path.display()))?
    };
    let digest = archive.digest()?;
    Ok(LocalPack {
        archive,
        digest,
        // Absolute where possible, so a report names one place rather than a
        // path that only means something from the directory it was typed in.
        path: path.canonicalize().unwrap_or_else(|_| path.to_path_buf()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_names_and_registry_coordinates_are_not_paths() {
        let empty = tempfile::tempdir().expect("tempdir");
        for package in ["code_review", "acme/shipping_plugins", "mailbox"] {
            assert!(classify(Some(empty.path()), package).is_none(), "{package}");
        }
    }

    #[test]
    fn a_path_announces_itself() {
        for package in [
            "./my_pack",
            "../my_pack",
            "/srv/packs/my_pack",
            "my_pack-0.1.0.tar.gz",
            "dist/my_pack-0.1.0.tgz",
            ".",
        ] {
            assert!(local_pack_argument(package).is_some(), "{package}");
        }
    }

    /// A bare name is only read as a path when the directory it names really
    /// is a pack, so `gents pack install pipeline` run from a checkout that
    /// happens to have a `pipeline/` directory of something else still means
    /// the pack called `pipeline`.
    #[test]
    fn a_bare_name_is_a_path_only_when_it_is_actually_a_pack() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::create_dir(dir.path().join("pipeline")).expect("mkdir");
        assert!(classify(Some(dir.path()), "pipeline").is_none());
        std::fs::write(dir.path().join("pipeline").join("manifest.json"), b"{}").expect("write");
        assert!(classify(Some(dir.path()), "pipeline").is_some());
    }

    #[test]
    fn a_path_that_names_nothing_is_refused_where_it_was_typed() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = dir.path().join("absent_pack");
        let error = load(&missing).expect_err("nothing is there").to_string();
        assert!(error.contains(&missing.display().to_string()), "{error}");
    }
}
