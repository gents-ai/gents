//! Probes, in the real webview with the app's own capabilities, whether a
//! plugin page's sandboxed frame can reach the app's IPC (#3483).
//!
//! The probe page is served from a scheme this builder registers, which Tauri
//! treats as a local origin, in a window labelled `main`, so the app's
//! capabilities apply to it exactly as to the shell. The page proves the IPC
//! answers it, then runs a frame sandboxed as plugin pages are
//! (`allow-scripts`, srcdoc, so its origin is opaque) that tries every route
//! into the IPC. The page carries no CSP, so the result rests on the sandbox
//! and Tauri's own checks alone; the shell and frame policies are tested
//! separately and only narrow it further.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{json, Value};
use tauri::Listener;

const SCHEME: &str = "gentsprobe";
const EVENT: &str = "frame-probe";

const PAGE: &str = r#"<!doctype html>
<meta charset="utf-8">
<body>
<script src="probe.js"></script>
</body>"#;

/* the parent proves it reaches the IPC, then hands the frame its routes and
forwards what the frame saw; only the parent's own emits may arrive */
const PARENT: &str = r#"
const emit = (payload) =>
  window.__TAURI_INTERNALS__.invoke("plugin:event|emit", { event: "frame-probe", payload });
emit({ from: "parent" });
window.addEventListener("message", (event) => {
  if (event.data && event.data.kind === "frame-report") emit({ from: "report", report: event.data.report });
});
fetch("frame.js").then((r) => r.text()).then((source) => {
  const frame = document.createElement("iframe");
  frame.setAttribute("sandbox", "allow-scripts");
  frame.srcdoc = "<!doctype html><script>" + source + "<\/script>";
  document.body.appendChild(frame);
});
"#;

/* every route a page in the frame could take to the app's IPC; each records
what it found, and a payload that reached the app would name its route */
const FRAME: &str = r#"
const report = { origin: String(self.origin) };
const body = (route) => JSON.stringify({ event: "frame-probe", payload: { from: route } });
const headers = {
  "Content-Type": "application/json",
  "Tauri-Callback": "1",
  "Tauri-Error": "2",
  "Tauri-Invoke-Key": "",
};
const attempt = async (name, run) => {
  try { report[name] = String(await run()); } catch (error) { report[name] = "refused: " + error; }
};
(async () => {
  await attempt("internals", () => {
    const internals = window.__TAURI_INTERNALS__;
    if (!internals) return "absent";
    internals.invoke("plugin:event|emit", { event: "frame-probe", payload: { from: "frame-internals" } });
    return "present";
  });
  await attempt("messageHandler", () => {
    const handler = window.webkit && window.webkit.messageHandlers && window.webkit.messageHandlers.ipc;
    if (!handler) return "absent";
    handler.postMessage(JSON.stringify({
      cmd: "plugin:event|emit", callback: 1, error: 2,
      payload: { event: "frame-probe", payload: { from: "frame-message-handler" } },
      options: {}, invokeKey: "",
    }));
    return "present";
  });
  await attempt("ipcScheme", () =>
    fetch("ipc://localhost/plugin%3Aevent%7Cemit", { method: "POST", body: body("frame-ipc-scheme"), headers })
      .then((r) => "status " + r.status));
  await attempt("ipcHost", () =>
    fetch("http://ipc.localhost/plugin%3Aevent%7Cemit", { method: "POST", body: body("frame-ipc-host"), headers })
      .then((r) => "status " + r.status));
  await attempt("parentInternals", () => typeof parent.__TAURI_INTERNALS__);
  parent.postMessage({ kind: "frame-report", report }, "*");
})();
"#;

#[derive(Default)]
struct Seen {
    parent: bool,
    report: Option<Value>,
    frame: Vec<Value>,
}

/// Runs the probe and writes what reached the IPC to `receipt`.
pub fn verify_frame_ipc(receipt: &Path) -> anyhow::Result<()> {
    let mut context = crate::bridge::context();
    for window in &mut context.config_mut().app.windows {
        window.create = false;
    }
    let seen = Arc::new(Mutex::new(Seen::default()));
    let receipt = receipt.to_path_buf();
    let app = tauri::Builder::default()
        .register_uri_scheme_protocol(SCHEME, |_, request| {
            let (body, kind) = match request.uri().path() {
                "/probe.js" => (PARENT, "text/javascript"),
                "/frame.js" => (FRAME, "text/javascript"),
                _ => (PAGE, "text/html"),
            };
            tauri::http::Response::builder()
                .header("Content-Type", kind)
                .body(body.as_bytes().to_vec())
                .expect("a static response")
        })
        .setup({
            let seen = seen.clone();
            move |app| {
                app.listen_any(EVENT, {
                    let seen = seen.clone();
                    move |event| {
                        let Ok(payload) = serde_json::from_str::<Value>(event.payload()) else {
                            return;
                        };
                        let mut seen = seen.lock().expect("probe state");
                        match payload["from"].as_str() {
                            Some("parent") => seen.parent = true,
                            Some("report") => seen.report = Some(payload["report"].clone()),
                            _ => seen.frame.push(payload),
                        }
                    }
                });
                tauri::WebviewWindowBuilder::new(
                    app,
                    "main",
                    tauri::WebviewUrl::CustomProtocol(
                        format!("{SCHEME}://localhost/index.html").parse()?,
                    ),
                )
                .incognito(true)
                .visible(false)
                .build()?;
                let handle = app.handle().clone();
                let seen = seen.clone();
                std::thread::spawn(move || write_when_settled(&handle, &seen, &receipt));
                Ok(())
            }
        })
        .build(context)?;
    app.run(|_, _| {});
    Ok(())
}

#[expect(
    clippy::disallowed_methods,
    reason = "a dedicated OS thread of a test-only probe, off the runtime"
)]
fn write_when_settled(handle: &tauri::AppHandle, seen: &Mutex<Seen>, receipt: &Path) {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while std::time::Instant::now() < deadline && seen.lock().expect("probe state").report.is_none()
    {
        std::thread::sleep(Duration::from_millis(100));
    }
    // The frame's fetches and posts may settle after its report; a late
    // arrival is still caught within this grace period.
    std::thread::sleep(Duration::from_secs(2));
    let seen = seen.lock().expect("probe state");
    let written = std::fs::write(
        receipt,
        json!({
            "parentReached": seen.parent,
            "report": seen.report,
            "frameReached": seen.frame,
        })
        .to_string(),
    );
    handle.exit(if written.is_ok() { 0 } else { 1 });
}
