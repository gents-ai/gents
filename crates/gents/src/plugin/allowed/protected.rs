use std::fs::{self, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use same_file::Handle;

static PROTECTED: Mutex<Vec<PathBuf>> = Mutex::new(Vec::new());

pub fn protect(home: &Path) {
    let home = home.canonicalize().unwrap_or_else(|_| home.to_path_buf());
    let paths = [
        super::file(&home),
        super::lock_file(&home),
        crate::plugin::approval::lock_file(&home),
        home.join(crate::plugin::approval::workspace::FILE),
        home.join(crate::plugin::approval::workspace::LOCK),
        home.join(crate::home::PLUGIN_APPROVALS_DIR_NAME),
    ];
    if let Ok(mut protected) = PROTECTED.lock() {
        for path in paths {
            if !protected.contains(&path) {
                protected.push(path);
            }
        }
    }
}

/// Missing targets have already had their existing ancestors canonicalized
/// by the path owner. Object comparisons retain both handles until the
/// comparison finishes, including on filesystems that reuse closed file IDs.
pub fn is_protected(path: &Path) -> bool {
    let Ok(paths) = PROTECTED.lock().map(|paths| paths.clone()) else {
        return true;
    };
    inspect(&paths, path).unwrap_or(true)
}

pub(super) fn inspect(paths: &[PathBuf], path: &Path) -> io::Result<bool> {
    if paths.iter().any(|guarded| path.starts_with(guarded)) {
        return Ok(true);
    }
    let canonical = match path.canonicalize() {
        Ok(path) => path,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(error),
    };
    if paths.iter().any(|guarded| canonical.starts_with(guarded)) {
        return Ok(true);
    }
    if fs::metadata(&canonical)?.is_dir() {
        return Ok(false);
    }
    let target = regular_file(&canonical)?;
    let mut remaining = paths.to_vec();
    while let Some(guarded) = remaining.pop() {
        let metadata = match fs::symlink_metadata(&guarded) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        if metadata.is_dir() {
            if let Some(entries) = optional_directory(&guarded)? {
                for entry in entries {
                    remaining.push(entry?.path());
                }
            }
        } else {
            if metadata.is_symlink() && fs::metadata(&guarded)?.is_dir() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "operator state contains a directory link",
                ));
            }
            match regular_file(&guarded) {
                Ok(current) if current == target => return Ok(true),
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
    }
    Ok(false)
}

pub(super) fn optional_directory(path: &Path) -> io::Result<Option<fs::ReadDir>> {
    match fs::read_dir(path) {
        Ok(entries) => Ok(Some(entries)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

fn regular_file(path: &Path) -> io::Result<Handle> {
    if !fs::metadata(path)?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "operator file identity is unavailable",
        ));
    }
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NONBLOCK);
    }
    let file = match options.open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::PermissionDenied => {
            options.read(false).write(true).open(path)?
        }
        Err(error) => return Err(error),
    };
    if !file.metadata()?.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "operator file changed during inspection",
        ));
    }
    Handle::from_file(file)
}
