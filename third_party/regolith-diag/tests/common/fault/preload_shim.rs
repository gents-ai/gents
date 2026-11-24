//! LD_PRELOAD interposer that journals the file-I/O syscall stream of a
//! child process and can kill that child at a byte-exact point in the
//! stream.
//!
//! This file is NOT a module of any test crate. It is compiled at test
//! time by `fault::shim::build()` with a direct `rustc` invocation into a
//! `cdylib`, then injected with `LD_PRELOAD`. Keeping it out of the
//! workspace keeps `cargo check --workspace`, clippy and the MSRV job on
//! pure library code.
//!
//! # Why interposition works here
//!
//! regolith performs every write through `std::fs`, which on
//! `*-linux-gnu` calls the glibc `write`/`pwrite64`/`writev`/`fsync`/
//! `fdatasync`/`open64`/`openat` symbols through the PLT, and through the
//! glibc `pwritev` for a run of log records. It uses `rustix`
//! (raw syscalls, NOT interposable) only for `flock`, `fadvise` and the
//! positional reads of its tables (`pread`), none of which writes file data.
//! There is no `mmap` write path.
//!
//! # Journal format
//!
//! One ASCII record per line, tab separated, `\n` terminated, appended with
//! the real `write` on an `O_APPEND` fd so a `SIGKILL` cannot lose a record
//! that was already emitted. Fields:
//!
//! ```text
//! seq \t tid \t kind \t fd \t a \t b \t ret \t path
//! ```
//!
//! * `O` open      a = size at open, b = raw open flags
//! * `W` write     a = offset (-1 = append, resolve against running EOF), b = requested len
//! * `S` fsync     a = 0 for fsync, 1 for fdatasync
//! * `T` truncate  a = new length
//! * `C` close
//! * `R` rename    path = "from\tto" is avoided: `a` unused, path = from, ret field carries to
//! * `U` unlink
//!
//! `seq` is a global monotonic counter, so records can be totally ordered
//! even though threads append concurrently. `tid` is the kernel thread id,
//! which is what lets a reconstruction tell a foreground write from a
//! background flush or compaction write.
//!
//! Every record is [`RECORD`] bytes: the path is followed by NUL, which no
//! path holds, up to the line end. The length divides a page, so in a file
//! that holds nothing but whole records no record lies across two pages.
//! That is what keeps a record whole when the process is killed. The kernel
//! copies a write a page at a time and gives up between two pages once the
//! kill is pending, so a record across a page boundary could be left cut at
//! the boundary by a thread that was inside its write when another thread
//! killed the process; and since the kill reaches the threads one after the
//! other, a thread it had not reached yet could still append a whole record
//! behind the cut one.
//!
//! # Environment
//!
//! * `REGOLITH_FAULT_JOURNAL`   path of the journal. Unset = the shim is inert.
//! * `REGOLITH_FAULT_ROOT`      only paths containing this substring are tracked.
//! * `REGOLITH_FAULT_DIE_KIND`  `write` | `fsync` | `open` | `truncate`, unset = never die.
//! * `REGOLITH_FAULT_DIE_PATH`  substring the path must contain to count.
//! * `REGOLITH_FAULT_DIE_NTH`   1-based index of the matching operation to die on.
//! * `REGOLITH_FAULT_DIE_WHEN`  `after` (default) or `before` the real call.
//! * `REGOLITH_FAULT_LOADED`    a file the shim appends its pid to when the
//!   dynamic linker loads it, so the parent can tell a child that ran with
//!   the shim from one that ran without it.

#![allow(clippy::missing_safety_doc)]

use std::ffi::{c_char, c_int, c_long, c_uint, c_void, VaList};
use std::sync::atomic::{AtomicI32, AtomicU64, AtomicU8, Ordering};

const RTLD_NEXT: *mut c_void = -1isize as *mut c_void;

const O_APPEND: c_int = 0o2000;
const O_CREAT: c_int = 0o100;
const O_DIRECTORY: c_int = if cfg!(any(
    target_arch = "arm",
    target_arch = "aarch64",
    target_arch = "m68k",
    target_arch = "powerpc",
    target_arch = "powerpc64",
)) {
    0o40000
} else {
    0o200000
};
const O_TMPFILE: c_int = if cfg!(any(target_arch = "sparc", target_arch = "sparc64")) {
    0o200000000 | O_DIRECTORY
} else {
    0o20000000 | O_DIRECTORY
};
const SEEK_SET: c_int = 0;
const SEEK_CUR: c_int = 1;
const SEEK_END: c_int = 2;
const SIGKILL: c_int = 9;

const MAX_FD: usize = 4096;
/// The length of every journal record, line end included. It divides a
/// page; see the journal format above.
const RECORD: usize = 512;
const TAG_UNTRACKED: u8 = 0;
const TAG_TRACKED: u8 = 1;
const TAG_DIE_MATCH: u8 = 2;

const DIE_NONE: u8 = 0;
const DIE_WRITE: u8 = 1;
const DIE_FSYNC: u8 = 2;
const DIE_OPEN: u8 = 3;
const DIE_TRUNCATE: u8 = 4;

extern "C" {
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
    fn getpid() -> c_int;
    fn gettid() -> c_int;
    fn kill(pid: c_int, sig: c_int) -> c_int;
}

type WriteFn = unsafe extern "C" fn(c_int, *const c_void, usize) -> isize;
type PwriteFn = unsafe extern "C" fn(c_int, *const c_void, usize, i64) -> isize;
type WritevFn = unsafe extern "C" fn(c_int, *const c_void, c_int) -> isize;
type PwritevFn = unsafe extern "C" fn(c_int, *const c_void, c_int, i64) -> isize;
type SyncFn = unsafe extern "C" fn(c_int) -> c_int;
type OpenFn = unsafe extern "C" fn(*const c_char, c_int, ...) -> c_int;
type OpenatFn = unsafe extern "C" fn(c_int, *const c_char, c_int, ...) -> c_int;
type LseekFn = unsafe extern "C" fn(c_int, i64, c_int) -> i64;
type CloseFn = unsafe extern "C" fn(c_int) -> c_int;
type TruncFn = unsafe extern "C" fn(c_int, i64) -> c_int;
type RenameFn = unsafe extern "C" fn(*const c_char, *const c_char) -> c_int;
type UnlinkFn = unsafe extern "C" fn(*const c_char) -> c_int;

/// Resolve the next definition of `name` in the link chain. Returns null
/// when the symbol does not exist, which is a fatal condition for any
/// symbol we interpose, so callers abort loudly rather than recursing.
unsafe fn next_sym(name: &[u8]) -> *mut c_void {
    dlsym(RTLD_NEXT, name.as_ptr() as *const c_char)
}

macro_rules! real {
    ($cache:ident, $ty:ty, $name:literal) => {{
        static CACHE: AtomicU64 = AtomicU64::new(0);
        let mut p = CACHE.load(Ordering::Relaxed);
        if p == 0 {
            p = next_sym(concat!($name, "\0").as_bytes()) as u64;
            if p == 0 {
                die_loudly(concat!("regolith-fault-shim: missing symbol ", $name));
            }
            CACHE.store(p, Ordering::Relaxed);
        }
        let _ = stringify!($cache);
        core::mem::transmute::<u64, $ty>(p)
    }};
}

fn die_loudly(msg: &str) -> ! {
    unsafe {
        let w = next_sym(b"write\0");
        if !w.is_null() {
            let f = core::mem::transmute::<*mut c_void, WriteFn>(w);
            f(2, msg.as_ptr() as *const c_void, msg.len());
            f(2, b"\n".as_ptr() as *const c_void, 1);
        }
        kill(getpid(), SIGKILL);
    }
    unreachable!()
}

static SEQ: AtomicU64 = AtomicU64::new(1);
static JOURNAL_FD: AtomicI32 = AtomicI32::new(-2);
static DIE_HITS: AtomicU64 = AtomicU64::new(0);

static FD_TAGS: [AtomicU8; MAX_FD] = {
    #[allow(clippy::declare_interior_mutable_const)]
    const Z: AtomicU8 = AtomicU8::new(0);
    [Z; MAX_FD]
};

thread_local! {
    static IN_SHIM: core::cell::Cell<bool> = const { core::cell::Cell::new(false) };
}

struct Guard(bool);

impl Guard {
    fn enter() -> Guard {
        let prev = IN_SHIM.with(|c| c.replace(true));
        Guard(prev)
    }
    fn active(&self) -> bool {
        !self.0
    }
}

impl Drop for Guard {
    fn drop(&mut self) {
        IN_SHIM.with(|c| c.set(self.0));
    }
}

fn env(name: &str) -> Option<String> {
    std::env::var(name).ok().filter(|s| !s.is_empty())
}

struct Config {
    root: String,
    die_kind: u8,
    die_path: String,
    die_nth: u64,
    die_before: bool,
}

fn config() -> &'static Config {
    static CELL: std::sync::OnceLock<Config> = std::sync::OnceLock::new();
    CELL.get_or_init(|| Config {
        root: env("REGOLITH_FAULT_ROOT").unwrap_or_default(),
        die_kind: match env("REGOLITH_FAULT_DIE_KIND").as_deref() {
            Some("write") => DIE_WRITE,
            Some("fsync") => DIE_FSYNC,
            Some("open") => DIE_OPEN,
            Some("truncate") => DIE_TRUNCATE,
            _ => DIE_NONE,
        },
        die_path: env("REGOLITH_FAULT_DIE_PATH").unwrap_or_default(),
        die_nth: env("REGOLITH_FAULT_DIE_NTH")
            .and_then(|s| s.parse().ok())
            .unwrap_or(0),
        die_before: matches!(env("REGOLITH_FAULT_DIE_WHEN").as_deref(), Some("before")),
    })
}

/// Journal fd, opened once. `-1` means the shim is inert for this process.
unsafe fn journal_fd() -> c_int {
    let cur = JOURNAL_FD.load(Ordering::Acquire);
    if cur != -2 {
        return cur;
    }
    let fd = match env("REGOLITH_FAULT_JOURNAL") {
        None => -1,
        Some(path) => {
            let mut c = path.into_bytes();
            c.push(0);
            let open = real!(o, OpenFn, "open64");
            open(
                c.as_ptr() as *const c_char,
                1 | O_CREAT | O_APPEND,
                0o644u32,
            )
        }
    };
    match JOURNAL_FD.compare_exchange(-2, fd, Ordering::AcqRel, Ordering::Acquire) {
        Ok(_) => fd,
        Err(other) => {
            if fd >= 0 {
                let close = real!(c, CloseFn, "close");
                close(fd);
            }
            other
        }
    }
}

struct Rec {
    buf: [u8; RECORD],
    len: usize,
}

impl Rec {
    fn new() -> Rec {
        Rec {
            buf: [0u8; RECORD],
            len: 0,
        }
    }
    fn raw(&mut self, b: &[u8]) {
        let n = b.len().min(self.buf.len() - self.len);
        self.buf[self.len..self.len + n].copy_from_slice(&b[..n]);
        self.len += n;
    }
    fn num(&mut self, mut v: i64) {
        let mut tmp = [0u8; 24];
        let mut i = tmp.len();
        let neg = v < 0;
        let mut u = if neg {
            (v as i128).unsigned_abs() as u128
        } else {
            v as u128
        };
        if u == 0 {
            i -= 1;
            tmp[i] = b'0';
        }
        while u > 0 {
            i -= 1;
            tmp[i] = b'0' + (u % 10) as u8;
            u /= 10;
        }
        if neg {
            i -= 1;
            tmp[i] = b'-';
        }
        v = 0;
        let _ = v;
        self.raw(&tmp[i..]);
    }
    fn tab(&mut self) {
        self.raw(b"\t");
    }
}

/// Emit one journal record. Records are `O_APPEND` single-write appends so
/// a concurrent thread cannot interleave a partial line, and each is
/// [`RECORD`] bytes so a kill cannot cut one.
unsafe fn emit(kind: u8, fd: c_int, a: i64, b: i64, ret: i64, path: &[u8]) {
    let jfd = journal_fd();
    if jfd < 0 {
        return;
    }
    let mut r = Rec::new();
    r.num(SEQ.fetch_add(1, Ordering::Relaxed) as i64);
    r.tab();
    r.num(gettid() as i64);
    r.tab();
    r.raw(&[kind]);
    r.tab();
    r.num(fd as i64);
    r.tab();
    r.num(a);
    r.tab();
    r.num(b);
    r.tab();
    r.num(ret);
    r.tab();
    let room = r.buf.len() - r.len - 1;
    let p = if path.len() > room {
        &path[path.len() - room..]
    } else {
        path
    };
    r.raw(p);
    // The buffer is NUL from here to its last byte, the line end.
    r.buf[RECORD - 1] = b'\n';
    r.len = RECORD;
    let w = real!(w, WriteFn, "write");
    let mut off = 0usize;
    while off < r.len {
        let n = w(jfd, r.buf.as_ptr().add(off) as *const c_void, r.len - off);
        if n <= 0 {
            break;
        }
        off += n as usize;
    }
}

unsafe fn cstr(p: *const c_char) -> &'static [u8] {
    if p.is_null() {
        return b"";
    }
    let mut n = 0usize;
    while *p.add(n) != 0 && n < 4096 {
        n += 1;
    }
    core::slice::from_raw_parts(p as *const u8, n)
}

fn contains(hay: &[u8], needle: &str) -> bool {
    let n = needle.as_bytes();
    if n.is_empty() {
        return true;
    }
    hay.windows(n.len()).any(|w| w == n)
}

/// Whether `path` is tracked: under the database directory, or that
/// directory itself. The root carries a trailing separator to keep the
/// harness's own files beside the directory out; the directory is matched
/// whole so that its own sync, which makes the names of the files directly
/// in it durable, is recorded too.
fn tracked(path: &[u8], root: &str) -> bool {
    contains(path, root) || {
        let dir = root.trim_end_matches('/');
        !dir.is_empty() && (path == dir.as_bytes() || path.strip_suffix(b"/") == Some(dir.as_bytes()))
    }
}

fn tag_of(fd: c_int) -> u8 {
    if fd < 0 || fd as usize >= MAX_FD {
        return TAG_UNTRACKED;
    }
    FD_TAGS[fd as usize].load(Ordering::Relaxed)
}

fn set_tag(fd: c_int, tag: u8) {
    if fd >= 0 && (fd as usize) < MAX_FD {
        FD_TAGS[fd as usize].store(tag, Ordering::Relaxed);
    }
}

/// Count a matching operation and `SIGKILL` self on the configured nth hit.
unsafe fn maybe_die(kind: u8, tag: u8) {
    let cfg = config();
    if cfg.die_kind != kind || cfg.die_nth == 0 {
        return;
    }
    if !cfg.die_path.is_empty() && tag != TAG_DIE_MATCH {
        return;
    }
    let hit = DIE_HITS.fetch_add(1, Ordering::SeqCst) + 1;
    if hit == cfg.die_nth {
        kill(getpid(), SIGKILL);
    }
}

unsafe fn after_open(fd: c_int, path: &[u8], flags: c_int) {
    if fd < 0 || !tracked(path, &config().root) {
        if fd >= 0 {
            set_tag(fd, TAG_UNTRACKED);
        }
        return;
    }
    let cfg = config();
    let tag = if !cfg.die_path.is_empty() && contains(path, &cfg.die_path) {
        TAG_DIE_MATCH
    } else {
        TAG_TRACKED
    };
    set_tag(fd, tag);
    let lseek = real!(l, LseekFn, "lseek64");
    let size = lseek(fd, 0, SEEK_END);
    if size >= 0 {
        lseek(fd, 0, SEEK_SET);
    }
    emit(b'O', fd, size.max(0), flags as i64, 0, path);
    maybe_die(DIE_OPEN, tag);
}

/// Offset a write lands at: exact for a normal fd (a pure `SEEK_CUR`
/// query), `-1` for `O_APPEND` where the kernel repositions atomically and
/// the reconstructor resolves it against the running end of file.
unsafe fn write_offset(fd: c_int) -> i64 {
    let lseek = real!(l, LseekFn, "lseek64");
    lseek(fd, 0, SEEK_CUR)
}

#[no_mangle]
pub unsafe extern "C" fn write(fd: c_int, buf: *const c_void, count: usize) -> isize {
    let real = real!(w, WriteFn, "write");
    let g = Guard::enter();
    let tag = tag_of(fd);
    if !g.active() || tag == TAG_UNTRACKED {
        return real(fd, buf, count);
    }
    if config().die_before {
        maybe_die(DIE_WRITE, tag);
    }
    let off = write_offset(fd);
    let ret = real(fd, buf, count);
    emit(b'W', fd, off, count as i64, ret as i64, b"");
    if !config().die_before {
        maybe_die(DIE_WRITE, tag);
    }
    ret
}

#[no_mangle]
pub unsafe extern "C" fn pwrite64(fd: c_int, buf: *const c_void, count: usize, off: i64) -> isize {
    let real = real!(p, PwriteFn, "pwrite64");
    let g = Guard::enter();
    let tag = tag_of(fd);
    if !g.active() || tag == TAG_UNTRACKED {
        return real(fd, buf, count, off);
    }
    if config().die_before {
        maybe_die(DIE_WRITE, tag);
    }
    let ret = real(fd, buf, count, off);
    emit(b'W', fd, off, count as i64, ret as i64, b"");
    if !config().die_before {
        maybe_die(DIE_WRITE, tag);
    }
    ret
}

#[no_mangle]
pub unsafe extern "C" fn pwrite(fd: c_int, buf: *const c_void, count: usize, off: i64) -> isize {
    pwrite64(fd, buf, count, off)
}

/// One `struct iovec`: what `pwritev` and `writev` take a list of.
#[repr(C)]
struct IoVec {
    base: *const c_void,
    len: usize,
}

/// The bytes the `count` buffers at `iov` hold together: what one vectored
/// write asks to write.
unsafe fn iov_len(iov: *const c_void, count: c_int) -> usize {
    let iov = iov.cast::<IoVec>();
    (0..count.max(0) as usize).map(|i| (*iov.add(i)).len).sum()
}

/// A positional vectored write: the log's write unit puts a run of records
/// at one offset with one of these (`commit::write_unit`), journaled as one
/// write of the bytes its buffers hold together.
#[no_mangle]
pub unsafe extern "C" fn pwritev64(fd: c_int, iov: *const c_void, iovcnt: c_int, off: i64) -> isize {
    let real = real!(pv, PwritevFn, "pwritev64");
    let g = Guard::enter();
    let tag = tag_of(fd);
    if !g.active() || tag == TAG_UNTRACKED {
        return real(fd, iov, iovcnt, off);
    }
    if config().die_before {
        maybe_die(DIE_WRITE, tag);
    }
    let count = iov_len(iov, iovcnt);
    let ret = real(fd, iov, iovcnt, off);
    emit(b'W', fd, off, count as i64, ret as i64, b"");
    if !config().die_before {
        maybe_die(DIE_WRITE, tag);
    }
    ret
}

#[no_mangle]
pub unsafe extern "C" fn pwritev(fd: c_int, iov: *const c_void, iovcnt: c_int, off: i64) -> isize {
    pwritev64(fd, iov, iovcnt, off)
}

#[no_mangle]
pub unsafe extern "C" fn writev(fd: c_int, iov: *const c_void, iovcnt: c_int) -> isize {
    let real = real!(v, WritevFn, "writev");
    let g = Guard::enter();
    let tag = tag_of(fd);
    if !g.active() || tag == TAG_UNTRACKED {
        return real(fd, iov, iovcnt);
    }
    if config().die_before {
        maybe_die(DIE_WRITE, tag);
    }
    let off = write_offset(fd);
    let ret = real(fd, iov, iovcnt);
    emit(b'W', fd, off, ret as i64, ret as i64, b"");
    if !config().die_before {
        maybe_die(DIE_WRITE, tag);
    }
    ret
}

#[no_mangle]
pub unsafe extern "C" fn fsync(fd: c_int) -> c_int {
    let real = real!(s, SyncFn, "fsync");
    let g = Guard::enter();
    let tag = tag_of(fd);
    if !g.active() || tag == TAG_UNTRACKED {
        return real(fd);
    }
    if config().die_before {
        maybe_die(DIE_FSYNC, tag);
    }
    let ret = real(fd);
    emit(b'S', fd, 0, 0, ret as i64, b"");
    if !config().die_before {
        maybe_die(DIE_FSYNC, tag);
    }
    ret
}

#[no_mangle]
pub unsafe extern "C" fn fdatasync(fd: c_int) -> c_int {
    let real = real!(s, SyncFn, "fdatasync");
    let g = Guard::enter();
    let tag = tag_of(fd);
    if !g.active() || tag == TAG_UNTRACKED {
        return real(fd);
    }
    if config().die_before {
        maybe_die(DIE_FSYNC, tag);
    }
    let ret = real(fd);
    emit(b'S', fd, 1, 0, ret as i64, b"");
    if !config().die_before {
        maybe_die(DIE_FSYNC, tag);
    }
    ret
}

#[no_mangle]
pub unsafe extern "C" fn ftruncate64(fd: c_int, len: i64) -> c_int {
    let real = real!(t, TruncFn, "ftruncate64");
    let g = Guard::enter();
    let tag = tag_of(fd);
    if !g.active() || tag == TAG_UNTRACKED {
        return real(fd, len);
    }
    let ret = real(fd, len);
    emit(b'T', fd, len, 0, ret as i64, b"");
    maybe_die(DIE_TRUNCATE, tag);
    ret
}

#[no_mangle]
pub unsafe extern "C" fn ftruncate(fd: c_int, len: i64) -> c_int {
    ftruncate64(fd, len)
}

unsafe fn open_mode(flags: c_int, mut args: VaList<'_>) -> c_uint {
    // Ordinary opens have no mode argument. O_TMPFILE includes O_DIRECTORY,
    // which by itself must not make us read an absent variadic argument.
    if flags & O_CREAT != 0 || flags & O_TMPFILE == O_TMPFILE {
        args.next_arg::<c_uint>()
    } else {
        0
    }
}

unsafe fn open_with_mode(path: *const c_char, flags: c_int, mode: c_uint) -> c_int {    let real = real!(o, OpenFn, "open64");
    let g = Guard::enter();
    if !g.active() {
        return real(path, flags, mode);
    }
    let fd = real(path, flags, mode);
    after_open(fd, cstr(path), flags);
    fd
}

#[allow(unknown_lints, invalid_runtime_symbol_definitions)]
#[no_mangle]
pub unsafe extern "C" fn open64(path: *const c_char, flags: c_int, args: ...) -> c_int {
    open_with_mode(path, flags, open_mode(flags, args))
}

#[allow(unknown_lints, invalid_runtime_symbol_definitions)]
#[no_mangle]
pub unsafe extern "C" fn open(path: *const c_char, flags: c_int, args: ...) -> c_int {
    open_with_mode(path, flags, open_mode(flags, args))
}

unsafe fn openat_with_mode(dirfd: c_int, path: *const c_char, flags: c_int, mode: c_uint) -> c_int {
    let real = real!(a, OpenatFn, "openat64");
    let g = Guard::enter();
    if !g.active() {
        return real(dirfd, path, flags, mode);
    }
    let fd = real(dirfd, path, flags, mode);
    after_open(fd, cstr(path), flags);
    fd
}

#[allow(unknown_lints, invalid_runtime_symbol_definitions)]
#[no_mangle]
pub unsafe extern "C" fn openat64(
    dirfd: c_int,
    path: *const c_char,
    flags: c_int,
    args: ...
) -> c_int {
    openat_with_mode(dirfd, path, flags, open_mode(flags, args))
}

#[no_mangle]
pub unsafe extern "C" fn openat(
    dirfd: c_int,
    path: *const c_char,
    flags: c_int,
    args: ...
) -> c_int {
    openat_with_mode(dirfd, path, flags, open_mode(flags, args))
}

#[no_mangle]
pub unsafe extern "C" fn close(fd: c_int) -> c_int {
    let real = real!(c, CloseFn, "close");
    let g = Guard::enter();
    let tag = tag_of(fd);
    if !g.active() || tag == TAG_UNTRACKED {
        return real(fd);
    }
    emit(b'C', fd, 0, 0, 0, b"");
    set_tag(fd, TAG_UNTRACKED);
    real(fd)
}

#[no_mangle]
pub unsafe extern "C" fn rename(from: *const c_char, to: *const c_char) -> c_int {
    let real = real!(r, RenameFn, "rename");
    let g = Guard::enter();
    if !g.active() {
        return real(from, to);
    }
    let ret = real(from, to);
    let f = cstr(from);
    if contains(f, &config().root) {
        emit(b'R', -1, 0, 0, ret as i64, f);
        emit(b'R', -1, 1, 0, ret as i64, cstr(to));
    }
    ret
}

#[no_mangle]
pub unsafe extern "C" fn unlink(path: *const c_char) -> c_int {
    let real = real!(u, UnlinkFn, "unlink");
    let g = Guard::enter();
    if !g.active() {
        return real(path);
    }
    let ret = real(path);
    let p = cstr(path);
    if contains(p, &config().root) {
        emit(b'U', -1, 0, 0, ret as i64, p);
    }
    ret
}

/// Exported so a harness can assert the shim is actually loaded rather
/// than silently running an un-instrumented child.
#[no_mangle]
pub extern "C" fn regolith_fault_shim_present() -> c_long {
    1
}

/// Run by the dynamic linker as it loads the shim into a process: append
/// this pid to `REGOLITH_FAULT_LOADED`, the parent's proof that the child
/// ran instrumented. Through the real `open` and `write`, never the
/// interposers, so the record is not journaled.
extern "C" fn announce_load() {
    let Some(path) = env("REGOLITH_FAULT_LOADED") else {
        return;
    };
    unsafe {
        let mut c = path.into_bytes();
        c.push(0);
        let open = real!(o, OpenFn, "open64");
        let fd = open(
            c.as_ptr() as *const c_char,
            1 | O_CREAT | O_APPEND,
            0o644u32,
        );
        if fd < 0 {
            return;
        }
        let mut r = Rec::new();
        r.num(getpid() as i64);
        r.raw(b"\n");
        let w = real!(w, WriteFn, "write");
        w(fd, r.buf.as_ptr() as *const c_void, r.len);
        let close = real!(c, CloseFn, "close");
        close(fd);
    }
}

#[used]
#[link_section = ".init_array"]
static ANNOUNCE_LOAD: extern "C" fn() = announce_load;
