//! Reading parts of a ZIP container (XLSX, ODS) as owned streams.
//!
//! Each part is opened through its own file handle and inflated on the fly, so
//! a multi-GiB sheet streams with flat memory and the stream owns everything
//! it needs. A part is refused when it is encrypted, uses an unknown
//! compression method, or inflates by more than [`MAX_RATIO`] into more than
//! [`BOMB_FLOOR`] bytes (a decompression bomb); a part read whole is also
//! capped by the caller.
use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use flate2::read::DeflateDecoder;
use zip::{CompressionMethod, ZipArchive};

use crate::Res;

/// The most entries a container may list.
const MAX_ENTRIES: usize = 100_000;
/// An entry that inflates past this many times its stored size...
const MAX_RATIO: u64 = 200;
/// ...and past this many bytes is a decompression bomb.
const BOMB_FLOOR: u64 = 256 * 1024 * 1024;

/// A ZIP container on disk.
pub struct Zip {
    path: PathBuf,
    archive: ZipArchive<File>,
}

/// One part's location inside the container.
struct Part {
    start: u64,
    stored: u64,
    size: u64,
    deflated: bool,
}

impl Zip {
    /// Opens the container at `path`.
    pub fn open(path: &Path) -> Res<Self> {
        let file = File::open(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
        let archive = ZipArchive::new(file).map_err(|e| {
            format!(
                "{} is not a readable ZIP container ({e}); check it was fully written",
                path.display()
            )
        })?;
        if archive.len() > MAX_ENTRIES {
            return Err(format!(
                "{} lists over {MAX_ENTRIES} entries and is refused",
                path.display()
            ));
        }
        Ok(Self {
            path: path.to_path_buf(),
            archive,
        })
    }

    /// Whether the container has a part named `name`.
    pub fn has(&self, name: &str) -> bool {
        self.archive.file_names().any(|n| n == name)
    }

    fn part(&mut self, name: &str) -> Res<Part> {
        let shown = self.path.display().to_string();
        let entry = self.archive.by_name(name).map_err(|_| {
            format!("{shown} has no part named {name}; check it is the right kind of file")
        })?;
        if entry.encrypted() {
            return Err(format!(
                "{shown} is encrypted and cannot be read without its password"
            ));
        }
        let deflated = match entry.compression() {
            CompressionMethod::Stored => false,
            CompressionMethod::Deflated => true,
            _ => {
                return Err(format!(
                    "{shown} uses a compression method that cannot be read"
                ));
            }
        };
        let (stored, size) = (entry.compressed_size(), entry.size());
        if size > BOMB_FLOOR && size / stored.max(1) > MAX_RATIO {
            return Err(format!(
                "part {name} of {shown} inflates far beyond its stored size and is refused as a decompression bomb"
            ));
        }
        let start = entry
            .data_start()
            .ok_or_else(|| format!("part {name} of {shown} cannot be located"))?;
        Ok(Part {
            start,
            stored,
            size,
            deflated,
        })
    }

    /// A stream of the part `name`, ending after its declared size.
    pub fn stream(&mut self, name: &str) -> Res<Box<dyn Read + Send>> {
        let part = self.part(name)?;
        let mut f = File::open(&self.path)
            .map_err(|e| format!("cannot read {}: {e}", self.path.display()))?;
        f.seek(SeekFrom::Start(part.start))
            .map_err(|e| format!("cannot read {}: {e}", self.path.display()))?;
        let raw = f.take(part.stored);
        Ok(if part.deflated {
            // A deflate stream cannot be trusted to stop at the declared size.
            Box::new(DeflateDecoder::new(raw).take(part.size.saturating_add(1)))
        } else {
            Box::new(raw)
        })
    }

    /// The whole of the part `name`, refused when it is over `cap` bytes.
    pub fn read(&mut self, name: &str, cap: u64) -> Res<Vec<u8>> {
        let part = self.part(name)?;
        if part.size > cap {
            return Err(format!(
                "part {name} of {} is {} MiB, over the {} MiB that can be read; it is too large",
                self.path.display(),
                part.size / 1024 / 1024,
                cap / 1024 / 1024
            ));
        }
        let mut out = Vec::with_capacity(usize::try_from(part.size).unwrap_or(0));
        self.stream(name)?
            .take(cap)
            .read_to_end(&mut out)
            .map_err(|e| format!("part {name} of {} is corrupt ({e})", self.path.display()))?;
        Ok(out)
    }
}
