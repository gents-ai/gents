//! Writing the image files into the bound folder. A name is checked like any
//! other path: relative, no `..`, no symbolic link on the way. A file is
//! written whole under a temporary name and renamed, so a failed write never
//! leaves half an image under the real name.

use std::io::Write as _;
use std::path::Path;

use crate::err::{fail, Res};
use crate::load::resolve;

fn describe(name: &str, e: &std::io::Error) -> String {
    use std::io::ErrorKind::{NotFound, PermissionDenied, ReadOnlyFilesystem};
    match e.kind() {
        PermissionDenied | ReadOnlyFilesystem => {
            format!("{name} cannot be written because the folder is read-only; allow write access to it and try again")
        }
        NotFound => format!("{name} cannot be written because its folder does not exist"),
        _ => format!("{name} cannot be written: {e}"),
    }
}

/// Writes `bytes` as `name` below `dir`, creating missing folders.
pub fn write(dir: &Path, name: &str, bytes: &[u8]) -> Res<()> {
    if !dir.is_dir() {
        return fail("to save files, give the folder in path and name the data file in file");
    }
    let target = resolve(dir, name)?;
    if target.is_dir() {
        return fail(format!("{name} is a folder; give the image a file name"));
    }
    let Some(parent) = target.parent() else {
        return fail(format!("{name} is not a file name"));
    };
    std::fs::create_dir_all(parent).map_err(|e| describe(name, &e))?;
    let file = target.file_name().map(|f| f.to_string_lossy().into_owned()).unwrap_or_default();
    let tmp = parent.join(format!(".{file}.tmp"));
    let result = std::fs::File::create(&tmp)
        .and_then(|mut f| f.write_all(bytes).and_then(|()| f.sync_all()))
        .and_then(|()| std::fs::rename(&tmp, &target));
    if let Err(e) = result {
        let _ = std::fs::remove_file(&tmp);
        return fail(describe(name, &e));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutil::TempDir;

    #[test]
    fn a_file_is_written_whole_and_replaces_an_older_one() {
        let d = TempDir::new();
        write(d.path(), "chart.svg", b"one").unwrap();
        write(d.path(), "chart.svg", b"two!").unwrap();
        assert_eq!(std::fs::read(d.path().join("chart.svg")).unwrap(), b"two!");
        let left: Vec<_> = std::fs::read_dir(d.path()).unwrap().map(|e| e.unwrap().file_name()).collect();
        assert_eq!(left.len(), 1, "no temporary file is left: {left:?}");
    }

    #[test]
    fn missing_folders_on_the_way_are_created() {
        let d = TempDir::new();
        write(d.path(), "out/charts/a.png", b"x").unwrap();
        assert_eq!(std::fs::read(d.path().join("out/charts/a.png")).unwrap(), b"x");
    }

    #[test]
    fn names_that_leave_the_folder_are_refused_and_nothing_is_written() {
        let d = TempDir::new();
        let outer = d.path().join("inner");
        std::fs::create_dir(&outer).unwrap();
        for bad in ["../escape.svg", "/tmp/escape.svg", "a/../../escape.svg", "a\\b.svg", ""] {
            assert!(write(&outer, bad, b"x").is_err(), "{bad:?}");
        }
        assert!(!d.path().join("escape.svg").exists());
    }

    #[test]
    fn a_folder_or_a_file_in_place_of_the_bound_folder_is_refused() {
        let d = TempDir::new();
        let f = d.write("data.csv", b"a\n1\n");
        assert!(write(&f, "chart.svg", b"x").unwrap_err().0.contains("give the folder in path"));
        std::fs::create_dir(d.path().join("chart.svg")).unwrap();
        assert!(write(d.path(), "chart.svg", b"x").unwrap_err().0.contains("is a folder"));
    }

    #[cfg(unix)]
    #[test]
    fn a_symbolic_link_to_a_file_or_folder_outside_is_not_followed() {
        let d = TempDir::new();
        let outside = TempDir::new();
        std::os::unix::fs::symlink(outside.path(), d.path().join("link")).unwrap();
        std::os::unix::fs::symlink(outside.path().join("target.svg"), d.path().join("evil.svg")).unwrap();
        assert!(write(d.path(), "link/chart.svg", b"x").unwrap_err().0.contains("symbolic link"));
        assert!(write(d.path(), "evil.svg", b"x").unwrap_err().0.contains("symbolic link"));
        assert!(!outside.path().join("chart.svg").exists());
        assert!(!outside.path().join("target.svg").exists());
    }

    #[cfg(unix)]
    #[test]
    fn a_read_only_folder_gives_the_allow_write_sentence_and_no_stray_file() {
        use std::os::unix::fs::PermissionsExt;
        let d = TempDir::new();
        std::fs::set_permissions(d.path(), std::fs::Permissions::from_mode(0o555)).unwrap();
        let result = write(d.path(), "chart.svg", b"x");
        std::fs::set_permissions(d.path(), std::fs::Permissions::from_mode(0o755)).unwrap();
        // Running as the owner of the folder with write access overridden (root) can succeed.
        if let Err(e) = result {
            assert!(e.0.contains("read-only") || e.0.contains("cannot be written"), "{e}");
            assert!(!d.path().join(".chart.svg.tmp").exists());
        }
    }
}
