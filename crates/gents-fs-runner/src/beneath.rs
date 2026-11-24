//! Filesystem access beneath a tool root through directory handles (Lean
//! `PathNamespace`). The root is opened once; every component is looked up in
//! the handle the previous one returned without following a link there, so a
//! concurrent rename or link swap cannot redirect an operation out of the
//! root. Links are read and resolved again beneath the root.

pub mod decision;
mod tree;

pub use tree::{Entry, Kind, LocateTarget, OpenRoot, Walk};

use std::collections::VecDeque;
use std::ffi::{OsStr, OsString};
use std::fs::File;
use std::io::{Read as _, Write as _};
use std::os::fd::OwnedFd;
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};
use std::path::{Component as PathComponent, Path, PathBuf};
use std::sync::Arc;

use rustix::fs::{FileType, Mode, OFlags};
use rustix::io::Errno;

use decision::{step, Component, Observed, Step, StepInput};

/// Links one resolution may follow, as the kernel's own limit for a path.
const MAX_LINK_HOPS: usize = 40;

#[derive(Debug, thiserror::Error)]
pub enum BeneathError {
    #[error("path is outside the allowed tool root {}: {}", root.display(), path.display())]
    Outside { root: PathBuf, path: PathBuf },
    #[error("path is not a directory: {}", path.display())]
    NotADirectory { path: PathBuf },
    #[error("path is not a file: {}", path.display())]
    NotAFile { path: PathBuf },
    #[error("resolving path {}: no such file or directory; list_files its parent directory to find the right name", path.display())]
    NotFound { path: PathBuf },
    #[error("{} is a symbolic link whose target does not exist; name the target path itself to create it", path.display())]
    DanglingLink { path: PathBuf },
    #[error("too many symbolic links resolving {}", path.display())]
    Loop { path: PathBuf },
    #[error("{} changed while it was being opened; repeat the call to resolve it again", path.display())]
    Changed { path: PathBuf },
    #[error("resolving path {}: {source}", path.display())]
    Io {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
}

impl BeneathError {
    fn io(path: &Path, source: impl Into<std::io::Error>) -> Self {
        Self::Io {
            path: path.to_owned(),
            source: source.into(),
        }
    }

    /// Whether the refusal is that the path names nothing.
    pub fn is_not_found(&self) -> bool {
        matches!(self, Self::NotFound { .. })
            || matches!(self, Self::Io { source, .. } if source.kind() == std::io::ErrorKind::NotFound)
    }
}

/// One component of a path below the root.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Comp {
    Name(OsString),
    Parent,
}

/// Whether an operation reads an existing target or may create it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Intent {
    Existing,
    Create,
}

/// How an operation opens a located file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Access {
    Read,
    ReadWrite,
    /// Create the file and its missing directories, or truncate it.
    Replace,
}

/// Called with the held directory and the name before each lookup the walk
/// makes, so a test can change the namespace at that point.
#[cfg(feature = "test-fixtures")]
pub type LookupHook = Arc<dyn Fn(&Path, &OsStr) + Send + Sync>;

/// An open tool root and its canonical path.
#[derive(Clone)]
pub struct RootDir {
    fd: Arc<OwnedFd>,
    path: Arc<PathBuf>,
    max_hops: usize,
    #[cfg(feature = "test-fixtures")]
    hook: Option<LookupHook>,
}

impl std::fmt::Debug for RootDir {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RootDir").field("path", &self.path).finish()
    }
}

struct Held {
    fd: Arc<OwnedFd>,
    name: Option<OsString>,
}

enum Found {
    Directory(OwnedFd),
    Other,
    Missing,
    Link(Vec<Comp>),
}

impl RootDir {
    /// Opens the root by its canonical path. The root's own path is the
    /// operator's choice; only what lies beneath it is resolved by handle.
    pub fn open(path: &Path) -> Result<Self, BeneathError> {
        let canonical = std::fs::canonicalize(path).map_err(|source| BeneathError::Io {
            path: path.to_owned(),
            source,
        })?;
        let fd = rustix::fs::openat(rustix::fs::CWD, &canonical, dir_flags(), Mode::empty())
            .map_err(|errno| BeneathError::io(&canonical, errno))?;
        Ok(Self {
            fd: Arc::new(fd),
            path: Arc::new(canonical),
            max_hops: MAX_LINK_HOPS,
            #[cfg(feature = "test-fixtures")]
            hook: None,
        })
    }

    #[cfg(feature = "test-fixtures")]
    pub fn with_lookup_hook(mut self, hook: LookupHook) -> Self {
        self.hook = Some(hook);
        self
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    /// This root with no link followed beneath it: a link anywhere on a
    /// resolved path is refused (`PathNamespace.step` with no hop left).
    pub fn without_links(mut self) -> Self {
        self.max_hops = 0;
        self
    }

    /// Resolves `requested` (absolute, or relative to `base`, a directory
    /// beneath the root) to its target beneath the root.
    pub fn locate(
        &self,
        requested: &Path,
        base: &Path,
        intent: Intent,
    ) -> Result<Located, BeneathError> {
        let shown = if requested.is_absolute() {
            requested.to_path_buf()
        } else {
            base.join(requested)
        };
        let components = self.components_below(&shown)?;
        self.locate_components(components, intent, &shown)
    }

    /// Resolves a path given relative to the root.
    pub fn locate_relative(
        &self,
        relative: &Path,
        intent: Intent,
    ) -> Result<Located, BeneathError> {
        self.locate(relative, &self.path.clone(), intent)
    }

    /// The components below the root that an absolute path names. A path
    /// that does not name the root literally is taken through the canonical
    /// form of its deepest existing ancestor; that only chooses which path
    /// beneath the root the walk then resolves by handle.
    fn components_below(&self, path: &Path) -> Result<Vec<Comp>, BeneathError> {
        if let Ok(rest) = path.strip_prefix(&*self.path) {
            return Ok(components(rest));
        }
        for ancestor in path.ancestors() {
            let Ok(canonical) = std::fs::canonicalize(ancestor) else {
                continue;
            };
            let below = path.strip_prefix(ancestor).unwrap_or(Path::new(""));
            return match canonical.strip_prefix(&*self.path) {
                Ok(rest) => Ok(components(&rest.join(below))),
                Err(_) => Err(self.outside(path)),
            };
        }
        Err(self.outside(path))
    }

    fn outside(&self, path: &Path) -> BeneathError {
        BeneathError::Outside {
            root: (*self.path).clone(),
            path: path.to_owned(),
        }
    }

    fn locate_components(
        &self,
        components: Vec<Comp>,
        intent: Intent,
        shown: &Path,
    ) -> Result<Located, BeneathError> {
        let create = intent == Intent::Create;
        let mut stack = vec![Held {
            fd: Arc::clone(&self.fd),
            name: None,
        }];
        let mut queue: VecDeque<(Comp, bool)> =
            components.into_iter().map(|comp| (comp, false)).collect();
        let mut hops = 0usize;
        while let Some((comp, via_link)) = queue.pop_front() {
            let facts = |component, observed| StepInput {
                component,
                at_root: stack.len() == 1,
                last: queue.is_empty(),
                create,
                via_link,
                hops_left: hops < self.max_hops,
                observed,
            };
            let name = match comp {
                Comp::Parent => match step(facts(Component::Parent, Observed::Missing)) {
                    Step::Ascend => {
                        stack.pop();
                        continue;
                    }
                    refusal => return Err(self.refusal(refusal, shown)),
                },
                Comp::Name(name) => name,
            };
            let held = stack.last().expect("the root is always held");
            self.before_lookup(&stack, &name);
            let (observed, found) = self.observe(&held.fd, &name, shown)?;
            match (step(facts(Component::Name, observed)), found) {
                (Step::Descend, Found::Directory(fd)) => stack.push(Held {
                    fd: Arc::new(fd),
                    name: Some(name),
                }),
                (Step::Land, found) => {
                    let directory = match found {
                        Found::Directory(fd) => Some(Arc::new(fd)),
                        _ => None,
                    };
                    return Ok(self.located(stack, Some(name), Vec::new(), true, directory));
                }
                (Step::LandMissing, _) => {
                    return Ok(self.located(stack, Some(name), Vec::new(), false, None))
                }
                (Step::PendingCreate, _) => {
                    let mut missing = Vec::with_capacity(queue.len());
                    for (comp, _) in queue.drain(..) {
                        match comp {
                            Comp::Name(name) => missing.push(name),
                            Comp::Parent => return Err(self.refusal(Step::RefuseNotFound, shown)),
                        }
                    }
                    return Ok(self.located(stack, Some(name), missing, false, None));
                }
                (Step::FollowRelative, Found::Link(text)) => {
                    hops += 1;
                    prepend(&mut queue, text);
                }
                (Step::FollowFromRoot, Found::Link(text)) => {
                    hops += 1;
                    stack.truncate(1);
                    prepend(&mut queue, text);
                }
                (refusal, _) => return Err(self.refusal(refusal, shown)),
            }
        }
        let held = stack.last().expect("the root is always held");
        let directory = Some(Arc::clone(&held.fd));
        Ok(self.located(stack, None, Vec::new(), true, directory))
    }

    fn located(
        &self,
        stack: Vec<Held>,
        entry: Option<OsString>,
        missing: Vec<OsString>,
        exists: bool,
        directory: Option<Arc<OwnedFd>>,
    ) -> Located {
        let parent = Arc::clone(&stack.last().expect("the root is always held").fd);
        let names = stack.into_iter().filter_map(|held| held.name).collect();
        Located {
            root: self.clone(),
            parent,
            names,
            entry,
            missing,
            exists,
            directory,
        }
    }

    fn refusal(&self, refusal: Step, shown: &Path) -> BeneathError {
        let path = shown.to_owned();
        match refusal {
            Step::RefuseEscape => self.outside(shown),
            Step::RefuseNotDirectory => BeneathError::NotADirectory { path },
            Step::RefuseNotFound => BeneathError::NotFound { path },
            Step::RefuseDanglingLink => BeneathError::DanglingLink { path },
            Step::RefuseLoop => BeneathError::Loop { path },
            _ => BeneathError::Changed { path },
        }
    }

    /// One lookup in `dir` that does not follow a link at `name`.
    fn observe(
        &self,
        dir: &OwnedFd,
        name: &OsStr,
        shown: &Path,
    ) -> Result<(Observed, Found), BeneathError> {
        match rustix::fs::openat(dir, name, dir_flags() | OFlags::NOFOLLOW, Mode::empty()) {
            Ok(fd) => Ok((Observed::Directory, Found::Directory(fd))),
            Err(Errno::NOENT) => Ok((Observed::Missing, Found::Missing)),
            Err(Errno::NOTDIR | Errno::LOOP) => match rustix::fs::readlinkat(dir, name, Vec::new())
            {
                Ok(text) => {
                    let text = PathBuf::from(OsString::from_vec(text.into_bytes()));
                    Ok(self.classify_link(&text))
                }
                Err(Errno::INVAL) => Ok((Observed::NonDirectory, Found::Other)),
                Err(Errno::NOENT) => Ok((Observed::Missing, Found::Missing)),
                Err(errno) => Err(BeneathError::io(shown, errno)),
            },
            Err(errno) => Err(BeneathError::io(shown, errno)),
        }
    }

    fn classify_link(&self, text: &Path) -> (Observed, Found) {
        if !text.is_absolute() {
            return (Observed::LinkRelative, Found::Link(components(text)));
        }
        match self.components_below(text) {
            Ok(below) => (Observed::LinkBeneathRoot, Found::Link(below)),
            Err(_) => (Observed::LinkOutsideRoot, Found::Other),
        }
    }

    #[cfg(feature = "test-fixtures")]
    fn before_lookup(&self, stack: &[Held], name: &OsStr) {
        if let Some(hook) = &self.hook {
            let mut dir = (*self.path).clone();
            dir.extend(stack.iter().filter_map(|held| held.name.as_ref()));
            hook(&dir, name);
        }
    }

    #[cfg(not(feature = "test-fixtures"))]
    fn before_lookup(&self, _stack: &[Held], _name: &OsStr) {}

    #[cfg(feature = "test-fixtures")]
    fn before_child(&self, dir: &Path, name: &OsStr) {
        if let Some(hook) = &self.hook {
            hook(dir, name);
        }
    }

    #[cfg(not(feature = "test-fixtures"))]
    fn before_child(&self, _dir: &Path, _name: &OsStr) {}

    /// The entries of a held directory, without following any of them, read
    /// until `stop` answers true for the number already collected.
    pub fn children(
        &self,
        dir: &OwnedFd,
        shown: &Path,
        mut stop: impl FnMut(usize) -> bool,
    ) -> Result<Vec<Child>, BeneathError> {
        let entries =
            rustix::fs::Dir::read_from(dir).map_err(|errno| BeneathError::io(shown, errno))?;
        let mut children = Vec::new();
        for entry in entries {
            if stop(children.len()) {
                break;
            }
            let entry = entry.map_err(|errno| BeneathError::io(shown, errno))?;
            let name = OsStr::from_bytes(entry.file_name().to_bytes());
            if name == "." || name == ".." {
                continue;
            }
            let kind = match entry.file_type() {
                FileType::Unknown => {
                    match rustix::fs::statat(dir, name, rustix::fs::AtFlags::SYMLINK_NOFOLLOW) {
                        Ok(stat) => FileType::from_raw_mode(stat.st_mode as _),
                        Err(Errno::NOENT) => continue,
                        Err(errno) => return Err(BeneathError::io(shown, errno)),
                    }
                }
                kind => kind,
            };
            children.push(Child {
                name: name.to_owned(),
                kind: match kind {
                    FileType::Directory => ChildKind::Directory,
                    FileType::Symlink => ChildKind::Link,
                    FileType::RegularFile => ChildKind::File,
                    _ => ChildKind::Other,
                },
            });
        }
        Ok(children)
    }

    /// The child directory `name` of a held directory, refused when it is no
    /// longer a directory there.
    pub fn child_dir(&self, dir: &OwnedFd, shown: &Path, name: &OsStr) -> Option<OwnedFd> {
        self.before_child(shown, name);
        rustix::fs::openat(dir, name, dir_flags() | OFlags::NOFOLLOW, Mode::empty()).ok()
    }

    /// The regular file `name` of a held directory, refused when it is no
    /// longer one there.
    pub fn child_file(&self, dir: &OwnedFd, shown: &Path, name: &OsStr) -> Option<File> {
        self.before_child(shown, name);
        open_regular(dir, name, OFlags::RDONLY).ok()
    }
}

/// A directory entry, as the directory listed it.
#[derive(Debug, Clone)]
pub struct Child {
    pub name: OsString,
    pub kind: ChildKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChildKind {
    Directory,
    File,
    Link,
    Other,
}

/// A resolved target beneath the root: the held directory it lies in and its
/// name there. Every operation acts through that handle.
#[derive(Clone)]
pub struct Located {
    root: RootDir,
    parent: Arc<OwnedFd>,
    names: Vec<OsString>,
    entry: Option<OsString>,
    missing: Vec<OsString>,
    exists: bool,
    directory: Option<Arc<OwnedFd>>,
}

impl std::fmt::Debug for Located {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Located")
            .field("path", &self.path())
            .field("exists", &self.exists)
            .finish()
    }
}

impl Located {
    pub fn root(&self) -> &RootDir {
        &self.root
    }

    /// The target's path below the root, through the directories the walk held.
    pub fn relative(&self) -> PathBuf {
        let mut path = PathBuf::new();
        path.extend(&self.names);
        path.extend(&self.entry);
        path.extend(&self.missing);
        path
    }

    pub fn path(&self) -> PathBuf {
        self.root.path().join(self.relative())
    }

    /// Whether the target existed when it was resolved.
    pub fn exists(&self) -> bool {
        self.exists
    }

    pub fn is_dir(&self) -> bool {
        self.directory.is_some()
    }

    /// The handle of a directory target.
    pub fn open_dir(&self) -> Result<Arc<OwnedFd>, BeneathError> {
        match &self.directory {
            Some(directory) => Ok(Arc::clone(directory)),
            None => Err(BeneathError::NotADirectory { path: self.path() }),
        }
    }

    /// Opens the target file. A target that turned into a link since it was
    /// resolved is refused, never followed.
    pub fn open(&self, access: Access) -> Result<File, BeneathError> {
        let (dir, leaf) = self.leaf(access == Access::Replace)?;
        let flags = match access {
            Access::Read => OFlags::RDONLY,
            Access::ReadWrite => OFlags::RDWR,
            Access::Replace => OFlags::WRONLY | OFlags::CREATE | OFlags::TRUNC,
        };
        open_regular(&dir, leaf, flags).map_err(|error| self.opened(error))
    }

    pub fn read(&self) -> Result<Vec<u8>, BeneathError> {
        let mut bytes = Vec::new();
        self.open(Access::Read)?
            .read_to_end(&mut bytes)
            .map_err(|source| BeneathError::io(&self.path(), source))?;
        Ok(bytes)
    }

    /// Creates or replaces the target file's content.
    pub fn write(&self, bytes: &[u8]) -> Result<(), BeneathError> {
        self.open(Access::Replace)?
            .write_all(bytes)
            .map_err(|source| BeneathError::io(&self.path(), source))
    }

    /// Renames the target to `destination`, creating its missing directories.
    /// Neither name is followed if it is a link by then.
    pub fn rename_to(&self, destination: &Located) -> Result<(), BeneathError> {
        let (from_dir, from) = self.leaf(false)?;
        let (to_dir, to) = destination.leaf(true)?;
        rustix::fs::renameat(&*from_dir, from, &*to_dir, to)
            .map_err(|errno| self.opened(std::io::Error::from(errno)))
    }

    /// The held directory and the name an operation acts on, creating the
    /// missing directories before it when `create`.
    fn leaf(&self, create: bool) -> Result<(Arc<OwnedFd>, &OsStr), BeneathError> {
        let Some(entry) = &self.entry else {
            return Err(BeneathError::NotAFile { path: self.path() });
        };
        let Some((leaf, dirs)) = self.missing.split_last() else {
            return Ok((Arc::clone(&self.parent), entry));
        };
        if !create {
            return Err(BeneathError::NotFound { path: self.path() });
        }
        let mut dir = Arc::clone(&self.parent);
        for name in std::iter::once(entry).chain(dirs) {
            match rustix::fs::mkdirat(&*dir, name, Mode::from_raw_mode(0o777)) {
                Ok(()) | Err(Errno::EXIST) => {}
                Err(errno) => return Err(BeneathError::io(&self.path(), errno)),
            }
            dir = Arc::new(
                rustix::fs::openat(&*dir, name, dir_flags() | OFlags::NOFOLLOW, Mode::empty())
                    .map_err(|_| BeneathError::Changed { path: self.path() })?,
            );
        }
        Ok((dir, leaf))
    }

    fn opened(&self, error: std::io::Error) -> BeneathError {
        let path = self.path();
        match error.raw_os_error().map(Errno::from_raw_os_error) {
            Some(Errno::LOOP) => BeneathError::Changed { path },
            Some(Errno::NOENT) => BeneathError::NotFound { path },
            _ if error.kind() == std::io::ErrorKind::InvalidInput => {
                BeneathError::NotAFile { path }
            }
            _ => BeneathError::Io {
                path,
                source: error,
            },
        }
    }
}

fn dir_flags() -> OFlags {
    OFlags::RDONLY | OFlags::DIRECTORY | OFlags::CLOEXEC
}

/// Opens `name` in `dir` without following a link and only when it is a
/// regular file; the non-blocking open keeps a FIFO from stalling the caller.
fn open_regular(dir: &OwnedFd, name: &OsStr, flags: OFlags) -> std::io::Result<File> {
    let fd = rustix::fs::openat(
        dir,
        name,
        flags | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC,
        Mode::from_raw_mode(0o666),
    )?;
    let file = File::from(fd);
    if !file.metadata()?.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "not a regular file",
        ));
    }
    Ok(file)
}

fn components(path: &Path) -> Vec<Comp> {
    path.components()
        .filter_map(|component| match component {
            PathComponent::Normal(name) => Some(Comp::Name(name.to_owned())),
            PathComponent::ParentDir => Some(Comp::Parent),
            PathComponent::CurDir | PathComponent::RootDir | PathComponent::Prefix(_) => None,
        })
        .collect()
}

fn prepend(queue: &mut VecDeque<(Comp, bool)>, text: Vec<Comp>) {
    for comp in text.into_iter().rev() {
        queue.push_front((comp, true));
    }
}
