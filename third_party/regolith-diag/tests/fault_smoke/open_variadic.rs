//! Exercise the variadic glibc entry points through the preloaded shim.

use std::ffi::{CString, c_char, c_int, c_uint};
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::FromRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

use crate::common::fault::{self, ChildSpec, CrashRun, OpKind, Phase};
use rustix::fs::OFlags;
use tempfile::TempDir;

const O_RDONLY: c_int = OFlags::RDONLY.bits() as c_int;
const O_RDWR: c_int = OFlags::RDWR.bits() as c_int;
const O_CREAT: c_int = OFlags::CREATE.bits() as c_int;
const O_EXCL: c_int = OFlags::EXCL.bits() as c_int;
const O_DIRECTORY: c_int = OFlags::DIRECTORY.bits() as c_int;
const AT_FDCWD: c_int = -100;
const MODE: c_uint = 0o666;
const MASK: c_uint = 0o027;
const CONTENTS: &[u8] = b"variadic open probe";

unsafe extern "C" {
    fn open(path: *const c_char, flags: c_int, ...) -> c_int;
    fn open64(path: *const c_char, flags: c_int, ...) -> c_int;
    fn openat(dirfd: c_int, path: *const c_char, flags: c_int, ...) -> c_int;
    fn openat64(dirfd: c_int, path: *const c_char, flags: c_int, ...) -> c_int;
    fn umask(mask: c_uint) -> c_uint;
}

type OpenFn = unsafe extern "C" fn(*const c_char, c_int, ...) -> c_int;
type OpenatFn = unsafe extern "C" fn(c_int, *const c_char, c_int, ...) -> c_int;

#[derive(Clone, Copy)]
enum Entry {
    Direct(OpenFn),
    At(OpenatFn),
}

const ENTRIES: [(&str, Entry); 4] = [
    ("open", Entry::Direct(open)),
    ("open64", Entry::Direct(open64)),
    ("openat", Entry::At(openat)),
    ("openat64", Entry::At(openat64)),
];

impl Entry {
    fn call(self, path: &Path, flags: c_int, mode: Option<c_uint>) -> File {
        let c_path = CString::new(path.as_os_str().as_bytes()).unwrap();
        // The optional argument must actually be absent for read-only and
        // directory opens; passing zero would hide an incorrect shim signature.
        let fd = unsafe {
            match (self, mode) {
                (Self::Direct(f), Some(mode)) => f(c_path.as_ptr(), flags, mode),
                (Self::Direct(f), None) => f(c_path.as_ptr(), flags),
                (Self::At(f), Some(mode)) => f(AT_FDCWD, c_path.as_ptr(), flags, mode),
                (Self::At(f), None) => f(AT_FDCWD, c_path.as_ptr(), flags),
            }
        };
        assert!(
            fd >= 0,
            "open {} with flags {flags:#o}: {}",
            path.display(),
            std::io::Error::last_os_error(),
        );
        // A successful open returned a new descriptor owned by this call.
        unsafe { File::from_raw_fd(fd) }
    }
}

fn write_probe(mut file: File) {
    let metadata = file.metadata().unwrap();
    assert_eq!(metadata.permissions().mode() & 0o777, MODE & !MASK);
    file.write_all(CONTENTS).unwrap();
    file.sync_all().unwrap();
}

fn workload(spec: &ChildSpec) {
    // This workload runs alone in its own process and exits afterward.
    unsafe { umask(MASK) };
    for (name, entry) in ENTRIES {
        let path = spec.db_path.join(name);
        write_probe(entry.call(&path, O_RDWR | O_CREAT | O_EXCL, Some(MODE)));
        let mut contents = Vec::new();
        entry
            .call(&path, O_RDONLY, None)
            .read_to_end(&mut contents)
            .unwrap();
        assert_eq!(contents, CONTENTS);

        let directory = spec.db_path.join(format!("{name}-directory"));
        std::fs::create_dir(&directory).unwrap();
        assert!(
            entry
                .call(&directory, O_RDONLY | O_DIRECTORY, None)
                .metadata()
                .unwrap()
                .is_dir(),
        );
    }
}

#[test]
fn open_child() {
    fault::child_entrypoint(workload);
}

#[test]
fn all_open_symbols_preserve_optional_modes_and_record_io() {
    let tmp = TempDir::new().unwrap();
    let db = tmp.path().join("db");
    let out = CrashRun::new(ChildSpec::new(Phase::Custom("open-variadic".into()), &db))
        .entry_test("open_variadic::open_child")
        .run();
    out.assert_clean();
    assert_eq!(out.journal.malformed, 0, "{}", out.journal);

    for (name, _) in ENTRIES {
        for (path, flags, written) in [
            (
                db.join(name),
                vec![O_RDWR | O_CREAT | O_EXCL, O_RDONLY],
                CONTENTS.len() as u64,
            ),
            (
                db.join(format!("{name}-directory")),
                vec![O_RDONLY | O_DIRECTORY],
                0,
            ),
        ] {
            let records: Vec<_> = out
                .journal
                .records
                .iter()
                .filter(|record| record.path == path)
                .collect();
            let opens: Vec<_> = records
                .iter()
                .filter(|record| record.kind == OpKind::Open && record.succeeded())
                .map(|record| record.b)
                .collect();
            assert_eq!(
                opens,
                flags.into_iter().map(i64::from).collect::<Vec<_>>(),
                "{}",
                path.display(),
            );
            assert_eq!(
                records.iter().map(|record| record.written()).sum::<u64>(),
                written,
                "missing or duplicated writes for {}",
                path.display(),
            );
            if written > 0 {
                assert!(
                    records
                        .iter()
                        .any(|record| record.kind == OpKind::Sync && record.succeeded()),
                    "missing sync for {}",
                    path.display(),
                );
            }
        }
    }
}
