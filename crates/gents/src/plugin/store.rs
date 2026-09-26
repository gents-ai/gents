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
    home.join(crate::home::PLUGINS_DIR_NAME)
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

/// Splits `namespace/name`, refusing anything that is not a plugin coordinate.
pub fn parse_coordinate(coordinate: &str) -> Result<(&str, &str)> {
    let (namespace, name) = coordinate
        .split_once('/')
        .with_context(|| format!("{coordinate:?} is not a plugin; name it as namespace/name"))?;
    checked_component("namespace", namespace)?;
    checked_component("name", name)?;
    Ok((namespace, name))
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
pub struct InstalledPlugin {
    pub namespace: String,
    pub name: String,
    pub version: String,
    /// `sha256:<hex>`, matching the pack registry's own digest format.
    pub digest: String,
    pub language: String,
    /// Authored admission metadata, retained verbatim rather than reconstructed
    /// from artifact capabilities at execution time.
    pub declaration: crate::pack::PackPlugin,
    /// The authority the operator granted at install; absent means sealed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub granted: Option<crate::plugin::Manifold>,
    /// The plugin's `TOOL.md`, kept with the install so a tool needs nothing
    /// from the pack at call time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    /// `{namespace}/{name}` of the pack install that owns this record (a
    /// standalone `gents plugin install` is the plugin's own single-plugin
    /// pack). `None` for a record written before this field existed; such a
    /// record is treated as unowned and never refuses a reinstall.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_pack_coordinate: Option<String>,
    /// The owning pack's content digest at the time this record was
    /// written, kept for operator visibility only; ownership is decided by
    /// coordinate alone so an update or reinstall of the same pack always
    /// replaces its own record regardless of version or digest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_pack_digest: Option<String>,
}

impl InstalledPlugin {
    /// The ceiling a call runs under: the recorded grant.
    pub fn ceiling(&self) -> crate::plugin::Manifold {
        self.granted
            .clone()
            .unwrap_or_else(crate::plugin::Manifold::sealed)
    }
}

/// Refuses to overwrite `namespace/name`'s existing record when it is owned
/// by a different pack coordinate than `pack_coordinate`; an unowned record
/// (written before ownership was tracked) or one owned by the same
/// coordinate (an update or reinstall) is not refused. Registry and local
/// packs share one plugin store keyed by `namespace/name`
/// ([`InstalledPlugin`]'s own doc), so without this check installing one
/// pack could silently steal a name another pack's install still owns.
pub fn check_plugin_ownership(
    home: &Path,
    namespace: &str,
    name: &str,
    pack_coordinate: &str,
) -> Result<()> {
    let Ok(existing) = read_record(home, namespace, name) else {
        return Ok(());
    };
    match existing.owner_pack_coordinate {
        Some(owner) if owner != pack_coordinate => Err(anyhow::anyhow!(
            "{namespace}/{name} is already installed by pack {owner}; installing it from {pack_coordinate} \
             would overwrite that pack's plugin. Remove {owner} first, or install it again to replace its own plugin."
        )),
        _ => Ok(()),
    }
}

/// Whether `namespace/name`'s installed record is owned by `pack_coordinate`:
/// its `owner_pack_coordinate` matches, or it has no owner on record (written
/// before ownership was tracked) and still holds the artifact `digest` that
/// pack installed. `gents pack remove` uses this to remove only what its own
/// pack owns, leaving a name another install still owns untouched.
pub fn owns_plugin_record(
    home: &Path,
    namespace: &str,
    name: &str,
    pack_coordinate: &str,
    digest: &str,
) -> bool {
    read_record(home, namespace, name).is_ok_and(|record| {
        match record.owner_pack_coordinate.as_deref() {
            Some(owner) => owner == pack_coordinate,
            None => record.digest == digest,
        }
    })
}

/// The grant to record when `plugin` is installed as `namespace/name`,
/// asking for consent only when it wants more than was granted before.
pub fn grant_on_install(
    home: &Path,
    namespace: &str,
    plugin: &crate::pack::PackPlugin,
    consent: bool,
) -> Result<Option<crate::plugin::Manifold>> {
    let previous = read_record(home, namespace, &plugin.name)
        .ok()
        .map(|record| record.ceiling());
    let granted = crate::plugin::authority::grant_for(plugin, previous.as_ref(), consent)?;
    Ok((granted != crate::plugin::Manifold::sealed()).then_some(granted))
}

/// Writes `bytes` into the content-addressed store under `digest_hex`.
/// Reuses the pack registry's own stage-then-persist-noclobber helper: two
/// concurrent installs of the same content race safely, and a second
/// install of content already present is a no-op, never a partial file a
/// concurrent `gents plugin run` could observe.
pub fn store_bytes(home: &Path, digest_hex: &str, bytes: &[u8]) -> Result<()> {
    let path = store_path(home, digest_hex)?;
    let dir = store_dir(home);
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    crate::pack_registry::stage_and_persist(&dir, &path, bytes)
}

pub fn read_bytes(home: &Path, digest_hex: &str) -> Result<Vec<u8>> {
    let path = store_path(home, digest_hex)?;
    std::fs::read(&path)
        .with_context(|| format!("reading the installed plugin bytes at {}", path.display()))
}

/// Points `namespace/name` at `record`, replacing whatever it pointed to
/// before. The content the old pointer named is left in the store: see
/// this module's own doc for why that is deliberate, not a leak.
pub fn write_record(home: &Path, record: &InstalledPlugin) -> Result<()> {
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

pub fn read_record(home: &Path, namespace: &str, name: &str) -> Result<InstalledPlugin> {
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
pub fn remove_record(home: &Path, namespace: &str, name: &str) -> Result<InstalledPlugin> {
    let record = read_record(home, namespace, name)?;
    let path = record_path(home, namespace, name)?;
    std::fs::remove_file(&path).with_context(|| format!("removing {}", path.display()))?;
    Ok(record)
}

/// Every installed plugin under `home`, across every namespace, sorted by
/// namespace then name for a stable `gents plugin list` order.
pub fn list_records(home: &Path) -> Result<Vec<InstalledPlugin>> {
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

    fn sample_record(
        namespace: &str,
        name: &str,
        owner_pack_coordinate: Option<&str>,
    ) -> InstalledPlugin {
        InstalledPlugin {
            namespace: namespace.to_owned(),
            name: name.to_owned(),
            version: "1.0.0".into(),
            digest: format!("sha256:{}", "a".repeat(64)),
            language: "rust".into(),
            declaration: crate::pack::PackPlugin {
                name: name.to_owned(),
                description: "test".into(),
                artifact: format!("plugins/{name}.afb"),
                source: None,
                language: "rust".into(),
                input_schema: serde_json::json!({"type": "object"}),
                manifold: None,
                instructions: None,
            },
            granted: None,
            instructions: None,
            owner_pack_coordinate: owner_pack_coordinate.map(str::to_owned),
            owner_pack_digest: owner_pack_coordinate.map(|_| "sha256:pack".to_owned()),
        }
    }

    /// A record owned by a different pack coordinate refuses installing
    /// over it, naming both packs so the operator can act.
    #[test]
    fn ownership_refuses_a_different_pack_coordinate() {
        let home = tempfile::tempdir().unwrap();
        write_record(
            home.path(),
            &sample_record("acme", "echo", Some("acme/widget")),
        )
        .unwrap();

        let error = check_plugin_ownership(home.path(), "acme", "echo", "acme/gadget").unwrap_err();
        let message = format!("{error:#}");
        assert!(message.contains("acme/widget"), "{message}");
        assert!(message.contains("acme/gadget"), "{message}");
    }

    /// The same coordinate (an update or reinstall) is never refused, and
    /// neither is a name with no owner on record (written before ownership
    /// was tracked, or never installed).
    #[test]
    fn ownership_allows_the_same_coordinate_and_an_unowned_or_absent_record() {
        let home = tempfile::tempdir().unwrap();
        check_plugin_ownership(home.path(), "acme", "echo", "acme/widget").unwrap();

        write_record(
            home.path(),
            &sample_record("acme", "echo", Some("acme/widget")),
        )
        .unwrap();
        check_plugin_ownership(home.path(), "acme", "echo", "acme/widget").unwrap();

        write_record(home.path(), &sample_record("acme", "unowned", None)).unwrap();
        check_plugin_ownership(home.path(), "acme", "unowned", "acme/anything").unwrap();
    }

    /// `gents pack remove` (via [`owns_plugin_record`]) removes only a
    /// record its own coordinate owns, never another pack's; a record with no
    /// coordinate on record is its own only while it holds the same artifact.
    #[test]
    fn owns_plugin_record_is_true_only_for_the_owning_coordinate() {
        let home = tempfile::tempdir().unwrap();
        write_record(
            home.path(),
            &sample_record("acme", "widget-echo", Some("acme/widget")),
        )
        .unwrap();
        write_record(
            home.path(),
            &sample_record("acme", "gadget-echo", Some("acme/gadget")),
        )
        .unwrap();
        write_record(home.path(), &sample_record("acme", "legacy-echo", None)).unwrap();
        let legacy_digest = format!("sha256:{}", "a".repeat(64));

        assert!(owns_plugin_record(
            home.path(),
            "acme",
            "widget-echo",
            "acme/widget",
            &legacy_digest
        ));
        assert!(!owns_plugin_record(
            home.path(),
            "acme",
            "gadget-echo",
            "acme/widget",
            &legacy_digest
        ));
        assert!(owns_plugin_record(
            home.path(),
            "acme",
            "legacy-echo",
            "acme/widget",
            &legacy_digest
        ));
        assert!(!owns_plugin_record(
            home.path(),
            "acme",
            "legacy-echo",
            "acme/widget",
            "sha256:another-artifact"
        ));
        assert!(!owns_plugin_record(
            home.path(),
            "acme",
            "not-installed",
            "acme/widget",
            &legacy_digest
        ));
    }
}
