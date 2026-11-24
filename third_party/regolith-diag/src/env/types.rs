//! The plain data [`Env`] exchanges with the engine.
//!
//! Directory listings, file metadata, the open mode, and the
//! capability set. None of it holds a handle or touches the host, so
//! it lives apart from the traits in [`super`] that do.

use std::path::PathBuf;

#[allow(unused_imports)] // referenced only by the doc links below
use super::{Env, WriteFile};

/// How [`Env::open_write`] opens a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WriteMode {
    /// Create, discarding any existing contents. WAL and SSTable
    /// writes, and the write half of every write-then-rename.
    Truncate,
    /// Create if absent, then position at the end. Reopening the
    /// MANIFEST for further appends.
    Append,
    /// Create if absent, keep the contents, and position at the start
    /// with full write access.
    ///
    /// The one thing [`Append`](WriteMode::Append) cannot do is shorten
    /// a file. On Windows an append handle is opened without
    /// `FILE_WRITE_DATA`, which `SetEndOfFile` requires, so
    /// [`WriteFile::set_len`] through one fails with "Access is denied";
    /// and asking for write access alongside append does not help,
    /// because Rust's access-mode mapping lets append win. Manifest
    /// recovery, which trims a torn tail before reopening the log for
    /// appends, is the only caller.
    Update,
}

/// One entry from [`Env::read_dir`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirEntry {
    /// Full path to the entry, not just its file name.
    pub path: PathBuf,
    /// Whether the entry is a directory.
    pub is_dir: bool,
}

impl DirEntry {
    /// The entry's final path component, lossily decoded as UTF-8.
    /// Empty when the path has no final component.
    pub fn file_name(&self) -> String {
        self.path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default()
    }
}

/// What [`Env::metadata`] knows about one path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileMeta {
    /// Size in bytes. Unspecified for a directory.
    pub len: u64,
    /// Whether the entry is a directory.
    pub is_dir: bool,
}

/// Free and total space on the filesystem holding a path, as reported
/// by [`Env::disk_space`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct DiskSpace {
    /// Bytes an unprivileged writer can still allocate.
    pub available_bytes: u64,
    /// Size of the filesystem in bytes.
    pub total_bytes: u64,
    /// Inodes an unprivileged writer can still allocate. Every SSTable,
    /// WAL segment and temporary file takes one.
    pub available_inodes: u64,
    /// Inodes the filesystem has in total. `0` where it does not report
    /// a fixed count.
    pub total_inodes: u64,
}

impl DiskSpace {
    /// Build a report, for an [`Env`] implementation outside this crate.
    pub fn new(
        available_bytes: u64,
        total_bytes: u64,
        available_inodes: u64,
        total_inodes: u64,
    ) -> Self {
        Self {
            available_bytes,
            total_bytes,
            available_inodes,
            total_inodes,
        }
    }
}

/// What a given [`Env`] can actually do.
///
/// regolith reads this at open and adapts: an environment without
/// `hard_link` copies bytes for a checkpoint, and one without
/// `sync_dir` is warned about at open and reported through
/// [`crate::Db::capabilities`] rather than being allowed to claim a
/// durability it does not provide.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Capabilities {
    /// [`Env::hard_link`] creates a real link. `false` makes
    /// checkpoints copy bytes instead.
    pub hard_link: bool,
    /// [`Env::sync_dir`] is real. `false` narrows regolith's crash
    /// guarantee to file contents: a create, rename, or unlink may be
    /// lost even though the bytes survived.
    pub sync_dir: bool,
    /// [`Env::rename`] over an existing entry is atomic across a
    /// crash.
    pub atomic_rename: bool,
    /// [`Env::lock_file`] excludes other *processes*. `false` does not
    /// mean no exclusion at all: every environment regolith ships refuses
    /// a second [`crate::Db`] on one directory inside one process, and
    /// on a single-process target such as wasm that is the whole
    /// guarantee. What `false` says is that a second process is not
    /// excluded, so concurrent access from one is undefined.
    pub file_lock: bool,
    /// [`Env::spawn`] can create a thread.
    pub threads: bool,
    /// [`WriteFile::sync_all`] makes bytes durable.
    pub durable_sync: bool,
}

impl Capabilities {
    /// Nothing available. The starting point for a backend that then
    /// declares only what it can genuinely do.
    pub const fn none() -> Self {
        Self {
            hard_link: false,
            sync_dir: false,
            atomic_rename: false,
            file_lock: false,
            threads: false,
            durable_sync: false,
        }
    }

    /// Everything a POSIX filesystem with threads provides.
    pub const fn posix() -> Self {
        Self {
            hard_link: true,
            sync_dir: true,
            atomic_rename: true,
            file_lock: true,
            threads: true,
            durable_sync: true,
        }
    }

    /// Set [`Capabilities::hard_link`].
    pub const fn with_hard_link(mut self, yes: bool) -> Self {
        self.hard_link = yes;
        self
    }

    /// Set [`Capabilities::sync_dir`].
    pub const fn with_sync_dir(mut self, yes: bool) -> Self {
        self.sync_dir = yes;
        self
    }

    /// Set [`Capabilities::atomic_rename`].
    pub const fn with_atomic_rename(mut self, yes: bool) -> Self {
        self.atomic_rename = yes;
        self
    }

    /// Set [`Capabilities::file_lock`].
    pub const fn with_file_lock(mut self, yes: bool) -> Self {
        self.file_lock = yes;
        self
    }

    /// Set [`Capabilities::threads`].
    pub const fn with_threads(mut self, yes: bool) -> Self {
        self.threads = yes;
        self
    }

    /// Set [`Capabilities::durable_sync`].
    pub const fn with_durable_sync(mut self, yes: bool) -> Self {
        self.durable_sync = yes;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dir_entry_file_name_is_the_last_component() {
        let entry = DirEntry {
            path: PathBuf::from("/db/sst/000007.sst"),
            is_dir: false,
        };
        assert_eq!(entry.file_name(), "000007.sst");
    }

    #[test]
    fn capabilities_builders_flip_one_flag_each() {
        let none = Capabilities::none();
        assert!(!none.hard_link && !none.threads);
        assert!(Capabilities::posix().hard_link);
        assert!(!Capabilities::posix().with_hard_link(false).hard_link);
        assert!(Capabilities::none().with_threads(true).threads);
        assert!(Capabilities::none().with_sync_dir(true).sync_dir);
        assert!(Capabilities::none().with_atomic_rename(true).atomic_rename);
        assert!(Capabilities::none().with_file_lock(true).file_lock);
        assert!(Capabilities::none().with_durable_sync(true).durable_sync);
    }
}
