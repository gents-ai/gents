//! The nine OPFS calls regolith needs, reached through `js_sys::Reflect`.
//!
//! Bindings are hand-rolled rather than taken from `web-sys` because
//! `FileSystemSyncAccessHandle` is still an unstable web-sys API, so using
//! it would force `RUSTFLAGS=--cfg=web_sys_unstable_apis` on every
//! downstream build. The surface is small enough that `Reflect` costs less
//! than that constraint.
//!
//! Every function returns `Result<_, JsValue>`: a thrown JS exception
//! becomes an `Err`, never a trap, so no OPFS failure can abort the module.

use js_sys::{Function, Object, Promise, Reflect, Uint8Array};
use wasm_bindgen::{JsCast, JsValue};
use wasm_bindgen_futures::JsFuture;

/// Largest single read or write handed to a sync access handle. Buffers
/// are addressed with a JS `Number`, and `Uint8Array::new_with_length`
/// takes a `u32`, so a request is chunked above this.
pub(super) const MAX_CHUNK: usize = 8 * 1024 * 1024;

fn err(message: impl AsRef<str>) -> JsValue {
    JsValue::from_str(message.as_ref())
}

/// Look up a callable property, reporting a clear error when the host
/// does not provide it (an old browser, or a non-secure context).
fn method(target: &JsValue, name: &str) -> Result<Function, JsValue> {
    let value = Reflect::get(target, &JsValue::from_str(name))?;
    value
        .dyn_into::<Function>()
        .map_err(|_| err(format!("OPFS: `{name}` is unavailable on this host")))
}

async fn await_promise(value: JsValue, what: &str) -> Result<JsValue, JsValue> {
    let promise: Promise = value
        .dyn_into()
        .map_err(|_| err(format!("OPFS: `{what}` did not return a Promise")))?;
    JsFuture::from(promise).await
}

fn number(value: &JsValue, what: &str) -> Result<f64, JsValue> {
    value
        .as_f64()
        .ok_or_else(|| err(format!("OPFS: `{what}` did not return a number")))
}

/// `navigator.storage.getDirectory()`: the origin private root.
///
/// Fails when the realm has no `navigator.storage`, which is what a
/// non-secure context (plain `http://` on a non-loopback host) looks like
/// from inside wasm.
pub(super) async fn root_directory() -> Result<JsValue, JsValue> {
    let global = js_sys::global();
    let navigator = Reflect::get(&global, &JsValue::from_str("navigator"))?;
    if navigator.is_undefined() || navigator.is_null() {
        return Err(err("OPFS: this realm has no `navigator`"));
    }
    let storage = Reflect::get(&navigator, &JsValue::from_str("storage"))?;
    if storage.is_undefined() || storage.is_null() {
        return Err(err(
            "OPFS: `navigator.storage` is unavailable; OPFS needs a secure context (https or localhost)",
        ));
    }
    let get_directory = method(&storage, "getDirectory")?;
    await_promise(get_directory.call0(&storage)?, "getDirectory").await
}

/// `parent.getDirectoryHandle(name, { create })`.
pub(super) async fn directory_handle(
    parent: &JsValue,
    name: &str,
    create: bool,
) -> Result<JsValue, JsValue> {
    let options = Object::new();
    Reflect::set(&options, &JsValue::from_str("create"), &create.into())?;
    let get = method(parent, "getDirectoryHandle")?;
    await_promise(
        get.call2(parent, &JsValue::from_str(name), &options)?,
        "getDirectoryHandle",
    )
    .await
}

/// `dir.getFileHandle(name, { create })`.
pub(super) async fn file_handle(
    dir: &JsValue,
    name: &str,
    create: bool,
) -> Result<JsValue, JsValue> {
    let options = Object::new();
    Reflect::set(&options, &JsValue::from_str("create"), &create.into())?;
    let get = method(dir, "getFileHandle")?;
    await_promise(
        get.call2(dir, &JsValue::from_str(name), &options)?,
        "getFileHandle",
    )
    .await
}

/// `dir.removeEntry(name)`.
pub(super) async fn remove_entry(dir: &JsValue, name: &str) -> Result<(), JsValue> {
    let remove = method(dir, "removeEntry")?;
    await_promise(remove.call1(dir, &JsValue::from_str(name))?, "removeEntry").await?;
    Ok(())
}

/// A walk of the file entries directly under an OPFS directory, through
/// the directory's async `values()` iterator.
///
/// Each [`FileWalk::next_file`] awaits one `next()` promise, the way every
/// other asynchronous OPFS call here is awaited, so a walk holds one entry
/// at a time and never the directory: a mount over a directory of any size
/// costs the same memory. Subdirectories are skipped: regolith's OPFS
/// layout is flat.
pub(super) struct FileWalk {
    iterator: JsValue,
    next: Function,
}

impl FileWalk {
    /// Start a walk of `dir`.
    pub(super) fn new(dir: &JsValue) -> Result<Self, JsValue> {
        let values = method(dir, "values")?;
        let iterator = values.call0(dir)?;
        let next = method(&iterator, "next")?;
        Ok(Self { iterator, next })
    }

    /// The next file entry as `(name, FileSystemFileHandle)`, or `None`
    /// once the directory is exhausted.
    pub(super) async fn next_file(&mut self) -> Result<Option<(String, JsValue)>, JsValue> {
        loop {
            let step = await_promise(self.next.call0(&self.iterator)?, "values().next").await?;
            if Reflect::get(&step, &JsValue::from_str("done"))?
                .as_bool()
                .unwrap_or(true)
            {
                return Ok(None);
            }
            let handle = Reflect::get(&step, &JsValue::from_str("value"))?;
            let kind = Reflect::get(&handle, &JsValue::from_str("kind"))?;
            if kind.as_string().as_deref() != Some("file") {
                continue;
            }
            if let Some(name) = Reflect::get(&handle, &JsValue::from_str("name"))?.as_string() {
                return Ok(Some((name, handle)));
            }
        }
    }
}

/// The size in bytes of the file behind `file_handle`, through `getFile()`,
/// without reading its contents.
pub(super) async fn file_size(file_handle: &JsValue) -> Result<u64, JsValue> {
    let get_file = method(file_handle, "getFile")?;
    let file = await_promise(get_file.call0(file_handle)?, "getFile").await?;
    let size = number(&Reflect::get(&file, &JsValue::from_str("size"))?, "size")?;
    Ok(size as u64)
}

/// `fileHandle.createSyncAccessHandle()`.
///
/// Rejects outside a worker: every browser that ships OPFS restricts sync
/// access handles to worker scopes. That rejection is the probe that
/// selects [`super::OpfsMode::Mirror`].
pub(super) async fn sync_access_handle(file: &JsValue) -> Result<JsValue, JsValue> {
    let create = method(file, "createSyncAccessHandle")?;
    await_promise(create.call0(file)?, "createSyncAccessHandle").await
}

/// `handle.read(buffer, { at })`. Returns the bytes actually read, which
/// is short at end of file.
pub(super) fn read_at(handle: &JsValue, at: u64, buf: &mut [u8]) -> Result<usize, JsValue> {
    let mut done = 0usize;
    while done < buf.len() {
        let want = (buf.len() - done).min(MAX_CHUNK);
        let scratch = Uint8Array::new_with_length(want as u32);
        let options = Object::new();
        Reflect::set(
            &options,
            &JsValue::from_str("at"),
            &JsValue::from_f64((at + done as u64) as f64),
        )?;
        let read = method(handle, "read")?;
        let got = number(&read.call2(handle, &scratch, &options)?, "read")? as usize;
        let got = got.min(want);
        if got == 0 {
            break;
        }
        // A `Uint8Array` view over wasm linear memory would save this copy
        // but needs `unsafe`; the crate forbids it, so JS owns the landing
        // buffer and the bytes are copied across.
        scratch
            .subarray(0, got as u32)
            .copy_to(&mut buf[done..done + got]);
        done += got;
    }
    Ok(done)
}

/// `handle.write(buffer, { at })`. Returns the bytes accepted; a short
/// write means the origin's storage quota is exhausted.
pub(super) fn write_at(handle: &JsValue, at: u64, buf: &[u8]) -> Result<usize, JsValue> {
    let mut done = 0usize;
    while done < buf.len() {
        let want = (buf.len() - done).min(MAX_CHUNK);
        let chunk = Uint8Array::from(&buf[done..done + want]);
        let options = Object::new();
        Reflect::set(
            &options,
            &JsValue::from_str("at"),
            &JsValue::from_f64((at + done as u64) as f64),
        )?;
        let write = method(handle, "write")?;
        let put = number(&write.call2(handle, &chunk, &options)?, "write")? as usize;
        let put = put.min(want);
        if put == 0 {
            break;
        }
        done += put;
    }
    Ok(done)
}

/// `handle.flush()`: make everything written so far durable.
pub(super) fn flush(handle: &JsValue) -> Result<(), JsValue> {
    method(handle, "flush")?.call0(handle)?;
    Ok(())
}

/// `handle.truncate(len)`.
pub(super) fn truncate(handle: &JsValue, len: u64) -> Result<(), JsValue> {
    method(handle, "truncate")?.call1(handle, &JsValue::from_f64(len as f64))?;
    Ok(())
}

/// `handle.close()`. Errors are reported so a caller draining a pool can
/// see them; releasing a handle twice is the only way this fails.
pub(super) fn close(handle: &JsValue) -> Result<(), JsValue> {
    method(handle, "close")?.call0(handle)?;
    Ok(())
}

/// Overwrite an OPFS file with `data` through
/// `FileSystemWritableFileStream`, creating it when absent. This is the
/// only write path available off a worker thread.
pub(super) async fn write_whole_file(
    dir: &JsValue,
    name: &str,
    data: &[u8],
) -> Result<(), JsValue> {
    let handle = file_handle(dir, name, true).await?;
    let create_writable = method(&handle, "createWritable")?;
    let writable = await_promise(create_writable.call0(&handle)?, "createWritable").await?;

    let write = method(&writable, "write")?;
    await_promise(
        write.call1(&writable, &Uint8Array::from(data))?,
        "writable.write",
    )
    .await?;

    let close = method(&writable, "close")?;
    await_promise(close.call0(&writable)?, "writable.close").await?;
    Ok(())
}

/// Read an entire OPFS file through `getFile()` + `arrayBuffer()`.
pub(super) async fn read_whole_file(file_handle: &JsValue) -> Result<Vec<u8>, JsValue> {
    let get_file = method(file_handle, "getFile")?;
    let file = await_promise(get_file.call0(file_handle)?, "getFile").await?;

    let array_buffer = method(&file, "arrayBuffer")?;
    let buffer = await_promise(array_buffer.call0(&file)?, "arrayBuffer").await?;

    Ok(Uint8Array::new(&buffer).to_vec())
}

/// `performance.now()` in milliseconds, or `None` where the realm has no
/// monotonic clock. `Date.now()` is deliberately not substituted: it can
/// step backwards, and a duration measured against it would be wrong.
pub(super) fn monotonic_ms() -> Option<f64> {
    let global = js_sys::global();
    let performance = Reflect::get(&global, &JsValue::from_str("performance")).ok()?;
    if performance.is_undefined() || performance.is_null() {
        return None;
    }
    let now = Reflect::get(&performance, &JsValue::from_str("now"))
        .ok()?
        .dyn_into::<Function>()
        .ok()?;
    now.call0(&performance).ok()?.as_f64()
}

/// `Date.now()` in milliseconds since the Unix epoch.
pub(super) fn wall_clock_ms() -> f64 {
    js_sys::Date::now()
}
