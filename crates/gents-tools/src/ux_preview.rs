//! `gents config pack ux preview`: one UX plugin rendered by a real browser, at light and
//! dark, through the desktop's frame pipeline and the real node data path,
//! with a report a model can use without vision.
//!
//! The browser only ever talks to this module: every request the preview
//! page makes (the prebuilt page, the plugin's module, each `ctx.data` call)
//! is intercepted in the DevTools session and fulfilled here, so this module
//! listens on no socket and a page's data goes through
//! [`super::ux_data`] exactly as the desktop's does. A preview
//! writes nothing: a page's create tools and `ctx.plugin` are refused at the
//! door and reported.

mod browser;
pub mod report;

use std::path::{Component, Path, PathBuf};
use std::str::FromStr;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::{mpsc, OnceCell};

use self::browser::{Browser, Cdp, Event};
use self::report::{Audit, ConsoleEntry, DoorCall, PageStatus, ThemeReport, UxPreviewReport};
use super::ux_data::{self, UxDataCall};
use gents_pack::plugin::ux::{self, file_module, UxDoor, UxListing, UxModule, UxProducer};
use gents_store::config_client::ConfigAccess;

const AUDIT_JS: &str = include_str!("ux_preview/audit.js");
const PAGE_ORIGIN: &str = "https://ux-preview.gents.invalid";
const PAGE_ENTRY: &str = "ux-preview.html";
const DOOR_PREFIX: &str = "/__preview/";
const ASSETS_ENV: &str = "GENTS_UX_PREVIEW_ASSETS";
const ASSETS_DEV_DIR: &str = "apps/gents-desktop/dist-ux-preview";
const THEMES: [&str; 2] = ["light", "dark"];
const MAX_RECORDED_CALLS: usize = 100;
const MAX_CONSOLE_ENTRIES: usize = 40;
const MAX_CONSOLE_CHARS: usize = 600;
const RUN_DEADLINE: Duration = Duration::from_secs(150);
const BLOB_ORIGIN: &str = "blob:null/";
const BLOB_ID_CHARS: usize = 36;

/// The pane the page is drawn in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Viewport {
    pub width: u32,
    pub height: u32,
}

impl Default for Viewport {
    fn default() -> Self {
        Self {
            width: 1280,
            height: 800,
        }
    }
}

impl FromStr for Viewport {
    type Err = anyhow::Error;

    fn from_str(text: &str) -> Result<Self> {
        let (width, height) = text
            .split_once('x')
            .and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)))
            .with_context(|| format!("viewport must look like 1280x800, got {text:?}"))?;
        Ok(Self { width, height })
    }
}

/// What a preview renders against: its node and where its files land.
pub struct PreviewRequest {
    pub home: PathBuf,
    pub access: ConfigAccess,
    pub node_did: String,
    pub out: PathBuf,
    pub viewport: Viewport,
}

/// A plugin ready to preview: an installed listing, or a draft file that
/// borrows the grants of the installed plugin its module's `id` names.
pub struct PreviewTarget {
    pub label: String,
    listing: Option<UxListing>,
    file: String,
    module: UxModule,
}

/// A pack page and a page document declare what they may register and call,
/// so the frame is told its listing; a dev plugin declares nothing.
fn carries_grants(listing: &UxListing) -> bool {
    matches!(listing.door, UxDoor::Pack | UxDoor::Document)
}

impl PreviewTarget {
    /// A page document, previewed under its own declared data grants.
    pub fn page(page: &gents_documents::PageDocument) -> Result<Self> {
        let listing = ux::page::listing(page)?;
        Ok(Self {
            label: listing.id.clone(),
            file: listing.file.clone(),
            module: ux::page::module(page),
            listing: Some(listing),
        })
    }
}

fn declared_plugin_id(listing: &UxListing) -> &str {
    listing.id.rsplit('/').next().unwrap_or(&listing.id)
}

pub(crate) fn listing_named(home: &Path, id: &str) -> Result<Option<UxListing>> {
    Ok(ux::list(home)?
        .into_iter()
        .find(|listing| listing.id == id || declared_plugin_id(listing) == id))
}

/// Resolves `target` (a listing id, a plugin's bare name or a `plugin.js`
/// path) and produces its module; `run_afb` runs a pack's `.afb` producer as
/// in [`ux::resolve_module`].
pub async fn resolve_target<F, Fut>(home: &Path, target: &str, run_afb: F) -> Result<PreviewTarget>
where
    F: FnOnce(String, String) -> Fut,
    Fut: std::future::Future<Output = Result<Value>>,
{
    if let Some(listing) = listing_named(home, target)? {
        let module = ux::resolve_module(home, &listing, run_afb).await?;
        return Ok(PreviewTarget {
            label: listing.id.clone(),
            file: listing.file.clone(),
            listing: Some(listing),
            module,
        });
    }
    let path = Path::new(target);
    anyhow::ensure!(
        path.is_file(),
        "no ux plugin {target:?} under {} and no such file; `ux list` names the installed plugins, or pass the path to a plugin.js",
        home.display()
    );
    let file = path
        .canonicalize()
        .with_context(|| format!("resolving {}", path.display()))?
        .to_string_lossy()
        .into_owned();
    let draft = UxListing {
        id: file.clone(),
        door: UxDoor::Dev,
        producer: UxProducer::File,
        file: file.clone(),
        pack: None,
        description: None,
        contributes: None,
        default_enabled: None,
        data: Vec::new(),
        digest: None,
    };
    let module = file_module(home, &draft, None)?;
    Ok(PreviewTarget {
        label: file.clone(),
        file,
        listing: None,
        module,
    })
}

/// Whether this build can render a page: an installed build carries no
/// preview page, so `ux preview` has nothing to load there.
pub fn renderer_available() -> bool {
    assets_dir().is_ok()
}

fn assets_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os(ASSETS_ENV) {
        return Ok(PathBuf::from(dir));
    }
    let exe = std::env::current_exe().context("locating this binary")?;
    exe.ancestors()
        .map(|dir| dir.join(ASSETS_DEV_DIR))
        .find(|dir| dir.join(PAGE_ENTRY).is_file())
        .context(
            "this build has no page renderer, so ux preview renders nothing here and no config call adds one. The available check is page lint ID for a page document, or ux lint PATH for a pack page; the page itself is seen in the desktop app",
        )
}

/// Everything the page asks of the runtime, for one preview.
struct Door {
    home: PathBuf,
    access: ConfigAccess,
    node_did: String,
    target: PreviewTarget,
    assets: PathBuf,
    surfaces: OnceCell<Value>,
    calls: Mutex<Vec<DoorCall>>,
    calls_omitted: Mutex<u64>,
    /// The listing a draft took its grants from, once its module named its id.
    bound: Mutex<Option<String>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct SurfacesRequest {
    ux: String,
    node_did: String,
}

#[derive(Deserialize)]
struct BindRequest {
    id: String,
}

#[derive(Deserialize)]
struct PluginRequest {
    name: String,
}

fn parse<'a, T>(body: &'a str, verb: &str) -> Result<T>
where
    T: Deserialize<'a>,
{
    serde_json::from_str(body).with_context(|| format!("decoding the {verb} request"))
}

impl Door {
    fn binding(listing: Option<&UxListing>) -> (Option<String>, Value) {
        match listing {
            Some(listing) => (
                carries_grants(listing).then(|| listing.id.clone()),
                json!(listing.contributes),
            ),
            None => (None, Value::Null),
        }
    }

    fn target(&self) -> Value {
        let (listing, declared) = Self::binding(self.target.listing.as_ref());
        json!({
            "source": self.target.module.source,
            "css": self.target.module.css,
            "file": self.target.file,
            "nodeDid": self.node_did,
            "pluginId": self.target.listing.as_ref().map(declared_plugin_id),
            "listing": listing,
            "declared": declared,
        })
    }

    fn record(&self, surface: &str, tool: &str, args: Value, outcome: &Result<Value>) {
        let mut calls = self.calls.lock().unwrap_or_else(PoisonError::into_inner);
        if calls.len() >= MAX_RECORDED_CALLS {
            *self
                .calls_omitted
                .lock()
                .unwrap_or_else(PoisonError::into_inner) += 1;
            return;
        }
        let (rows, error) = match outcome {
            Ok(value) => (
                value["output"]["results"]
                    .as_array()
                    .map(|rows| rows.len() as u64)
                    .or_else(|| value["output"]["count"].as_u64()),
                None,
            ),
            Err(error) => (None, Some(format!("{error:#}"))),
        };
        calls.push(DoorCall {
            surface: surface.to_owned(),
            tool: tool.to_owned(),
            args,
            rows,
            error,
        });
    }

    async fn data_call(&self, call: UxDataCall) -> Result<Value> {
        let surfaces = self
            .surfaces
            .get_or_try_init(|| {
                ux_data::surfaces(&self.home, &self.access, &call.ux, &call.node_did)
            })
            .await?;
        let is_create = surfaces["surfaces"].as_array().is_some_and(|surfaces| {
            surfaces.iter().any(|surface| {
                surface["surface"] == call.surface.as_str()
                    && surface["tools"].as_array().is_some_and(|tools| {
                        tools.iter().any(|tool| {
                            tool["tool_name"] == call.tool.as_str() && tool["kind"] == "create"
                        })
                    })
            })
        });
        anyhow::ensure!(
            !is_create,
            "create tool {:?} does not run in preview",
            call.tool
        );
        ux_data::call(&self.home, &self.access, call).await
    }

    async fn dispatch(&self, verb: &str, body: &str) -> Result<Value> {
        match verb {
            "target" => Ok(self.target()),
            "bind" => {
                let request: BindRequest = parse(body, verb)?;
                let listing = listing_named(&self.home, &request.id)?;
                let (listing, declared) = Self::binding(listing.as_ref());
                *self.bound.lock().unwrap_or_else(PoisonError::into_inner) = listing.clone();
                Ok(json!({ "listing": listing, "declared": declared }))
            }
            "data/call" => {
                let call: UxDataCall = parse(body, verb)?;
                let (surface, tool, args) =
                    (call.surface.clone(), call.tool.clone(), call.args.clone());
                let outcome = self.data_call(call).await;
                self.record(&surface, &tool, args, &outcome);
                outcome
            }
            "data/surfaces" => {
                let request: SurfacesRequest = parse(body, verb)?;
                ux_data::surfaces(&self.home, &self.access, &request.ux, &request.node_did).await
            }
            "plugin" => {
                let request: PluginRequest = parse(body, verb)?;
                let refusal = Err(anyhow!("ctx.plugin does not run in preview"));
                self.record("<plugin>", &request.name, Value::Null, &refusal);
                refusal
            }
            other => bail!("unknown preview door {other:?}"),
        }
    }

    async fn answer(&self, verb: &str, body: &str) -> Value {
        match self.dispatch(verb, body).await {
            Ok(value) => json!({ "value": value }),
            Err(error) => json!({ "error": format!("{error:#}") }),
        }
    }

    async fn asset(&self, path: &str) -> (u16, &'static str, Vec<u8>) {
        let relative = Path::new(path.trim_start_matches('/'));
        if relative
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
        {
            return (404, "text/plain", Vec::new());
        }
        match tokio::fs::read(self.assets.join(relative)).await {
            Ok(body) => {
                let content_type = match relative.extension().and_then(|ext| ext.to_str()) {
                    Some("html") => "text/html",
                    Some("js") => "text/javascript",
                    Some("css") => "text/css",
                    Some("svg") => "image/svg+xml",
                    Some("png") => "image/png",
                    Some("woff2") => "font/woff2",
                    Some("json") => "application/json",
                    _ => "application/octet-stream",
                };
                (200, content_type, body)
            }
            Err(_) => (404, "text/plain", Vec::new()),
        }
    }
}

/// What the page and its frame report as they run, per theme.
#[derive(Default)]
struct Capture {
    console: Vec<ConsoleEntry>,
    console_omitted: u64,
    frame_session: Option<String>,
}

/// A frame's scripts load from per-load blob URLs; without the random id a
/// finding is the same finding in both themes.
fn without_blob_ids(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find(BLOB_ORIGIN) {
        out.push_str(&rest[..at]);
        out.push_str("<frame module>");
        rest = rest
            .get(at + BLOB_ORIGIN.len() + BLOB_ID_CHARS..)
            .unwrap_or_default();
    }
    out.push_str(rest);
    out
}

impl Capture {
    fn console(&mut self, level: &str, text: String) {
        let text = without_blob_ids(&text);
        let text = text.trim();
        let mut text = text.chars().take(MAX_CONSOLE_CHARS).collect::<String>();
        if text.is_empty() {
            text.push_str("(empty)");
        }
        if self.console.iter().any(|entry| entry.text == text) {
            return;
        }
        if self.console.len() >= MAX_CONSOLE_ENTRIES {
            self.console_omitted += 1;
            return;
        }
        self.console.push(ConsoleEntry {
            level: level.to_owned(),
            text,
        });
    }
}

type Shared = Arc<Mutex<Capture>>;

fn lock(shared: &Shared) -> std::sync::MutexGuard<'_, Capture> {
    shared.lock().unwrap_or_else(PoisonError::into_inner)
}

fn console_text(params: &Value) -> String {
    params["args"]
        .as_array()
        .map(|args| {
            args.iter()
                .map(|arg| {
                    arg["value"]
                        .as_str()
                        .map(ToOwned::to_owned)
                        .or_else(|| arg["description"].as_str().map(ToOwned::to_owned))
                        .unwrap_or_else(|| arg["value"].to_string())
                })
                .collect::<Vec<_>>()
                .join(" ")
        })
        .unwrap_or_default()
}

fn exception_text(params: &Value) -> String {
    let details = &params["exceptionDetails"];
    details["exception"]["description"]
        .as_str()
        .or_else(|| details["text"].as_str())
        .unwrap_or("uncaught exception")
        .to_owned()
}

/// Answers one intercepted request from the assets or the door.
async fn fulfil(cdp: Arc<Cdp>, door: Arc<Door>, root: String, params: Value) {
    let request_id = params["requestId"].as_str().unwrap_or_default().to_owned();
    let request = &params["request"];
    let url = request["url"].as_str().unwrap_or_default();
    let path = url
        .strip_prefix(PAGE_ORIGIN)
        .and_then(|rest| rest.split(['?', '#']).next())
        .unwrap_or("/");
    let (status, content_type, body) = if let Some(verb) = path.strip_prefix(DOOR_PREFIX) {
        let post = match request["postData"].as_str() {
            Some(post) => post.to_owned(),
            None => cdp
                .send(
                    Some(&root),
                    "Fetch.getRequestPostData",
                    json!({ "requestId": request_id }),
                )
                .await
                .ok()
                .and_then(|value| value["postData"].as_str().map(ToOwned::to_owned))
                .unwrap_or_default(),
        };
        let answer = door.answer(verb, &post).await;
        (200, "application/json", answer.to_string().into_bytes())
    } else {
        door.asset(path).await
    };
    let encoded = base64::engine::general_purpose::STANDARD.encode(body);
    if let Err(error) = cdp
        .send(
            Some(&root),
            "Fetch.fulfillRequest",
            json!({
                "requestId": request_id,
                "responseCode": status,
                "responseHeaders": [{ "name": "Content-Type", "value": content_type }],
                "body": encoded,
            }),
        )
        .await
    {
        tracing::warn!(error = %format!("{error:#}"), url, "ux preview request not fulfilled");
    }
}

fn pump(
    cdp: Arc<Cdp>,
    door: Arc<Door>,
    root: String,
    capture: Shared,
    mut events: mpsc::UnboundedReceiver<Event>,
) {
    tokio::spawn(async move {
        while let Some(Event { method, params }) = events.recv().await {
            match method.as_str() {
                "Fetch.requestPaused" => {
                    tokio::spawn(fulfil(cdp.clone(), door.clone(), root.clone(), params));
                }
                "Target.attachedToTarget" => {
                    let session = params["sessionId"].as_str().unwrap_or_default().to_owned();
                    let frame = params["targetInfo"]["type"] == "iframe";
                    if frame {
                        lock(&capture).frame_session = Some(session.clone());
                    }
                    let cdp = cdp.clone();
                    tokio::spawn(async move {
                        if frame {
                            let _ = cdp.send(Some(&session), "Runtime.enable", json!({})).await;
                            let _ = cdp.send(Some(&session), "Log.enable", json!({})).await;
                        }
                        let _ = cdp
                            .send(Some(&session), "Runtime.runIfWaitingForDebugger", json!({}))
                            .await;
                    });
                }
                "Runtime.consoleAPICalled" if params["type"] == "error" => {
                    lock(&capture).console("error", console_text(&params));
                }
                "Runtime.exceptionThrown" => {
                    lock(&capture).console("exception", exception_text(&params));
                }
                "Log.entryAdded" if params["entry"]["level"] == "error" => {
                    let entry = &params["entry"];
                    let text = entry["text"].as_str().unwrap_or_default();
                    let text = match entry["url"].as_str() {
                        Some(url) => format!("{text} ({url})"),
                        None => text.to_owned(),
                    };
                    lock(&capture).console("error", text);
                }
                _ => {}
            }
        }
    });
}

async fn evaluate(cdp: &Cdp, session: &str, expression: &str) -> Result<Value> {
    let result = cdp
        .send(
            Some(session),
            "Runtime.evaluate",
            json!({ "expression": expression, "awaitPromise": true, "returnByValue": true }),
        )
        .await?;
    if result.get("exceptionDetails").is_some() {
        bail!("{}", exception_text(&result));
    }
    Ok(result["result"]["value"].clone())
}

const WAIT_SETTLED_JS: &str = r#"new Promise((resolve) => {
  const started = Date.now();
  const tick = () => {
    if (window.__UX_PREVIEW__) window.__UX_PREVIEW__.settled().then(resolve);
    else if (Date.now() - started > 15000) resolve({ loaded: false, error: "the preview page did not start" });
    else setTimeout(tick, 25);
  };
  tick();
})"#;

async fn render_theme(
    cdp: &Cdp,
    root: &str,
    capture: &Shared,
    door: &Door,
    theme: &str,
    out: &Path,
) -> Result<ThemeReport> {
    door.calls
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clear();
    *door
        .calls_omitted
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = 0;
    *lock(capture) = Capture::default();
    cdp.send(
        Some(root),
        "Emulation.setEmulatedMedia",
        json!({ "features": [{ "name": "prefers-color-scheme", "value": theme }] }),
    )
    .await?;
    let navigation = cdp
        .send(
            Some(root),
            "Page.navigate",
            json!({ "url": format!("{PAGE_ORIGIN}/{PAGE_ENTRY}?theme={theme}") }),
        )
        .await?;
    if let Some(error) = navigation["errorText"].as_str() {
        bail!("the preview page did not open: {error}");
    }
    let status: PageStatus = serde_json::from_value(evaluate(cdp, root, WAIT_SETTLED_JS).await?)
        .context("reading the page state")?;
    let frame = lock(capture).frame_session.clone();
    let (audit, audit_error) = match frame {
        Some(frame) if status.loaded && status.page.is_some() => {
            match evaluate(cdp, &frame, AUDIT_JS).await.and_then(|value| {
                serde_json::from_value::<Audit>(value).context("reading the audit")
            }) {
                Ok(audit) => (Some(audit), None),
                Err(error) => (None, Some(format!("{error:#}"))),
            }
        }
        None if status.loaded && status.page.is_some() => (
            None,
            Some("the browser did not attach the sandbox frame".to_owned()),
        ),
        _ => (None, None),
    };
    let shot = cdp
        .send(
            Some(root),
            "Page.captureScreenshot",
            json!({ "format": "png" }),
        )
        .await?;
    let png = base64::engine::general_purpose::STANDARD
        .decode(shot["data"].as_str().context("screenshot data")?)
        .context("decoding the screenshot")?;
    let path = out.join(format!("{theme}.png"));
    tokio::fs::write(&path, png)
        .await
        .with_context(|| format!("writing {}", path.display()))?;
    let captured = std::mem::take(&mut *lock(capture));
    let data_calls =
        std::mem::take(&mut *door.calls.lock().unwrap_or_else(PoisonError::into_inner));
    let data_calls_omitted = *door
        .calls_omitted
        .lock()
        .unwrap_or_else(PoisonError::into_inner);
    Ok(ThemeReport {
        theme: theme.to_owned(),
        png: path,
        status,
        audit,
        audit_error,
        console: captured.console,
        console_omitted: captured.console_omitted,
        data_calls,
        data_calls_omitted,
    })
}

async fn drive(
    browser: &mut Browser,
    request: &PreviewRequest,
    door: Arc<Door>,
) -> Result<Vec<ThemeReport>> {
    let cdp = browser.cdp.clone();
    let target = cdp
        .send(None, "Target.createTarget", json!({ "url": "about:blank" }))
        .await?;
    let attached = cdp
        .send(
            None,
            "Target.attachToTarget",
            json!({ "targetId": target["targetId"], "flatten": true }),
        )
        .await?;
    let root = attached["sessionId"]
        .as_str()
        .context("the browser opened no page session")?
        .to_owned();
    let capture: Shared = Arc::default();
    pump(
        cdp.clone(),
        door.clone(),
        root.clone(),
        capture.clone(),
        browser
            .events
            .take()
            .context("the browser's events were taken")?,
    );
    for (method, params) in [
        ("Page.enable", json!({})),
        ("Runtime.enable", json!({})),
        ("Log.enable", json!({})),
        (
            "Fetch.enable",
            json!({ "patterns": [{ "urlPattern": "*" }] }),
        ),
        (
            "Target.setAutoAttach",
            json!({ "autoAttach": true, "waitForDebuggerOnStart": true, "flatten": true }),
        ),
        (
            "Emulation.setDeviceMetricsOverride",
            json!({
                "width": request.viewport.width,
                "height": request.viewport.height,
                "deviceScaleFactor": 1,
                "mobile": false,
            }),
        ),
    ] {
        cdp.send(Some(&root), method, params).await?;
    }
    let mut themes = Vec::with_capacity(THEMES.len());
    for theme in THEMES {
        themes.push(render_theme(&cdp, &root, &capture, &door, theme, &request.out).await?);
    }
    Ok(themes)
}

async fn run_inner(request: PreviewRequest, target: PreviewTarget) -> Result<UxPreviewReport> {
    let assets = assets_dir()?;
    let browser_path = browser::find()?;
    tokio::fs::create_dir_all(&request.out)
        .await
        .with_context(|| format!("creating {}", request.out.display()))?;
    let label = target.label.clone();
    let file = target.file.clone();
    let listing = target
        .listing
        .as_ref()
        .filter(|l| carries_grants(l))
        .map(|l| l.id.clone());
    let door = Arc::new(Door {
        home: request.home.clone(),
        access: request.access.clone(),
        node_did: request.node_did.clone(),
        target,
        assets,
        surfaces: OnceCell::new(),
        calls: Mutex::new(Vec::new()),
        calls_omitted: Mutex::new(0),
        bound: Mutex::new(None),
    });
    let mut browser = Browser::launch(&browser_path).await?;
    let themes = drive(&mut browser, &request, door.clone()).await;
    browser.close().await;
    let listing = listing.or_else(|| {
        door.bound
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    });
    let report = UxPreviewReport {
        target: label,
        listing,
        file,
        node_did: request.node_did.clone(),
        viewport: request.viewport,
        browser: browser_path.display().to_string(),
        report: request.out.join("report.json"),
        out: request.out.clone(),
        themes: themes?,
    };
    tokio::fs::write(&report.report, serde_json::to_vec_pretty(&report)?)
        .await
        .with_context(|| format!("writing {}", report.report.display()))?;
    Ok(report)
}

/// Renders `target` at light and dark and writes one PNG per theme and
/// `report.json` into `request.out`.
pub async fn run(request: PreviewRequest, target: PreviewTarget) -> Result<UxPreviewReport> {
    tokio::time::timeout(RUN_DEADLINE, run_inner(request, target))
        .await
        .map_err(|_| {
            anyhow!(
                "the preview did not finish within {}s",
                RUN_DEADLINE.as_secs()
            )
        })?
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draft_home() -> (tempfile::TempDir, PathBuf) {
        let home = tempfile::tempdir().unwrap();
        let file = home.path().join("drafts/plugin.js");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "export default {}").unwrap();
        std::fs::write(file.with_file_name("plugin.css"), ".a {}").unwrap();
        (home, file)
    }

    async fn no_afb(_: String, _: String) -> Result<Value> {
        bail!("no afb here")
    }

    #[tokio::test]
    async fn a_path_previews_as_a_draft_with_its_stylesheet() {
        let (home, file) = draft_home();
        let target = resolve_target(home.path(), file.to_str().unwrap(), no_afb)
            .await
            .unwrap();
        assert!(target.listing.is_none());
        assert_eq!(target.module.css.as_deref(), Some(".a {}"));
        assert_eq!(target.file, file.canonicalize().unwrap().to_string_lossy());
    }

    #[tokio::test]
    async fn an_unknown_target_names_both_forms() {
        let (home, _) = draft_home();
        let error = resolve_target(home.path(), "nope", no_afb)
            .await
            .err()
            .unwrap()
            .to_string();
        assert!(error.contains("no ux plugin \"nope\""), "{error}");
        assert!(error.contains("plugin.js"), "{error}");
    }

    #[tokio::test]
    async fn a_dev_plugin_resolves_by_its_listing_name() {
        let home = tempfile::tempdir().unwrap();
        let dev = home
            .path()
            .join(gents_store::local::home::UX_PLUGINS_DIR_NAME)
            .join("hello");
        std::fs::create_dir_all(&dev).unwrap();
        std::fs::write(dev.join(ux::DEV_ENTRY), "export default {}").unwrap();
        let target = resolve_target(home.path(), "hello", no_afb).await.unwrap();
        assert_eq!(target.label, "hello");
    }

    #[test]
    fn blob_ids_do_not_split_a_finding_by_theme() {
        let text = "boom at blob:null/defb72f8-de53-4064-9291-6a1ee8994858:124:48";
        assert_eq!(without_blob_ids(text), "boom at <frame module>:124:48");
    }

    #[test]
    fn viewports_parse_and_refuse() {
        assert_eq!(
            "640x480".parse::<Viewport>().unwrap(),
            Viewport {
                width: 640,
                height: 480
            }
        );
        assert!("wide".parse::<Viewport>().is_err());
    }
}
