//! The synchronous-access-handle pool.
//!
//! `createSyncAccessHandle()` is itself asynchronous even inside a worker;
//! only `read`, `write`, `flush`, `getSize`, `truncate` and `close` on the
//! resulting handle are synchronous. regolith creates WAL and SSTable files
//! from inside synchronous engine calls, and recovery lists directories
//! from inside a synchronous `Db::open`, so there is nowhere to `await`.
//!
//! The pool resolves that the way SQLite's `opfs-sahpool` VFS does: a fixed
//! set of physical OPFS files is opened with sync access handles up front,
//! and regolith's logical paths are assigned to those slots at runtime. Every
//! filesystem operation the engine performs is then a map lookup plus a
//! synchronous handle call.
//!
//! # Slot format
//!
//! Each physical file carries a 512-byte header followed by the logical
//! file contents, so logical offset `o` is physical offset `o + 512`.
//!
//! ```text
//! offset   0  u32 LE  magic 0x4B52414C
//! offset   4  u32 LE  flags; bit 0 = slot is in use
//! offset   8  u32 LE  logical path length in bytes
//! offset  12  u64 LE  logical file length in bytes
//! offset  20  u64 LE  generation, bumped on every path assignment
//! offset  28  u32 LE  checksum of bytes [0, 28) followed by the path
//! offset  32  [u8]    logical path, UTF-8, at most 480 bytes
//! offset 512  [u8]    file contents
//! ```
//!
//! `getSize()` reports the physical slot size, not the live logical
//! length, which is why the length is recorded explicitly.
//!
//! # Crash behaviour
//!
//! Contents are written before the header, and the header, which records
//! the logical length, is rewritten only when the file is synced, then
//! flushed with the contents in one `flush`. That flush does not order
//! them: a crash during it can persist the new length before the bytes it
//! covers, which then read back as whatever the slot held there.
//! regolith handles both shapes. A WAL tail past its last completed sync,
//! torn, zeroed or garbled alike, is dropped and reported by format 2
//! replay, and a record copied from a slot's earlier contents never
//! verifies, since each record's header check is bound to its log's nonce
//! and its own offset. An SSTable only enters a version after the manifest
//! edit that references it, so a torn SSTable is an unreferenced orphan.
//!
//! Rename is crash-safe without an atomic rename primitive, which OPFS
//! does not offer. The destination binding is written to the source slot
//! with a fresh, higher generation and flushed; only then is the old slot
//! released. A crash in between leaves two slots claiming one path, and
//! mount keeps the higher generation.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::io;
use std::path::{Path, PathBuf};

use wasm_bindgen::JsValue;

use super::js;
use crate::engine::checksum;

/// Bytes reserved at the head of every physical slot file.
pub(super) const SLOT_HEADER_LEN: u64 = 512;
/// Fixed-width part of the header, before the logical path.
const HEADER_FIXED_LEN: usize = 32;
/// "REGO" little-endian.
const SLOT_MAGIC: u32 = 0x4F47_4552;
const FLAG_IN_USE: u32 = 1 << 0;
const MAX_PATH_LEN: usize = (SLOT_HEADER_LEN as usize) - HEADER_FIXED_LEN;
/// Physical name prefix. Files in the OPFS directory that do not start
/// with this are left alone, so a mirror-mode database and a pool can
/// share one directory without either eating the other's files.
const SLOT_PREFIX: &str = ".regolith-sah-";

/// Identifies one mounted pool inside the per-thread handle registry.
pub(super) type MountId = u64;

/// The JS handles for one mount.
///
/// `FileSystemSyncAccessHandle` is neither `Send` nor `Sync`, and regolith's
/// [`crate::env::Env`] must be both. Rather than assert thread-safety with
/// `unsafe` (the crate forbids it), the handles live in a thread-local
/// registry and the pool itself holds only plain Rust data plus an integer
/// mount id. A lookup from another thread finds nothing and reports
/// `Unsupported`, so the worst case is a clear error, never a data race.
struct Mount {
    directory: JsValue,
    handles: Vec<JsValue>,
}

thread_local! {
    static MOUNTS: RefCell<HashMap<MountId, Mount>> = RefCell::new(HashMap::new());
    static NEXT_MOUNT: Cell<MountId> = const { Cell::new(0) };
}

fn wrong_thread() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "OPFS sync access handles are bound to the thread that mounted them",
    )
}

fn js_error(what: &str, value: JsValue) -> io::Error {
    let detail = value
        .as_string()
        .or_else(|| {
            js_sys::Reflect::get(&value, &JsValue::from_str("message"))
                .ok()?
                .as_string()
        })
        .unwrap_or_else(|| format!("{value:?}"));
    io::Error::other(format!("OPFS {what} failed: {detail}"))
}

fn with_mount<R>(id: MountId, f: impl FnOnce(&Mount) -> io::Result<R>) -> io::Result<R> {
    MOUNTS.with(|mounts| match mounts.borrow().get(&id) {
        Some(mount) => f(mount),
        None => Err(wrong_thread()),
    })
}

/// One physical slot's Rust-side bookkeeping.
#[derive(Debug, Clone)]
pub(super) struct Slot {
    /// `None` when the slot is free.
    pub(super) path: Option<PathBuf>,
    pub(super) len: u64,
    pub(super) generation: u64,
    /// Set when `len` or `path` has changed since the header was last
    /// written. `sync_all` and `sync_dir` clear it.
    pub(super) header_dirty: bool,
}

/// The decoded contents of a slot header.
pub(super) struct SlotHeader {
    pub(super) in_use: bool,
    pub(super) path: String,
    pub(super) len: u64,
    pub(super) generation: u64,
}

fn encode_header(header: &SlotHeader) -> Vec<u8> {
    let path = header.path.as_bytes();
    let mut buf = vec![0u8; SLOT_HEADER_LEN as usize];
    buf[0..4].copy_from_slice(&SLOT_MAGIC.to_le_bytes());
    let flags = if header.in_use { FLAG_IN_USE } else { 0 };
    buf[4..8].copy_from_slice(&flags.to_le_bytes());
    buf[8..12].copy_from_slice(&(path.len() as u32).to_le_bytes());
    buf[12..20].copy_from_slice(&header.len.to_le_bytes());
    buf[20..28].copy_from_slice(&header.generation.to_le_bytes());
    let sum = checksum::opfs_slot_header(&buf[0..28], path);
    buf[28..32].copy_from_slice(&sum.to_le_bytes());
    buf[HEADER_FIXED_LEN..HEADER_FIXED_LEN + path.len()].copy_from_slice(path);
    buf
}

/// Decode a slot header, or `None` when the slot has never been formatted
/// or its header was torn. Either way the slot is treated as free.
fn decode_header(buf: &[u8]) -> Option<SlotHeader> {
    if buf.len() < SLOT_HEADER_LEN as usize {
        return None;
    }
    if u32::from_le_bytes(buf[0..4].try_into().ok()?) != SLOT_MAGIC {
        return None;
    }
    let flags = u32::from_le_bytes(buf[4..8].try_into().ok()?);
    let path_len = u32::from_le_bytes(buf[8..12].try_into().ok()?) as usize;
    if path_len > MAX_PATH_LEN {
        return None;
    }
    let len = u64::from_le_bytes(buf[12..20].try_into().ok()?);
    let generation = u64::from_le_bytes(buf[20..28].try_into().ok()?);
    let stored = u32::from_le_bytes(buf[28..32].try_into().ok()?);
    let path_bytes = &buf[HEADER_FIXED_LEN..HEADER_FIXED_LEN + path_len];
    if checksum::opfs_slot_header(&buf[0..28], path_bytes) != stored {
        return None;
    }
    Some(SlotHeader {
        in_use: flags & FLAG_IN_USE != 0,
        path: String::from_utf8(path_bytes.to_vec()).ok()?,
        len,
        generation,
    })
}

fn slot_name(index: usize) -> String {
    format!("{SLOT_PREFIX}{index:04}")
}

/// The slot index a physical file's name carries, or `None` for a file
/// that is not a slot.
fn slot_index(name: &str) -> Option<usize> {
    name.strip_prefix(SLOT_PREFIX)?.parse().ok()
}

/// How many slots `directory` already holds, as one past the highest index
/// present, from one walk that holds a single entry at a time.
///
/// Mount opens at least this many, never fewer: a pool opened with fewer
/// slots than a previous run would leave the files above the cut
/// unreachable, and growing back into them would overwrite live data.
pub(super) async fn existing_slot_count(directory: &JsValue) -> Result<usize, JsValue> {
    let mut walk = js::FileWalk::new(directory)?;
    let mut count = 0;
    while let Some((name, _)) = walk.next_file().await? {
        if let Some(index) = slot_index(&name) {
            count = count.max(index.saturating_add(1));
        }
    }
    Ok(count)
}

/// Open, or create and open, the physical slots `indices` in `directory`.
///
/// Returns the JS handles together with the decoded header of each slot.
/// The first `createSyncAccessHandle` call is also the probe that decides
/// whether this realm supports the pool at all: browsers reject it outside
/// a worker, and that rejection is surfaced to the caller unchanged.
pub(super) async fn open_slots(
    directory: &JsValue,
    indices: std::ops::Range<usize>,
) -> Result<Vec<(JsValue, Option<SlotHeader>)>, JsValue> {
    let mut opened: Vec<(JsValue, Option<SlotHeader>)> = Vec::with_capacity(indices.len());
    for index in indices {
        let step = open_one_slot(directory, index).await;
        match step {
            Ok(slot) => opened.push(slot),
            Err(e) => {
                // Leaving handles open would lock those physical files
                // against the next mount attempt, so unwind before
                // reporting the probe failure.
                for (handle, _) in &opened {
                    let _ = js::close(handle);
                }
                return Err(e);
            }
        }
    }
    Ok(opened)
}

async fn open_one_slot(
    directory: &JsValue,
    index: usize,
) -> Result<(JsValue, Option<SlotHeader>), JsValue> {
    // `create: true` hands back the file a previous run left as it is, and
    // creates the slot only when it is new.
    let file = js::file_handle(directory, &slot_name(index), true).await?;
    let handle = js::sync_access_handle(&file).await?;

    let mut header = vec![0u8; SLOT_HEADER_LEN as usize];
    let read = js::read_at(&handle, 0, &mut header)?;
    let decoded = if read == SLOT_HEADER_LEN as usize {
        decode_header(&header)
    } else {
        None
    };
    Ok((handle, decoded))
}

/// Register a mount's JS handles on the current thread and return its id.
pub(super) fn register_mount(directory: JsValue, handles: Vec<JsValue>) -> MountId {
    let id = NEXT_MOUNT.with(|next| {
        let id = next.get();
        next.set(id + 1);
        id
    });
    MOUNTS.with(|mounts| mounts.borrow_mut().insert(id, Mount { directory, handles }));
    id
}

/// Append freshly opened handles to a registered mount.
pub(super) fn extend_mount(id: MountId, handles: Vec<JsValue>) -> io::Result<()> {
    MOUNTS.with(|mounts| match mounts.borrow_mut().get_mut(&id) {
        Some(mount) => {
            mount.handles.extend(handles);
            Ok(())
        }
        None => Err(wrong_thread()),
    })
}

/// The OPFS directory handle a mount was created against, cloned so the
/// caller can `await` on it without holding the registry borrow.
pub(super) fn mount_directory(id: MountId) -> io::Result<JsValue> {
    with_mount(id, |mount| Ok(mount.directory.clone()))
}

/// Close every handle of a mount and forget it. Called from the pool's
/// `Drop`; a pool dropped on a thread that never mounted it leaves the
/// handles to the browser rather than failing.
pub(super) fn release_mount(id: MountId) {
    MOUNTS.with(|mounts| {
        if let Some(mount) = mounts.borrow_mut().remove(&id) {
            for handle in &mount.handles {
                let _ = js::close(handle);
            }
        }
    });
}

/// Read logical bytes from a slot.
pub(super) fn read_slot(id: MountId, slot: usize, at: u64, buf: &mut [u8]) -> io::Result<usize> {
    with_mount(id, |mount| {
        let handle = mount
            .handles
            .get(slot)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "OPFS slot is not open"))?;
        js::read_at(handle, SLOT_HEADER_LEN + at, buf).map_err(|e| js_error("read", e))
    })
}

/// Write logical bytes to a slot. A short write means the origin's
/// storage quota is exhausted, which is reported rather than retried.
pub(super) fn write_slot(id: MountId, slot: usize, at: u64, buf: &[u8]) -> io::Result<()> {
    with_mount(id, |mount| {
        let handle = mount
            .handles
            .get(slot)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "OPFS slot is not open"))?;
        let written =
            js::write_at(handle, SLOT_HEADER_LEN + at, buf).map_err(|e| js_error("write", e))?;
        if written != buf.len() {
            return Err(io::Error::other(format!(
                "OPFS write was short ({written} of {} bytes); the origin's storage quota is exhausted",
                buf.len()
            )));
        }
        Ok(())
    })
}

/// Rewrite a slot's header and flush the physical file.
pub(super) fn sync_slot(id: MountId, slot: usize, state: &Slot) -> io::Result<()> {
    let header = encode_header(&SlotHeader {
        in_use: state.path.is_some(),
        path: state
            .path
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default(),
        len: state.len,
        generation: state.generation,
    });
    with_mount(id, |mount| {
        let handle = mount
            .handles
            .get(slot)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "OPFS slot is not open"))?;
        let written = js::write_at(handle, 0, &header).map_err(|e| js_error("header write", e))?;
        if written != header.len() {
            return Err(io::Error::other(
                "OPFS header write was short; the origin's storage quota is exhausted",
            ));
        }
        js::flush(handle).map_err(|e| js_error("flush", e))
    })
}

/// Flush a slot whose header is already durable.
pub(super) fn flush_slot(id: MountId, slot: usize) -> io::Result<()> {
    with_mount(id, |mount| {
        let handle = mount
            .handles
            .get(slot)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "OPFS slot is not open"))?;
        js::flush(handle).map_err(|e| js_error("flush", e))
    })
}

/// Shrink a slot's physical file to `len` logical bytes.
pub(super) fn truncate_slot(id: MountId, slot: usize, len: u64) -> io::Result<()> {
    with_mount(id, |mount| {
        let handle = mount
            .handles
            .get(slot)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "OPFS slot is not open"))?;
        js::truncate(handle, SLOT_HEADER_LEN + len).map_err(|e| js_error("truncate", e))
    })
}

/// Reject a logical path that will not fit in a slot header.
pub(super) fn check_path_fits(path: &Path) -> io::Result<()> {
    let len = path.to_string_lossy().len();
    if len > MAX_PATH_LEN {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("path is {len} bytes; an OPFS slot header holds at most {MAX_PATH_LEN}"),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use wasm_bindgen_test::wasm_bindgen_test;

    #[wasm_bindgen_test]
    fn header_round_trips() {
        let header = SlotHeader {
            in_use: true,
            path: "db/sst/000042.sst".to_string(),
            len: 4096,
            generation: 7,
        };
        let encoded = encode_header(&header);
        assert_eq!(encoded.len(), SLOT_HEADER_LEN as usize);
        let decoded = decode_header(&encoded).expect("valid header");
        assert!(decoded.in_use);
        assert_eq!(decoded.path, "db/sst/000042.sst");
        assert_eq!(decoded.len, 4096);
        assert_eq!(decoded.generation, 7);
    }

    #[wasm_bindgen_test]
    fn a_torn_header_decodes_as_free() {
        let mut encoded = encode_header(&SlotHeader {
            in_use: true,
            path: "db/MANIFEST".to_string(),
            len: 10,
            generation: 1,
        });
        encoded[12] ^= 0xff;
        assert!(decode_header(&encoded).is_none());
    }

    #[wasm_bindgen_test]
    fn an_unformatted_slot_decodes_as_free() {
        assert!(decode_header(&vec![0u8; SLOT_HEADER_LEN as usize]).is_none());
    }

    #[wasm_bindgen_test]
    fn a_path_longer_than_the_header_is_rejected() {
        let long = PathBuf::from("x".repeat(MAX_PATH_LEN + 1));
        assert!(check_path_fits(&long).is_err());
        assert!(check_path_fits(Path::new("db/MANIFEST")).is_ok());
    }
}
