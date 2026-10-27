//! The `gents` host imports this plugin declares (`host_imports: ["run_script"]`).
//!
//! `run_script(ptr, len) -> i32` hands the host a JSON document and returns the
//! answer's byte length, or a negative code: `-1` the call carries no script,
//! `-2` the engine failed (its message is the stored bytes), `-3` the bytes were
//! not UTF-8 JSON, `-4` the answer passed the plugin's output cap.
//! `read_result(ptr, cap) -> i32` copies the stored bytes (answer or message)
//! and returns the count copied. The script itself never crosses the boundary:
//! the host evaluates the call's own `script`.
use serde_json::Value;

/// Why the host did not answer with the script's output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostError {
    NoScript,
    Engine(String),
    BadInput(String),
    TooLarge,
}

const NO_SCRIPT: i32 = -1;
const ENGINE_FAILED: i32 = -2;
const BAD_INPUT: i32 = -3;
const TOO_LARGE: i32 = -4;

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "gents")]
extern "C" {
    #[link_name = "run_script"]
    fn host_run_script(ptr: *const u8, len: usize) -> i32;
    #[link_name = "read_result"]
    fn host_read_result(ptr: *mut u8, cap: usize) -> i32;
}

/// Asks the host to run the call's script on `input`.
#[cfg(target_arch = "wasm32")]
pub fn run_script(input: &Value) -> Result<Value, HostError> {
    let bytes = serde_json::to_vec(input).map_err(|e| HostError::BadInput(e.to_string()))?;
    // SAFETY: the host reads exactly `len` bytes at `ptr` from this module's
    // memory, which `bytes` owns for the duration of the call.
    let code = unsafe { host_run_script(bytes.as_ptr(), bytes.len()) };
    let stored = read_stored(if code >= 0 { code as usize } else { 64 * 1024 });
    match code {
        n if n >= 0 => {
            serde_json::from_slice(&stored).map_err(|e| HostError::BadInput(e.to_string()))
        }
        NO_SCRIPT => Err(HostError::NoScript),
        ENGINE_FAILED => Err(HostError::Engine(
            String::from_utf8_lossy(&stored).into_owned(),
        )),
        BAD_INPUT => Err(HostError::BadInput(
            String::from_utf8_lossy(&stored).into_owned(),
        )),
        TOO_LARGE => Err(HostError::TooLarge),
        other => Err(HostError::Engine(format!(
            "the host answered an unknown code {other}"
        ))),
    }
}

#[cfg(target_arch = "wasm32")]
fn read_stored(cap: usize) -> Vec<u8> {
    let mut buffer = vec![0u8; cap];
    // SAFETY: the host writes at most `cap` bytes at `ptr`, the buffer's length.
    let n = unsafe { host_read_result(buffer.as_mut_ptr(), buffer.len()) };
    buffer.truncate(usize::try_from(n).unwrap_or(0));
    buffer
}

/// Off the wasm target there is no host; unit tests inject an evaluator.
#[cfg(not(target_arch = "wasm32"))]
pub fn run_script(_input: &Value) -> Result<Value, HostError> {
    let _ = (NO_SCRIPT, ENGINE_FAILED, BAD_INPUT, TOO_LARGE);
    Err(HostError::Engine(
        "no host: this plugin runs under gents".into(),
    ))
}
