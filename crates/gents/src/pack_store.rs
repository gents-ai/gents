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
        let input =
            std::fs::File::open(file).with_context(|| format!("opening {}", file.display()))?;
        self.import(io::BufReader::new(input), expected)
            .with_context(|| format!("{} is not a valid pack", file.display()))
    }

    /// Opens the stored pack `digest`, verifying it again on the way in: a
    /// file the store holds is still refused if its content no longer
    /// matches its name.
    pub fn open(&self, digest: &str) -> Result<PackArchive> {
        let path = self.path(digest)?;
        let file = std::fs::File::open(&path).with_context(|| {
            format!(
                "pack {digest} is not in the store at {}",
                self.root.display()
            )
        })?;
        let archive = PackArchive::from_reader(io::BufReader::new(file), Bounds::default())
            .with_context(|| format!("the stored pack {} is damaged", path.display()))?;
        ensure!(
            archive.digest() == digest,
            "the stored pack {} holds {}, not {digest}",
            path.display(),
            archive.digest()
        );
        Ok(archive)
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
