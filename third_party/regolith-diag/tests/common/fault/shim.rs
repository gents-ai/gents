//! Builds the `LD_PRELOAD` interposer and reports whether it can run here.
//!
//! The shim source lives at `preload_shim.rs` next to this file and is
//! deliberately *not* a module of the test crate: it is compiled standalone
//! with a direct `rustc` call into a `cdylib`. Keeping it out of the
//! workspace keeps `cargo check --workspace`, clippy and the MSRV job on
//! pure library code, and means no new Cargo member and no C toolchain.
//! The standalone shim needs Rust 1.99 for C-variadic function definitions.
//!
//! The build is content-addressed and atomic: the object is named after a
//! hash of the source and of the compiler that builds it ([`cache_key`]),
//! and installed with a rename, so several test binaries running in
//! parallel cannot race each other, and a run on another toolchain builds
//! an object of its own. Keyed on the source alone, a shim one toolchain
//! built was preloaded into another toolchain's test binary, where it
//! fired no fault and recorded nothing until the cache was deleted. Each builder
//! compiles from a source file of its own: when every builder wrote one
//! shared source path, one builder's truncating write could land while
//! another's `rustc` was reading it, and an empty source compiles into a
//! valid library that interposes nothing. Preloaded into a child, that
//! library never fired a fault and recorded nothing. So an object is
//! installed, and a cached one used, only once it is seen to export every
//! interposer ([`INTERPOSERS`]); and a child records that the shim loaded
//! (`child.rs` checks it), so a child that ran without it fails its test
//! with the reason.

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::process::Command;

const SOURCE: &str = include_str!("preload_shim.rs");

/// Every symbol the shim exports, each a dynamic-symbol name a working
/// object carries. An object missing one was compiled from a source that
/// was empty or cut short.
pub const INTERPOSERS: &[&str] = &[
    "write",
    "pwrite64",
    "pwrite",
    "pwritev64",
    "pwritev",
    "writev",
    "fsync",
    "fdatasync",
    "ftruncate64",
    "ftruncate",
    "open64",
    "open",
    "openat64",
    "openat",
    "close",
    "rename",
    "unlink",
    "regolith_fault_shim_present",
];

/// Makes every builder's own file names unique within the process, beside
/// the process id that makes them unique across processes.
static BUILDS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Interposition needs glibc's dynamic linker and glibc's `write`/`fsync`
/// symbols. Anywhere else the harness must say so rather than quietly
/// running an un-instrumented child.
pub const fn supported() -> bool {
    cfg!(all(target_os = "linux", target_env = "gnu"))
}

#[derive(Clone, Debug)]
pub enum ShimError {
    Unsupported(&'static str),
    Build { status: String, stderr: String },
    Io(String),
}

impl std::fmt::Display for ShimError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ShimError::Unsupported(why) => write!(f, "fault shim unsupported here: {why}"),
            ShimError::Build { status, stderr } => {
                write!(f, "fault shim failed to compile ({status}):\n{stderr}")
            }
            ShimError::Io(e) => write!(f, "fault shim io error: {e}"),
        }
    }
}

impl std::error::Error for ShimError {}

fn out_dir() -> PathBuf {
    // Cargo sets CARGO_TARGET_TMPDIR while *compiling* an integration
    // test, not while running one, so it has to be read with `option_env!`
    // rather than at runtime. It points inside `target/`, which keeps the
    // cached object out of a shared `/tmp` and lets `cargo clean` remove
    // it.
    match option_env!("CARGO_TARGET_TMPDIR") {
        Some(d) => PathBuf::from(d).join("regolith-fault"),
        None => std::env::temp_dir().join("regolith-fault"),
    }
}

/// The compiler that builds the shim: `RUSTC` when cargo or the caller set
/// it, else the `rustc` on the path, which under rustup is the toolchain
/// this test run uses.
fn compiler() -> String {
    std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string())
}

/// What tells one compiler from another: everything `rustc -vV` prints
/// (release, commit, host, LLVM). Asked once a process.
pub fn compiler_identity() -> Result<String, ShimError> {
    static IDENTITY: std::sync::OnceLock<Result<String, ShimError>> = std::sync::OnceLock::new();
    IDENTITY
        .get_or_init(|| {
            let rustc = compiler();
            let out = Command::new(&rustc)
                .arg("-vV")
                .output()
                .map_err(|e| ShimError::Io(format!("asking {rustc} for its version: {e}")))?;
            if !out.status.success() {
                return Err(ShimError::Io(format!(
                    "{rustc} -vV failed ({}): {}",
                    out.status,
                    String::from_utf8_lossy(&out.stderr)
                )));
            }
            Ok(String::from_utf8_lossy(&out.stdout).into_owned())
        })
        .clone()
}

/// The cached object's name: a hash of the shim's source and of the
/// compiler that builds it. A different toolchain gets a different object,
/// since a cdylib embeds its compiler's std.
pub fn cache_key(compiler_identity: &str) -> u64 {
    let mut h = DefaultHasher::new();
    SOURCE.hash(&mut h);
    compiler_identity.hash(&mut h);
    h.finish()
}

/// Compile the shim if it is not already cached, and return its path.
pub fn build() -> Result<PathBuf, ShimError> {
    build_in(&out_dir())
}

/// [`build`] into `dir`: the cached object when one there exports every
/// interposer, otherwise a fresh one compiled from this builder's own copy
/// of the source and installed with a rename.
pub fn build_in(dir: &Path) -> Result<PathBuf, ShimError> {
    if !supported() {
        return Err(ShimError::Unsupported(
            "LD_PRELOAD interposition needs a linux-gnu target",
        ));
    }
    std::fs::create_dir_all(dir).map_err(|e| ShimError::Io(e.to_string()))?;
    let hash = cache_key(&compiler_identity()?);
    let lib = dir.join(format!("libregolith_fault_shim_{hash:016x}.so"));
    if lib.is_file() && missing_interposer(&lib)?.is_none() {
        return Ok(lib);
    }

    // Named for this builder alone: a shared path could be truncated by a
    // second builder while this one's rustc reads it.
    let unique = format!(
        "{hash:016x}_{}_{}_{}",
        std::process::id(),
        BUILDS.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    );
    let src = dir.join(format!("preload_shim_{unique}.rs"));
    let staged = dir.join(format!("staged_{unique}.so"));
    std::fs::write(&src, SOURCE).map_err(|e| ShimError::Io(e.to_string()))?;

    let out = Command::new(compiler())
        .args(["--edition", "2021"])
        .args(["--crate-type", "cdylib"])
        .args(["--crate-name", "regolith_fault_shim"])
        .args(["-C", "opt-level=1"])
        .args(["-C", "panic=abort"])
        .arg("-o")
        .arg(&staged)
        .arg(&src)
        .output();
    let _ = std::fs::remove_file(&src);
    let out = out.map_err(|e| ShimError::Io(format!("spawning rustc: {e}")))?;
    if !out.status.success() {
        let _ = std::fs::remove_file(&staged);
        return Err(ShimError::Build {
            status: out.status.to_string(),
            stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
        });
    }
    if let Some(symbol) = missing_interposer(&staged)? {
        let _ = std::fs::remove_file(&staged);
        return Err(ShimError::Build {
            status: format!("the compiled shim does not export `{symbol}`"),
            stderr: String::new(),
        });
    }
    // Rename is atomic within a directory, so a parallel builder either
    // sees no file or sees a complete one, and every complete one works.
    std::fs::rename(&staged, &lib).map_err(|e| ShimError::Io(e.to_string()))?;
    Ok(lib)
}

/// The first interposer `object` does not export, if any: not defined in
/// its dynamic symbol table. Read from the ELF itself, because a name also
/// appears in an object that only imports it (an empty library still calls
/// libc's `write`).
pub fn missing_interposer(object: &Path) -> Result<Option<&'static str>, ShimError> {
    let bytes = std::fs::read(object).map_err(|e| ShimError::Io(e.to_string()))?;
    let exported = exported_symbols(&bytes).ok_or_else(|| {
        ShimError::Io(format!(
            "{} is not a shared object this harness can read",
            object.display()
        ))
    })?;
    Ok(INTERPOSERS
        .iter()
        .copied()
        .find(|symbol| !exported.iter().any(|e| e == symbol)))
}

/// The names an ELF shared object defines in its dynamic symbol table, or
/// `None` when `bytes` is not a little-endian ELF this reads.
fn exported_symbols(bytes: &[u8]) -> Option<Vec<String>> {
    let u16_at = |at: usize| Some(u16::from_le_bytes(bytes.get(at..at + 2)?.try_into().ok()?));
    let u32_at = |at: usize| Some(u32::from_le_bytes(bytes.get(at..at + 4)?.try_into().ok()?));
    let u64_at = |at: usize| Some(u64::from_le_bytes(bytes.get(at..at + 8)?.try_into().ok()?));
    if bytes.get(..4)? != b"\x7fELF" || *bytes.get(5)? != 1 {
        return None;
    }
    let wide = match bytes.get(4)? {
        2 => true,
        1 => false,
        _ => return None,
    };
    let word = |at: usize| -> Option<usize> {
        if wide {
            usize::try_from(u64_at(at)?).ok()
        } else {
            usize::try_from(u32_at(at)?).ok()
        }
    };
    // Section header table: offset, entry size and count.
    let (shoff, shentsize, shnum) = if wide {
        (
            word(0x28)?,
            usize::from(u16_at(0x3a)?),
            usize::from(u16_at(0x3c)?),
        )
    } else {
        (
            word(0x20)?,
            usize::from(u16_at(0x2e)?),
            usize::from(u16_at(0x30)?),
        )
    };
    // One section header's type, file offset, size, link and entry size.
    let section = |index: usize| -> Option<(u32, usize, usize, usize, usize)> {
        let at = shoff.checked_add(index.checked_mul(shentsize)?)?;
        if wide {
            Some((
                u32_at(at + 4)?,
                word(at + 0x18)?,
                word(at + 0x20)?,
                usize::try_from(u32_at(at + 0x28)?).ok()?,
                word(at + 0x38)?,
            ))
        } else {
            Some((
                u32_at(at + 4)?,
                word(at + 0x10)?,
                word(at + 0x14)?,
                usize::try_from(u32_at(at + 0x18)?).ok()?,
                word(at + 0x24)?,
            ))
        }
    };
    const SHT_DYNSYM: u32 = 11;
    let (_, sym_off, sym_size, strtab, sym_ent) = (0..shnum)
        .filter_map(section)
        .find(|(kind, ..)| *kind == SHT_DYNSYM)?;
    let (_, str_off, str_size, _, _) = section(strtab)?;
    let names = bytes.get(str_off..str_off.checked_add(str_size)?)?;
    let mut exported = Vec::new();
    for at in (sym_off..sym_off.checked_add(sym_size)?).step_by(sym_ent.max(1)) {
        let name = usize::try_from(u32_at(at)?).ok()?;
        // `st_shndx` is 0 (undefined) for a symbol the object imports.
        let shndx = if wide {
            u16_at(at + 6)?
        } else {
            u16_at(at + 14)?
        };
        if shndx == 0 || name == 0 {
            continue;
        }
        let tail = names.get(name..)?;
        let end = tail.iter().position(|&b| b == 0)?;
        exported.push(String::from_utf8_lossy(&tail[..end]).into_owned());
    }
    Some(exported)
}

/// True when the shim compiles and can be preloaded on this machine.
pub fn available() -> bool {
    build().is_ok()
}

/// The shim path, or a panic that says exactly why power-loss testing
/// cannot run here. Tests call this rather than degrading silently to a
/// weaker model.
pub fn require() -> PathBuf {
    match build() {
        Ok(p) => p,
        Err(e) => panic!(
            "{e}\nPower-loss simulation needs the LD_PRELOAD shim. \
             Without it a crash test only models a process kill, which leaves unsynced \
             bytes in the page cache and proves far less. The shim is built by the \
             toolchain running this test and needs Rust 1.99 or newer: on an older \
             one, run the suite as `cargo +1.99.0 test --test <name>`."
        ),
    }
}

/// Prepend the shim to any inherited `LD_PRELOAD`.
pub fn preload_value(lib: &Path) -> String {
    match std::env::var("LD_PRELOAD") {
        Ok(existing) if !existing.is_empty() => format!("{}:{}", lib.display(), existing),
        _ => lib.display().to_string(),
    }
}
