//! The content-addressed pack store: `{home}/packs/store/sha256/{hex}.pack`.
//!
//! A file lands in the store only after [`read_pack`] verified it, streaming,
//! while its bytes were copied into a staging file beside the destination.
//! The staging file is renamed into place only when the computed digest
//! equals both the file's own header and the digest the caller asked for, so
//! a reader never sees a partial or unverified pack under a digest's name.
//! Because a name is its content, an existing file is never replaced.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{ensure, Context, Result};

use crate::pack_archive::{digest_hex, read_pack, Bounds, PackArchive, PackHeader, EXTENSION};

/// The verified header an unpacked pack keeps beside its files; a dotfile,
/// so it can never be mistaken for a pack asset.
const UNPACKED_HEADER: &str = ".pack-header.json";

/// A home's pack store.
#[derive(Debug, Clone)]
pub struct PackStore {
    root: PathBuf,
}

/// A pack the store holds.
#[derive(Debug, Clone)]
pub struct StoredPack {
    pub header: PackHeader,
    pub path: PathBuf,
}

impl PackStore {
    /// The store of the gents home at `home`.
    pub fn new(home: &Path) -> Self {
        Self {
            root: home.join("packs").join("store").join("sha256"),
        }
    }

    /// Where the pack with `digest` lives, whether or not it is there.
    pub fn path(&self, digest: &str) -> Result<PathBuf> {
        Ok(self
            .root
            .join(format!("{}.{EXTENSION}", digest_hex(digest)?)))
    }

    pub fn contains(&self, digest: &str) -> Result<bool> {
        Ok(self.path(digest)?.is_file())
    }

    /// Verifies `input` as a `.pack` and stores it under its digest. With
    /// `expected`, a pack with any other digest is refused and nothing is
    /// stored.
    pub fn import(&self, input: impl Read, expected: Option<&str>) -> Result<StoredPack> {
        self.import_accepting(input, expected, |_| Ok(()))
    }

    /// The same, refusing the pack when `accept` does, before anything is
    /// stored under its digest.
    pub fn import_accepting(
        &self,
        input: impl Read,
        expected: Option<&str>,
        accept: impl FnOnce(&crate::pack_archive::VerifiedPack) -> Result<()>,
    ) -> Result<StoredPack> {
        if let Some(expected) = expected {
            digest_hex(expected)?;
        }
        std::fs::create_dir_all(&self.root)
            .with_context(|| format!("creating the pack store {}", self.root.display()))?;
        let mut staged = tempfile::Builder::new()
            .prefix(".staging-")
            .tempfile_in(&self.root)
            .with_context(|| format!("staging a pack in {}", self.root.display()))?;
        let verified = {
            // `read_pack` reads the input to its end, so the copy is the whole file.
            let mut tee = TeeReader {
                inner: input,
                copy: io::BufWriter::new(staged.as_file_mut()),
            };
            let verified = read_pack(&mut tee, Bounds::default(), |_, _| Ok(()))?;
            tee.copy.flush().context("writing the staged pack")?;
            verified
        };
        accept(&verified)?;
        let digest = &verified.header.digest;
        if let Some(expected) = expected {
            ensure!(
                digest == expected,
                "asked for pack {expected} but received {digest}; nothing was stored"
            );
        }
        staged
            .as_file()
            .sync_all()
            .context("syncing the staged pack")?;
        let path = self.path(digest)?;
        match staged.persist_noclobber(&path) {
            Ok(_) => {}
            Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => {
                return Err(error.error)
                    .with_context(|| format!("storing the pack at {}", path.display()))
            }
        }
        Ok(StoredPack {
            header: verified.header,
            path,
        })
    }

    /// Stores the `.pack` file at `file`.
    pub fn import_file(&self, file: &Path, expected: Option<&str>) -> Result<StoredPack> {
        self.import_file_accepting(file, expected, |_| Ok(()))
    }

    /// [`Self::import_accepting`] for a file.
    pub fn import_file_accepting(
        &self,
        file: &Path,
        expected: Option<&str>,
        accept: impl FnOnce(&crate::pack_archive::VerifiedPack) -> Result<()>,
    ) -> Result<StoredPack> {
        let input =
            std::fs::File::open(file).with_context(|| format!("opening {}", file.display()))?;
        self.import_accepting(io::BufReader::new(input), expected, accept)
            .with_context(|| format!("{} is not a valid pack", file.display()))
    }

    /// Opens the stored pack `digest`. The first open unpacks it, verified
    /// while streaming, into `{home}/packs/unpacked/{hex}/` (staged, then
    /// renamed into place); every open maps its files from there, so a pack
    /// of any size costs page cache, not memory. [`Self::verify`] checks the
    /// stored file again from scratch.
    pub fn open(&self, digest: &str) -> Result<PackArchive> {
        let hex = digest_hex(digest)?;
        let unpacked = self.unpacked_root().join(hex);
        if !unpacked.is_dir() {
            self.unpack(digest, &unpacked)?;
        }
        let header: PackHeader = serde_json::from_slice(
            &std::fs::read(unpacked.join(UNPACKED_HEADER))
                .with_context(|| format!("reading {}", unpacked.display()))?,
        )
        .context("the unpacked pack's header is not valid")?;
        ensure!(
            header.digest == digest,
            "the unpacked pack at {} holds {}, not {digest}",
            unpacked.display(),
            header.digest
        );
        let manifest = serde_json::from_slice(
            &std::fs::read(unpacked.join("manifest.json")).context("reading manifest.json")?,
        )
        .context("the unpacked manifest is not valid")?;
        PackArchive::from_unpacked(&unpacked, header, manifest)
    }

    fn unpacked_root(&self) -> PathBuf {
        self.root.parent().and_then(Path::parent).map_or_else(
            || self.root.join("unpacked"),
            |packs| packs.join("unpacked"),
        )
    }

    fn unpack(&self, digest: &str, target: &Path) -> Result<()> {
        let path = self.path(digest)?;
        let file = std::fs::File::open(&path).with_context(|| {
            format!(
                "pack {digest} is not in the store at {}",
                self.root.display()
            )
        })?;
        let parent = target.parent().context("unpacked path has no parent")?;
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
        let staging = tempfile::Builder::new()
            .prefix(".unpacking-")
            .tempdir_in(parent)
            .with_context(|| format!("staging an unpacked pack in {}", parent.display()))?;
        let verified = read_pack(
            io::BufReader::new(file),
            Bounds::default(),
            |entry, content| {
                let out = staging.path().join(entry);
                if let Some(dir) = out.parent() {
                    std::fs::create_dir_all(dir)?;
                }
                io::copy(
                    content,
                    &mut io::BufWriter::new(std::fs::File::create(&out)?),
                )?;
                Ok(())
            },
        )
        .with_context(|| format!("the stored pack {} is damaged", path.display()))?;
        ensure!(
            verified.header.digest == digest,
            "the stored pack {} holds {}, not {digest}",
            path.display(),
            verified.header.digest
        );
        std::fs::write(
            staging.path().join("manifest.json"),
            &verified.manifest_bytes,
        )
        .context("writing the unpacked manifest")?;
        std::fs::write(
            staging.path().join(UNPACKED_HEADER),
            serde_json::to_vec(&verified.header)?,
        )
        .context("writing the unpacked header")?;
        match std::fs::rename(staging.path(), target) {
            Ok(()) => {
                // The directory now lives at its final name.
                let _ = staging.keep();
                Ok(())
            }
            // Another open unpacked the same digest first; its copy is identical.
            Err(_) if target.is_dir() => Ok(()),
            Err(error) => {
                Err(error).with_context(|| format!("unpacking into {}", target.display()))
            }
        }
    }

    /// Verifies the stored pack `digest` without holding its assets.
    pub fn verify(&self, digest: &str) -> Result<PackHeader> {
        let path = self.path(digest)?;
        let file = std::fs::File::open(&path).with_context(|| {
            format!(
                "pack {digest} is not in the store at {}",
                self.root.display()
            )
        })?;
        let verified = read_pack(io::BufReader::new(file), Bounds::default(), |_, _| Ok(()))
            .with_context(|| format!("the stored pack {} is damaged", path.display()))?;
        ensure!(
            verified.header.digest == digest,
            "the stored pack {} holds {}, not {digest}",
            path.display(),
            verified.header.digest
        );
        Ok(verified.header)
    }
}

/// Copies every byte read from `inner` into `copy`.
struct TeeReader<R, W> {
    inner: R,
    copy: W,
}

impl<R: Read, W: Write> Read for TeeReader<R, W> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.copy.write_all(&buf[..n])?;
        Ok(n)
    }
}

#[cfg(test)]
mod tests;
