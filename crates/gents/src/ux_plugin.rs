//! UX plugins on the runtime side: what is on disk for the desktop to load,
//! and how one module is produced. The desktop owns the loader, the
//! contribution registry and enable/disable; this module owns the two
//! doors it reads from and the static lint `gents pack check` runs.
//!
//! Two doors, listed by [`list`]:
//!
//! - **dev**: `<home>/ux-plugins/<id>/plugin.js` (+ `plugin.css`), dropped
//!   in by hand or by an agent. No manifest, so no declared-contributions
//!   gate; the desktop treats it like a bundled plugin.
//! - **pack**: every file-recorded pack install (`pack-installs/*.json`)
//!   whose manifest declared `ux[]`. The record carries the declaration
//!   verbatim, so the desktop gates each `register` call without opening
//!   the archive.
//!
//! A module is produced by [`resolve_module`] in one of two ways: read from
//! the materialized pack (or the dev folder), or run from the pack's own
//! `.afb` plugin, whose stdout for `{"role":"ux","surface":"webview"}` is
//! `{"module": "<esm>", "css": "<optional>"}`. The producer is sandboxed
//! by `PluginRunner`; the product passes the desktop's same gate either
//! way.
//!
//! A UX plugin is an asset, never a document: nothing here touches the
//! control plane.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::pack::{list_home_installs, HomePackInstall, PackUxPlugin, UxContributions};

pub mod lint;

/// The entry file a dev plugin folder must hold.
pub const DEV_ENTRY: &str = "plugin.js";
/// The optional stylesheet beside it.
pub const DEV_CSS: &str = "plugin.css";

/// Largest module the desktop will evaluate: a plugin is a UI, not a
/// bundle of a whole app, and the webview reads it whole.
pub const MAX_MODULE_BYTES: usize = 16 * 1024 * 1024;

/// The input a form-C `.afb` producer is given; keyword-additive only.
pub const PRODUCER_ROLE: &str = "ux";
pub const PRODUCER_SURFACE: &str = "webview";

/// Which door a listing came through.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UxDoor {
    Dev,
    Pack,
}

/// How the module is produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UxProducer {
    File,
    Afb,
}

/// One UX plugin the desktop may load, as `desktop_ux_plugins_list`
/// answers it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UxListing {
    /// `<name>` for a dev plugin, `<ns>/<pack>/<name>` for a pack one.
    pub id: String,
    pub door: UxDoor,
    pub producer: UxProducer,
    /// The entry file, or the `.afb` that produces the module.
    pub file: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pack: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// Absent for a dev plugin: nothing declared, nothing gated.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub contributes: Option<UxContributions>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_enabled: Option<bool>,
}

/// A produced module, as `desktop_ux_plugin_source` answers it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UxModule {
    pub source: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub css: Option<String>,
    /// `sha256:<hex>` of `source`, for the desktop's reload comparison.
    pub digest: String,
}

/// What a form-C producer must print: one JSON object with the module.
#[derive(Debug, Deserialize)]
struct ProducedModule {
    module: String,
    #[serde(default)]
    css: Option<String>,
}

fn dev_root(home: &Path) -> PathBuf {
    home.join(crate::home::UX_PLUGINS_DIR_NAME)
}

/// A dev plugin's folder name doubles as its listing id and lands in a
/// path, so it is held to the same rule as a pack name.
fn is_dev_id(name: &str) -> bool {
    crate::pack::is_valid_pack_name(name)
}

/// Every UX plugin on disk under `home`, dev door then pack door, each
/// sorted by id. A folder without `plugin.js` is not an error (an unrelated
/// directory); an unreadable pack record is skipped with a warning rather
/// than failing the whole list, so one bad install cannot hide the rest.
pub fn list(home: &Path) -> Result<Vec<UxListing>> {
    let mut out = Vec::new();
    let root = dev_root(home);
    if root.is_dir() {
        let mut dirs: Vec<_> = std::fs::read_dir(&root)
            .with_context(|| format!("reading {}", root.display()))?
            .filter_map(|entry| entry.ok())
            .filter(|entry| entry.path().is_dir())
            .collect();
        dirs.sort_by_key(|entry| entry.file_name());
        for entry in dirs {
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if !is_dev_id(&name) {
                tracing::warn!(folder = %name, "ux-plugins folder name is not snake_case; skipped");
                continue;
            }
            let file = entry.path().join(DEV_ENTRY);
            if !file.is_file() {
                continue;
            }
            out.push(UxListing {
                id: name,
                door: UxDoor::Dev,
                producer: UxProducer::File,
                file: file.to_string_lossy().into_owned(),
                pack: None,
                description: None,
                contributes: None,
                default_enabled: None,
            });
        }
    }
    for record in list_home_installs(home)? {
        for ux in &record.ux {
            match pack_listing(home, &record, ux) {
                Ok(listing) => out.push(listing),
                Err(error) => tracing::warn!(
                    pack = %record.coordinate,
                    plugin = %ux.name,
                    error = %format!("{error:#}"),
                    "ux plugin skipped"
                ),
            }
        }
    }
    Ok(out)
}

fn pack_listing(home: &Path, record: &HomePackInstall, ux: &PackUxPlugin) -> Result<UxListing> {
    let id = format!("{}/{}", record.coordinate, ux.name);
    let (producer, file) = match (&ux.entry, &ux.plugin) {
        (Some(entry), _) => (UxProducer::File, home.join(&record.assets).join(entry)),
        (None, Some(plugin)) => {
            let installed = record
                .plugins
                .iter()
                .find(|p| &p.name == plugin)
                .with_context(|| {
                    format!(
                        "pack {} did not install the plugin {plugin:?}",
                        record.coordinate
                    )
                })?;
            let digest_hex = installed
                .digest
                .strip_prefix("sha256:")
                .unwrap_or(&installed.digest);
            (
                UxProducer::Afb,
                crate::plugin::store::artifact_path(home, digest_hex)?,
            )
        }
        (None, None) => anyhow::bail!("ux plugin {id} names neither entry nor plugin"),
    };
    Ok(UxListing {
        id,
        door: UxDoor::Pack,
        producer,
        file: file.to_string_lossy().into_owned(),
        pack: Some(record.coordinate.clone()),
        description: Some(ux.description.clone()),
        contributes: Some(ux.contributes.clone()),
        default_enabled: ux.default_enabled,
    })
}

/// The listing for `id`, or an error naming what is missing.
pub fn find(home: &Path, id: &str) -> Result<UxListing> {
    list(home)?
        .into_iter()
        .find(|l| l.id == id)
        .with_context(|| format!("no ux plugin {id:?} under {}", home.display()))
}

fn read_bounded(path: &Path, what: &str) -> Result<String> {
    let bytes =
        std::fs::read(path).with_context(|| format!("reading {what} {}", path.display()))?;
    anyhow::ensure!(
        bytes.len() <= MAX_MODULE_BYTES,
        "{what} {} is {} bytes; the limit is {} MiB",
        path.display(),
        bytes.len(),
        MAX_MODULE_BYTES / 1024 / 1024
    );
    String::from_utf8(bytes).with_context(|| format!("{what} {} is not UTF-8", path.display()))
}

fn digest_of(source: &str) -> String {
    format!("sha256:{:x}", Sha256::digest(source.as_bytes()))
}

/// Reads a file-produced module: the entry, and the css beside it when
/// the listing (dev) or the manifest (pack) names one.
fn file_module(home: &Path, listing: &UxListing, css: Option<&str>) -> Result<UxModule> {
    let entry = PathBuf::from(&listing.file);
    let source = read_bounded(&entry, "ux plugin entry")?;
    let css_path = match (listing.door, css) {
        (UxDoor::Dev, _) => {
            let beside = entry.with_file_name(DEV_CSS);
            beside.is_file().then_some(beside)
        }
        (UxDoor::Pack, Some(css)) => Some(
            home.join(
                listing
                    .pack
                    .as_deref()
                    .and_then(|coordinate| pack_assets_dir(home, coordinate).ok())
                    .context("pack assets dir")?,
            )
            .join(css),
        ),
        (UxDoor::Pack, None) => None,
    };
    let css = css_path
        .map(|path| read_bounded(&path, "ux plugin css"))
        .transpose()?;
    Ok(UxModule {
        digest: digest_of(&source),
        source,
        css,
    })
}

fn pack_assets_dir(home: &Path, coordinate: &str) -> Result<PathBuf> {
    let record = crate::pack::read_home_install(home, coordinate)?
        .with_context(|| format!("pack {coordinate} is not installed"))?;
    Ok(PathBuf::from(record.assets))
}

/// The css path a pack's manifest declared for `listing`, if any.
fn declared_css(home: &Path, listing: &UxListing) -> Result<Option<String>> {
    let Some(coordinate) = listing.pack.as_deref() else {
        return Ok(None);
    };
    let record = crate::pack::read_home_install(home, coordinate)?
        .with_context(|| format!("pack {coordinate} is not installed"))?;
    let name = listing.id.rsplit('/').next().unwrap_or(&listing.id);
    Ok(record
        .ux
        .iter()
        .find(|u| u.name == name)
        .and_then(|u| u.css.clone()))
}

/// Parses what a form-C producer printed into a module.
pub fn module_from_producer_output(output: &serde_json::Value) -> Result<UxModule> {
    let produced: ProducedModule = serde_json::from_value(output.clone()).context(
        "a ux producer must print one JSON object {\"module\": \"<esm>\", \"css\": \"<optional>\"}",
    )?;
    anyhow::ensure!(
        produced.module.len() <= MAX_MODULE_BYTES,
        "the produced module is {} bytes; the limit is {} MiB",
        produced.module.len(),
        MAX_MODULE_BYTES / 1024 / 1024
    );
    Ok(UxModule {
        digest: digest_of(&produced.module),
        source: produced.module,
        css: produced.css,
    })
}

/// The input a form-C producer is handed.
pub fn producer_input() -> serde_json::Value {
    serde_json::json!({ "role": PRODUCER_ROLE, "surface": PRODUCER_SURFACE })
}

/// Produces the module for `listing`. A file-produced one is read here; an
/// `.afb`-produced one is run through `run_afb`, which the caller supplies
/// so this module needs no executor of its own (the desktop bridge and the
/// CLI each already hold one).
pub async fn resolve_module<F, Fut>(
    home: &Path,
    listing: &UxListing,
    run_afb: F,
) -> Result<UxModule>
where
    F: FnOnce(String, String) -> Fut,
    Fut: std::future::Future<Output = Result<serde_json::Value>>,
{
    match listing.producer {
        UxProducer::File => {
            let css = declared_css(home, listing)?;
            file_module(home, listing, css.as_deref())
        }
        UxProducer::Afb => {
            let coordinate = listing
                .pack
                .as_deref()
                .context("an afb-produced ux plugin always ships in a pack")?;
            let (namespace, _) = crate::pack_registry::split_pack_coordinate(coordinate);
            let record = crate::pack::read_home_install(home, coordinate)?
                .with_context(|| format!("pack {coordinate} is not installed"))?;
            let name = listing.id.rsplit('/').next().unwrap_or(&listing.id);
            let plugin = record
                .ux
                .iter()
                .find(|u| u.name == name)
                .and_then(|u| u.plugin.clone())
                .with_context(|| format!("ux plugin {} names no producer", listing.id))?;
            let output = run_afb(namespace.to_owned(), plugin).await?;
            module_from_producer_output(&output)
        }
    }
}

/// Lints every UX plugin a pack carries from its materialized directory:
/// what `gents pack check` and `gents pack build` run. Keys are plugin
/// names; a plugin produced by an `.afb` has no file to lint here.
pub fn lint_pack_dir(
    dir: &Path,
    ux: &[PackUxPlugin],
) -> Result<BTreeMap<String, Vec<lint::Finding>>> {
    let mut out = BTreeMap::new();
    for plugin in ux {
        let Some(entry) = &plugin.entry else {
            continue;
        };
        let source = read_bounded(&dir.join(entry), "ux plugin entry")?;
        out.insert(plugin.name.clone(), lint::lint(&source));
    }
    Ok(out)
}

#[cfg(test)]
mod tests;
