//! Where an installed plugin lives under the home directory, and why.
//!
//! Two concerns that must not be conflated: a plugin's own bytes (an
//! immutable `.afb`, safe to keep two versions of side by side) and which
//! version is "the" installed one for a bare name (mutable, one pointer
//! per namespace/name). Splitting them into a content-addressed store plus
//! a small per-name record is what makes both true at once:
//!
//! - installing a second version of the same plugin never disturbs the
//!   first (each lands under its own digest in the store);
//! - `gents plugin run <name>` and `gents plugin list` still have exactly
//!   one place to look up "which version is this name today";
//! - removing a name never has to reason about whether some other
//!   installed name still needs the same bytes - it is left in the store,
//!   exactly the way the pack asset cache leaves a superseded digest until
//!   an explicit prune.
//!
//! Layout, under `<home>/plugins/`:
//! ```text
//! store/<sha256-hex>.afb              content-addressed plugin bytes
//! installed/<namespace>/<name>.json   the installed record for one name
//! ```

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

fn plugins_root(home: &Path) -> PathBuf {
    home.join("plugins")
}

fn store_dir(home: &Path) -> PathBuf {
    plugins_root(home).join("store")
}

fn installed_dir(home: &Path) -> PathBuf {
    plugins_root(home).join("installed")
}

fn store_path(home: &Path, digest_hex: &str) -> Result<PathBuf> {
    anyhow::ensure!(
        digest_hex.len() == 64 && digest_hex.chars().all(|c| c.is_ascii_hexdigit()),
        "invalid plugin digest {digest_hex:?}: expected 64 hex characters"
    );
    Ok(store_dir(home).join(format!("{digest_hex}.afb")))
}

/// A registry coordinate becomes two path components here, so it is
/// checked before it is joined rather than trusted. A coordinate is
/// `[A-Za-z0-9_-]+` on both halves in the registry's own grammar; anything
/// else (a separator, `..`, an empty half) would let a typed or
/// pack-supplied name write outside the plugins directory, which is a
/// filesystem escape rather than a bad lookup.
fn checked_component(kind: &str, value: &str) -> Result<()> {
    anyhow::ensure!(
        !value.is_empty()
            && value
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
        "invalid plugin {kind} {value:?}: use letters, digits, underscore or hyphen"
    );
    Ok(())
}

fn record_path(home: &Path, namespace: &str, name: &str) -> Result<PathBuf> {
    checked_component("namespace", namespace)?;
    checked_component("name", name)?;
    Ok(installed_dir(home)
        .join(namespace)
        .join(format!("{name}.json")))
}

/// One installed plugin's identity: enough to find its bytes in the
/// content store and to show `gents plugin list` what is installed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct InstalledPlugin {
    pub(crate) namespace: String,
    pub(crate) name: String,
    pub(crate) version: String,
    /// `sha256:<hex>`, matching the pack registry's own digest format.
    pub(crate) digest: String,
    pub(crate) language: String,
}

/// Writes `bytes` into the content-addressed store under `digest_hex`.
/// Reuses the pack registry's own stage-then-persist-noclobber helper: two
/// concurrent installs of the same content race safely, and a second
/// install of content already present is a no-op, never a partial file a
/// concurrent `gents plugin run` could observe.
pub(crate) fn store_bytes(home: &Path, digest_hex: &str, bytes: &[u8]) -> Result<()> {
    let path = store_path(home, digest_hex)?;
    let dir = store_dir(home);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    crate::commands::pack::registry::stage_and_persist(&dir, &path, bytes)
}

pub(crate) fn read_bytes(home: &Path, digest_hex: &str) -> Result<Vec<u8>> {
    let path = store_path(home, digest_hex)?;
    std::fs::read(&path)
        .with_context(|| format!("reading the installed plugin bytes at {}", path.display()))
}

/// Points `namespace/name` at `record`, replacing whatever it pointed to
/// before. The content the old pointer named is left in the store: see
/// this module's own doc for why that is deliberate, not a leak.
pub(crate) fn write_record(home: &Path, record: &InstalledPlugin) -> Result<()> {
    let path = record_path(home, &record.namespace, &record.name)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    let bytes =
        serde_json::to_vec_pretty(record).context("encoding the installed-plugin record")?;
    std::fs::write(&path, bytes).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

pub(crate) fn read_record(home: &Path, namespace: &str, name: &str) -> Result<InstalledPlugin> {
    let path = record_path(home, namespace, name)?;
    let bytes = std::fs::read(&path).with_context(|| {
        format!(
            "{namespace}/{name} has no installed-plugin record at {}",
            path.display()
        )
    })?;
    serde_json::from_slice(&bytes).with_context(|| format!("parsing {}", path.display()))
}

/// Removes `namespace/name`'s installed record, returning it. The content
/// store is untouched (see this module's own doc for why).
pub(crate) fn remove_record(home: &Path, namespace: &str, name: &str) -> Result<InstalledPlugin> {
    let record = read_record(home, namespace, name)?;
    let path = record_path(home, namespace, name)?;
    std::fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
    Ok(record)
}

/// Every installed plugin under `home`, across every namespace, sorted by
/// namespace then name for a stable `gents plugin list` order.
pub(crate) fn list_records(home: &Path) -> Result<Vec<InstalledPlugin>> {
    let root = installed_dir(home);
    if !root.is_dir() {
        return Ok(Vec::new());
    }
    let mut records = Vec::new();
    for namespace_entry in
        std::fs::read_dir(&root).with_context(|| format!("reading {}", root.display()))?
    {
        let namespace_path = namespace_entry?.path();
        if !namespace_path.is_dir() {
            continue;
        }
        for record_entry in std::fs::read_dir(&namespace_path)
            .with_context(|| format!("reading {}", namespace_path.display()))?
        {
            let path = record_entry?.path();
            if path.extension().and_then(|extension| extension.to_str()) != Some("json") {
                continue;
            }
            let bytes =
                std::fs::read(&path).with_context(|| format!("reading {}", path.display()))?;
            records.push(
                serde_json::from_slice::<InstalledPlugin>(&bytes)
                    .with_context(|| format!("parsing {}", path.display()))?,
            );
        }
    }
    records.sort_by(|a, b| (&a.namespace, &a.name).cmp(&(&b.namespace, &b.name)));
    Ok(records)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A coordinate reaches the filesystem as two path components, so a
    /// separator or a `..` in either half is refused rather than joined.
    /// Without this, `gents plugin install ../../x/y` writes its record
    /// outside the plugins directory entirely.
    #[test]
    fn a_coordinate_that_would_escape_the_plugins_directory_is_refused() {
        let home = Path::new("/tmp/plugin-store-test");
        for (namespace, name) in [
            ("../../etc", "passwd"),
            ("gents", "../../../escape"),
            ("..", "x"),
            ("", "x"),
            ("gents", ""),
            ("ge/nts", "x"),
        ] {
            record_path(home, namespace, name)
                .expect_err(&format!("{namespace:?}/{name:?} must be refused"));
        }
        record_path(home, "gents", "format_check").expect("an ordinary coordinate is fine");
    }

    /// The store filename is a digest, and only a digest.
    #[test]
    fn a_digest_that_is_not_a_digest_is_refused() {
        let home = Path::new("/tmp/plugin-store-test");
        for digest in ["../escape", "", "zz", &"f".repeat(63), &"g".repeat(64)] {
            store_path(home, digest).expect_err(&format!("{digest:?} must be refused"));
        }
        store_path(home, &"a".repeat(64)).expect("a real digest is fine");
    }
}
