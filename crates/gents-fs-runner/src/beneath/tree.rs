//! Whole-tree operations beneath a root (Lean `PathNamespace.treeWalk`). A
//! directory is entered only by a lookup in its held parent that does not
//! follow a link there, and each entry is read, created, unlinked or renamed
//! relative to the held directory it was listed in, so a concurrent swap of
//! any directory in the tree for a link cannot carry the operation out of
//! the root.

use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{Read as _, Write as _};
use std::os::fd::OwnedFd;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rustix::fs::{AtFlags, FileType, Mode, OFlags};
use rustix::io::Errno;

use super::{dir_flags, open_regular, Access, BeneathError, ChildKind, Intent, Located, RootDir};

/// A root an operation acts beneath: one already held, or a directory an
/// operator names by path, opened by that path once when the operation
/// starts.
pub trait OpenRoot {
    fn open_root(self) -> Result<RootDir, BeneathError>;
}

impl OpenRoot for &RootDir {
    fn open_root(self) -> Result<RootDir, BeneathError> {
        Ok(self.clone())
    }
}

impl<P> OpenRoot for &P
where
    P: AsRef<Path> + ?Sized,
{
    fn open_root(self) -> Result<RootDir, BeneathError> {
        RootDir::open(self.as_ref())
    }
}

/// A target an operation creates or acts on: one already resolved, or a
/// path an operator names, whose parent directory is opened by that path
/// once when the operation starts.
pub trait LocateTarget {
    fn locate_target(self, intent: Intent) -> Result<Located, BeneathError>;
}

impl LocateTarget for &Located {
    fn locate_target(self, _intent: Intent) -> Result<Located, BeneathError> {
        Ok(self.clone())
    }
}

impl<P> LocateTarget for &P
where
    P: AsRef<Path> + ?Sized,
{
    fn locate_target(self, intent: Intent) -> Result<Located, BeneathError> {
        Located::at(self.as_ref(), intent)
    }
}

/// What an existing name beneath the root resolves to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Directory,
    File,
    Other,
}

/// One entry a walk reached: listed in a held directory, not followed.
pub struct Entry<'a> {
    root: &'a RootDir,
    dir: &'a OwnedFd,
    name: &'a OsStr,
    relative: &'a Path,
    kind: ChildKind,
}

impl Entry<'_> {
    /// The entry's path below the walked root.
    pub fn relative(&self) -> &Path {
        self.relative
    }

    pub fn name(&self) -> &OsStr {
        self.name
    }

    pub fn kind(&self) -> ChildKind {
        self.kind
    }

    /// The content of a file entry, opened in the directory it was listed
    /// in. A link is resolved again beneath the root; one leading out of it
    /// is refused.
    pub fn read(&self) -> Result<Vec<u8>, BeneathError> {
        let shown = self.root.path().join(self.relative);
        match self.kind {
            ChildKind::File => {
                let mut bytes = Vec::new();
                open_regular(self.dir, self.name, OFlags::RDONLY)
                    .and_then(|mut file| file.read_to_end(&mut bytes))
                    .map_err(|source| match source.raw_os_error().map(Errno::from_raw_os_error) {
                        Some(Errno::LOOP) => BeneathError::Changed { path: shown },
                        _ => BeneathError::io(&self.root.path().join(self.relative), source),
                    })?;
                Ok(bytes)
            }
            ChildKind::Link => self.root.read(self.relative),
            ChildKind::Directory | ChildKind::Other => {
                Err(BeneathError::NotAFile { path: shown })
            }
        }
    }
}

/// Whether a walk enters a directory entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Walk {
    Enter,
    Skip,
}

impl Located {
    /// The target an operator names by path: the path is the operator's
    /// choice, so it is resolved whole, links included; the directory
    /// holding the target is then opened as the root and the last name is
    /// resolved in it. With [`Intent::Create`] that directory is created
    /// first.
    pub fn at(path: &Path, intent: Intent) -> Result<Located, BeneathError> {
        let absolute = match std::fs::canonicalize(path) {
            Ok(canonical) => canonical,
            Err(_) => {
                std::path::absolute(path).map_err(|source| BeneathError::io(path, source))?
            }
        };
        let (Some(parent), Some(name)) = (absolute.parent(), absolute.file_name()) else {
            return RootDir::open(&absolute)?.locate_relative(Path::new(""), intent);
        };
        let root = match intent {
            Intent::Create => RootDir::create(parent)?,
            Intent::Existing => RootDir::open(parent)?,
        };
        root.locate_relative(Path::new(name), intent)
    }

    /// What the target is, without following a link at its name.
    pub fn kind(&self) -> Result<Kind, BeneathError> {
        if self.directory.is_some() {
            return Ok(Kind::Directory);
        }
        if !self.exists {
            return Err(BeneathError::NotFound { path: self.path() });
        }
        let (dir, leaf) = self.leaf(false)?;
        let stat = rustix::fs::statat(&*dir, leaf, AtFlags::SYMLINK_NOFOLLOW)
            .map_err(|errno| self.opened(errno.into()))?;
        Ok(match FileType::from_raw_mode(stat.st_mode as _) {
            FileType::RegularFile => Kind::File,
            FileType::Directory => Kind::Directory,
            FileType::Symlink => return Err(BeneathError::Changed { path: self.path() }),
            _ => Kind::Other,
        })
    }

    /// The directory target as a root of its own: what lies beneath it is
    /// resolved through its handle and confined to it.
    pub fn subroot(&self) -> Result<RootDir, BeneathError> {
        Ok(self.root.below(self.open_dir()?, self.path()))
    }

    /// Creates the directory target and its missing parents through held
    /// handles; an existing directory is kept.
    pub fn create_dir(&self) -> Result<RootDir, BeneathError> {
        if self.directory.is_some() {
            return self.subroot();
        }
        let (dir, leaf) = self.leaf(true)?;
        let fd = make_dir(&dir, leaf, &self.path())?;
        Ok(self.root.below(Arc::new(fd), self.path()))
    }

    /// Removes the target: a file or link is unlinked, a directory is
    /// emptied by handle and then removed. Nothing a link names is touched.
    pub fn remove(&self) -> Result<(), BeneathError> {
        if self.entry.is_none() {
            return Err(BeneathError::Outside {
                root: self.root.path().to_owned(),
                path: self.path(),
            });
        }
        let (dir, leaf) = self.leaf(false)?;
        let parent = self.path();
        let parent = parent.parent().unwrap_or(self.root.path());
        self.root.remove_entry(&dir, parent, leaf)
    }

    /// Removes the directory target when it is empty, through the directory
    /// it was resolved in.
    pub fn remove_empty_dir(&self) -> Result<(), BeneathError> {
        let (dir, leaf) = self.leaf(false)?;
        rustix::fs::unlinkat(&*dir, leaf, AtFlags::REMOVEDIR)
            .map_err(|errno| self.opened(errno.into()))
    }

    /// Writes the target through a staged sibling renamed over it in the
    /// held directory, so a reader sees the old content or the new.
    pub fn replace_with<E>(&self, write: impl FnOnce(&mut File) -> Result<(), E>) -> Result<(), E>
    where
        E: From<BeneathError>,
    {
        let (dir, leaf) = self.leaf(true)?;
        let shown = self.path();
        let mut staged_name = OsString::from(".staging-");
        staged_name.push(leaf);
        staged_name.push(format!("-{}", std::process::id()));
        let fd = rustix::fs::openat(
            &*dir,
            &*staged_name,
            OFlags::WRONLY | OFlags::CREATE | OFlags::TRUNC | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o666),
        )
        .map_err(|errno| BeneathError::io(&shown, errno))?;
        let mut file = File::from(fd);
        let written = write(&mut file).and_then(|()| {
            file.flush()
                .map_err(|source| BeneathError::io(&shown, source))?;
            rustix::fs::renameat(&*dir, &*staged_name, &*dir, leaf)
                .map_err(|errno| E::from(self.opened(errno.into())))
        });
        if written.is_err() {
            let _ = rustix::fs::unlinkat(&*dir, &*staged_name, AtFlags::empty());
        }
        written
    }

    /// The held directory the target's name was resolved in, as a root.
    pub fn parent_root(&self) -> RootDir {
        let parent = self.root.path().join(self.names.iter().collect::<PathBuf>());
        self.root.below(Arc::clone(&self.parent), parent)
    }
}

impl RootDir {
    fn below(&self, fd: Arc<OwnedFd>, path: PathBuf) -> RootDir {
        RootDir {
            fd,
            path: Arc::new(path),
            max_hops: self.max_hops,
            #[cfg(feature = "test-fixtures")]
            hook: self.hook.clone(),
        }
    }

    /// Opens `path` as a root when it is a directory, or creates it first.
    pub fn create(path: &Path) -> Result<Self, BeneathError> {
        std::fs::create_dir_all(path).map_err(|source| BeneathError::io(path, source))?;
        Self::open(path)
    }

    /// The existing target `relative` names below the root.
    pub fn existing(&self, relative: impl AsRef<Path>) -> Result<Located, BeneathError> {
        self.locate_relative(relative.as_ref(), Intent::Existing)
    }

    /// What `relative` names below the root, or nothing when it names nothing.
    pub fn kind(&self, relative: impl AsRef<Path>) -> Result<Option<Kind>, BeneathError> {
        match self.existing(relative).and_then(|located| located.kind()) {
            Ok(kind) => Ok(Some(kind)),
            Err(error) if error.is_not_found() => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub fn is_file(&self, relative: impl AsRef<Path>) -> bool {
        matches!(self.kind(relative), Ok(Some(Kind::File)))
    }

    pub fn is_dir(&self, relative: impl AsRef<Path>) -> bool {
        matches!(self.kind(relative), Ok(Some(Kind::Directory)))
    }

    pub fn exists(&self, relative: impl AsRef<Path>) -> bool {
        matches!(self.kind(relative), Ok(Some(_)))
    }

    pub fn open_file(&self, relative: impl AsRef<Path>) -> Result<File, BeneathError> {
        self.existing(relative)?.open(Access::Read)
    }

    pub fn read(&self, relative: impl AsRef<Path>) -> Result<Vec<u8>, BeneathError> {
        self.existing(relative)?.read()
    }

    pub fn read_to_string(&self, relative: impl AsRef<Path>) -> Result<String, BeneathError> {
        let located = self.existing(relative)?;
        String::from_utf8(located.read()?).map_err(|error| {
            BeneathError::io(
                &located.path(),
                std::io::Error::new(std::io::ErrorKind::InvalidData, error),
            )
        })
    }

    /// Creates or replaces a file, creating its missing directories.
    pub fn write(
        &self,
        relative: impl AsRef<Path>,
        bytes: impl AsRef<[u8]>,
    ) -> Result<(), BeneathError> {
        self.locate_relative(relative.as_ref(), Intent::Create)?
            .write(bytes.as_ref())
    }

    /// Creates a file that must not exist yet, creating its missing
    /// directories.
    pub fn create_new(
        &self,
        relative: impl AsRef<Path>,
        bytes: impl AsRef<[u8]>,
    ) -> Result<(), BeneathError> {
        let located = self.locate_relative(relative.as_ref(), Intent::Create)?;
        if located.exists() {
            return Err(BeneathError::io(
                &located.path(),
                std::io::Error::from(std::io::ErrorKind::AlreadyExists),
            ));
        }
        let (dir, leaf) = located.leaf(true)?;
        let fd = rustix::fs::openat(
            &*dir,
            leaf,
            OFlags::WRONLY | OFlags::CREATE | OFlags::EXCL | OFlags::NOFOLLOW | OFlags::CLOEXEC,
            Mode::from_raw_mode(0o666),
        )
        .map_err(|errno| BeneathError::io(&located.path(), errno))?;
        File::from(fd)
            .write_all(bytes.as_ref())
            .map_err(|source| BeneathError::io(&located.path(), source))
    }

    /// The directory `relative` names, created with its missing parents.
    pub fn create_dir_all(&self, relative: impl AsRef<Path>) -> Result<RootDir, BeneathError> {
        self.locate_relative(relative.as_ref(), Intent::Create)?
            .create_dir()
    }

    /// The existing directory `relative` names, as a root of its own.
    pub fn subdir(&self, relative: impl AsRef<Path>) -> Result<RootDir, BeneathError> {
        self.existing(relative)?.subroot()
    }

    /// Removes what `relative` names; a directory with everything in it.
    pub fn remove(&self, relative: impl AsRef<Path>) -> Result<(), BeneathError> {
        self.existing(relative)?.remove()
    }

    /// Removes what `relative` names when it exists.
    pub fn remove_if_present(&self, relative: impl AsRef<Path>) -> Result<(), BeneathError> {
        match self.remove(relative) {
            Err(error) if error.is_not_found() => Ok(()),
            other => other,
        }
    }

    /// The entries of the root, by name, without following any of them.
    pub fn entries(&self) -> Result<Vec<super::Child>, BeneathError> {
        let mut children = self.children(&self.fd, &self.path.clone(), |_| false)?;
        children.sort_by(|left, right| left.name.cmp(&right.name));
        Ok(children)
    }

    /// Removes every entry of the root, keeping the root itself.
    pub fn clear(&self) -> Result<(), BeneathError> {
        self.clear_dir(&self.fd, &self.path.clone())
    }

    /// Renames `from` to `to` beneath `destination`, each name acted on in
    /// the directory it was resolved to.
    pub fn rename(
        &self,
        from: impl AsRef<Path>,
        destination: &RootDir,
        to: impl AsRef<Path>,
    ) -> Result<(), BeneathError> {
        let from = self.existing(from)?;
        let to = destination.locate_relative(to.as_ref(), Intent::Create)?;
        from.rename_to(&to)
    }

    /// Visits every entry below the root, depth first in listing order. A
    /// directory is entered when `visit` answers [`Walk::Enter`], by a lookup
    /// in its held parent that refuses a link there; a link is reported,
    /// never entered.
    pub fn walk(
        &self,
        mut visit: impl FnMut(&Entry<'_>) -> Result<Walk, BeneathError>,
    ) -> Result<(), BeneathError> {
        self.walk_dir(&self.fd, Path::new(""), &mut visit)
    }

    fn walk_dir(
        &self,
        dir: &OwnedFd,
        relative: &Path,
        visit: &mut dyn FnMut(&Entry<'_>) -> Result<Walk, BeneathError>,
    ) -> Result<(), BeneathError> {
        let shown = self.path.join(relative);
        let mut children = self.children(dir, &shown, |_| false)?;
        children.sort_by(|left, right| left.name.cmp(&right.name));
        for child in children {
            let below = relative.join(&child.name);
            let entry = Entry {
                root: self,
                dir,
                name: &child.name,
                relative: &below,
                kind: child.kind,
            };
            let walk = visit(&entry)?;
            if child.kind == ChildKind::Directory && walk == Walk::Enter {
                let Some(fd) = self.child_dir(dir, &shown, &child.name) else {
                    return Err(BeneathError::Changed {
                        path: self.path.join(&below),
                    });
                };
                self.walk_dir(&fd, &below, visit)?;
            }
        }
        Ok(())
    }

    /// Copies every entry below this root into `destination`, creating each
    /// one relative to the held destination directory. A link is copied as
    /// the file it resolves to beneath this root; one leading out of it, or
    /// to a directory, is refused, as is any entry that is neither a file
    /// nor a directory.
    pub fn copy_into(&self, destination: &RootDir) -> Result<(), BeneathError> {
        self.copy_dir(&self.fd, Path::new(""), destination, &destination.fd)
    }

    fn copy_dir(
        &self,
        from: &OwnedFd,
        relative: &Path,
        destination: &RootDir,
        to: &OwnedFd,
    ) -> Result<(), BeneathError> {
        let shown = self.path.join(relative);
        let shown_to = destination.path.join(relative);
        for child in self.children(from, &shown, |_| false)? {
            let below = relative.join(&child.name);
            match child.kind {
                ChildKind::Directory => {
                    let Some(source) = self.child_dir(from, &shown, &child.name) else {
                        return Err(BeneathError::Changed {
                            path: self.path.join(&below),
                        });
                    };
                    destination.before_child(&shown_to, &child.name);
                    let target = make_dir(to, &child.name, &destination.path.join(&below))?;
                    self.copy_dir(&source, &below, destination, &target)?;
                }
                ChildKind::File | ChildKind::Link => {
                    let bytes = if child.kind == ChildKind::File {
                        self.before_child(&shown, &child.name);
                        let mut bytes = Vec::new();
                        open_regular(from, &child.name, OFlags::RDONLY)
                            .and_then(|mut file| file.read_to_end(&mut bytes))
                            .map_err(|source| BeneathError::io(&self.path.join(&below), source))?;
                        bytes
                    } else {
                        self.read(&below)?
                    };
                    destination.before_child(&shown_to, &child.name);
                    let mut file = open_regular(
                        to,
                        &child.name,
                        OFlags::WRONLY | OFlags::CREATE | OFlags::TRUNC,
                    )
                    .map_err(|source| BeneathError::io(&destination.path.join(&below), source))?;
                    file.write_all(&bytes)
                        .map_err(|source| BeneathError::io(&destination.path.join(&below), source))?;
                }
                ChildKind::Other => {
                    return Err(BeneathError::NotAFile {
                        path: self.path.join(&below),
                    })
                }
            }
        }
        Ok(())
    }

    fn clear_dir(&self, dir: &OwnedFd, shown: &Path) -> Result<(), BeneathError> {
        for child in self.children(dir, shown, |_| false)? {
            self.remove_entry(dir, shown, &child.name)?;
        }
        Ok(())
    }

    /// Removes `name` from the held directory `dir`: a directory found there
    /// by a lookup that does not follow a link is emptied and removed; any
    /// other entry, a link included, is unlinked itself.
    fn remove_entry(&self, dir: &OwnedFd, shown: &Path, name: &OsStr) -> Result<(), BeneathError> {
        let path = shown.join(name);
        match self.child_dir(dir, shown, name) {
            Some(child) => {
                self.clear_dir(&child, &path)?;
                rustix::fs::unlinkat(dir, name, AtFlags::REMOVEDIR)
                    .map_err(|errno| BeneathError::io(&path, errno))
            }
            None => match rustix::fs::unlinkat(dir, name, AtFlags::empty()) {
                Ok(()) | Err(Errno::NOENT) => Ok(()),
                Err(Errno::ISDIR | Errno::PERM) => Err(BeneathError::Changed { path }),
                Err(errno) => Err(BeneathError::io(&path, errno)),
            },
        }
    }
}

/// Creates the directory `name` in the held directory `dir`, keeping one
/// that exists, and opens it without following a link there.
fn make_dir(dir: &OwnedFd, name: &OsStr, shown: &Path) -> Result<OwnedFd, BeneathError> {
    match rustix::fs::mkdirat(dir, name, Mode::from_raw_mode(0o777)) {
        Ok(()) | Err(Errno::EXIST) => {}
        Err(errno) => return Err(BeneathError::io(shown, errno)),
    }
    rustix::fs::openat(dir, name, dir_flags() | OFlags::NOFOLLOW, Mode::empty()).map_err(
        |errno| match errno {
            Errno::NOTDIR | Errno::LOOP => BeneathError::NotADirectory {
                path: shown.to_owned(),
            },
            errno => BeneathError::io(shown, errno),
        },
    )
}
