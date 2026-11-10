use std::io::{Read, Write};

#[cfg(target_arch = "wasm32")]
#[link(wasm_import_module = "gents")]
extern "C" {
    fn host_call(ptr: *const u8, len: usize) -> i32;
    fn read_result(ptr: *mut u8, cap: usize) -> i32;
}

#[cfg(target_arch = "wasm32")]
fn invoke(request: String) -> Result<String, String> {
    // SAFETY: the host reads the request buffer while it is alive and writes
    // at most the capacity of the result buffer in this module's memory.
    let length = unsafe { host_call(request.as_ptr(), request.len()) };
    if length < 0 {
        return Err("host call stopped or exceeded its bounds".into());
    }
    let mut bytes = vec![0; length as usize];
    let written = unsafe { read_result(bytes.as_mut_ptr(), bytes.len()) };
    if written != length {
        return Err("incomplete host result".into());
    }
    String::from_utf8(bytes).map_err(|e| e.to_string())
}

#[cfg(not(target_arch = "wasm32"))]
fn invoke(_: String) -> Result<String, String> {
    Err("host calls require the Gents WASM sandbox".into())
}

fn main() {
    let result = (|| {
        let mut input = String::new();
        std::io::stdin()
            .read_to_string(&mut input)
            .map_err(|e| e.to_string())?;
        let input = serde_json::from_str(&input).map_err(|e| e.to_string())?;
        script::run(&input, invoke)
    })();
    match result {
        Ok(result) => {
            let mut stdout = std::io::stdout().lock();
            serde_json::to_writer(&mut stdout, &result).expect("write result");
            stdout.flush().expect("flush result");
        }
        Err(error) => {
            writeln!(std::io::stderr(), "{error}").ok();
            std::process::exit(1);
        }
    }
}
