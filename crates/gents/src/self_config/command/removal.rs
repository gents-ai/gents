//! Model-facing pack removal consumption, delegating to the existing
//! [`PackInstaller`] removal owner.
//!
//! Two disjoint cases exist, decided only by already-recorded state:
//!
//! - Node-recorded packs (graph/documents): delegated whole to
//!   [`PackInstaller::remove`], which owns version refusal, drift
//!   (`DriftPolicy::Refuse`), dependent-pack refusal, unfinished-run refusal
//!   and dependency release. This module adds no policy of its own.
//! - Home-recorded plugin/assets packs: the immutable [`HomePackInstall`]
//!   record is the only provenance ever consulted; a missing record is
//!   "not installed", never a store or registry lookup, and a versioned
//!   request names no record at all. No owner yet releases the recorded
//!   plugins, materialized assets and home record together, so removal is
//!   reported as blocked and nothing is written.

use anyhow::Result;
use serde_json::json;

use super::super::{ordered, PackInstaller};
use crate::pack::{read_home_install, HomePackInstall};

/// The bare coordinate a home pin is recorded under, or `None` when
/// `package` carries a version: a versioned request cannot address the
/// recorded pin (the pin itself is the version authority) and must never
/// resolve to a latest version instead.
pub(crate) fn home_pin_coordinate(package: &str) -> Option<String> {
    let spec = crate::pack_resolve::parse_pack_spec(package.trim()).ok()?;
    spec.version
        .is_none()
        .then(|| format!("{}/{}", spec.namespace, spec.name))
}

/// The existing stored immutable provenance for `coordinate`, if any. Reads
/// only the file-recorded install; there is deliberately no store, registry
/// or latest-version fallback.
pub(crate) fn stored_home_pin(
    home: Option<&std::path::Path>,
    coordinate: &str,
) -> Result<Option<HomePackInstall>> {
    match home {
        Some(home) => read_home_install(home, coordinate),
        None => Ok(None),
    }
}

/// The blocked-removal report for a home-recorded pin. Pure: it takes no
/// home handle, so it cannot touch the record, its plugins or its assets.
fn blocked_home_removal(record: &HomePackInstall) -> String {
    ordered! {
        "removed": false,
        "blocked": true,
        "pack": record.coordinate.clone(),
        "recorded_pin": json!({
            "version": record.version,
            "digest": record.digest,
            "kind": record.kind,
            "plugins": record.plugins,
        }),
        "reason": "Removing a home-recorded plugin/assets pack requires a release owner that reclaims its plugins and materialized assets together; none is wired yet, so the recorded pin cannot be removed and stays installed as-is.",
        "next_call": json!({"argv": ["help", "pack"]}),
        "effect": "Nothing changed: the recorded installation, its plugins and its materialized assets are intact.",
    }
    .pretty()
}

/// Removes `package` for the installer's node and home.
///
/// A home-recorded pin is reported as blocked until the release owner
/// above exists; every other case is delegated to
/// [`PackInstaller::remove`], which owns all guards and the transaction.
/// Caller input can only name a package: it cannot pick a home, grant
/// authority, select tools, or reach the store or registry.
pub(crate) async fn remove_pack(installer: &PackInstaller, package: &str) -> Result<String> {
    if let Some(coordinate) = home_pin_coordinate(package) {
        // One read: the report must echo this exact record, and no second
        // read can race a concurrent removal that retires it.
        if let Some(record) = stored_home_pin(installer.home.as_deref(), &coordinate)? {
            return Ok(blocked_home_removal(&record));
        }
    }
    installer.remove(package).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pack::{write_home_install, PackKind};

    fn sample(coordinate: &str) -> HomePackInstall {
        HomePackInstall {
            coordinate: coordinate.to_owned(),
            version: "1.0.0".into(),
            digest: format!("sha256:{}", "a".repeat(64)),
            kind: PackKind::Assets,
            assets: "packs/.materialized/acme/tools/aaaa".into(),
            plugins: vec![crate::pack::InstalledPackPlugin {
                name: "acme/tools/tool".into(),
                digest: format!("sha256:{}", "b".repeat(64)),
            }],
            installed_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    #[test]
    fn a_versioned_request_names_no_pin() {
        assert_eq!(
            home_pin_coordinate("acme/tools").as_deref(),
            Some("acme/tools")
        );
        // A version must never fall back to the recorded pin or resolve a
        // latest version; it names nothing here.
        assert_eq!(home_pin_coordinate("acme/tools@1.2.3"), None);
    }

    #[test]
    fn the_stored_pin_is_the_only_provenance() {
        let home = tempfile::tempdir().unwrap();
        let record = sample("acme/tools");
        write_home_install(home.path(), &record).unwrap();
        assert_eq!(
            stored_home_pin(Some(home.path()), "acme/tools").unwrap(),
            Some(record)
        );
        // Absence is absence: no store, registry or latest-version fallback.
        assert_eq!(
            stored_home_pin(Some(home.path()), "acme/other").unwrap(),
            None
        );
        // No home, no provenance.
        assert_eq!(stored_home_pin(None, "acme/tools").unwrap(), None);
    }

    #[test]
    fn blocked_removal_reports_the_pin_without_touching_anything() {
        let record = sample("acme/tools");
        let text = blocked_home_removal(&record);
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["removed"], false);
        assert_eq!(value["blocked"], true);
        assert_eq!(value["pack"], "acme/tools");
        assert_eq!(value["recorded_pin"]["digest"], record.digest);
        assert_eq!(value["recorded_pin"]["version"], record.version);
        // The blocked result is a stable reason plus a next call, not
        // implementation policy or owner signatures.
        assert_eq!(
            value["reason"]
                .as_str()
                .unwrap()
                .contains("release owner"),
            true
        );
        assert_eq!(value["missing_owner"], serde_json::Value::Null);
        assert_eq!(value["recovery_owner"], serde_json::Value::Null);
        assert_eq!(value["next_call"]["argv"], json!(["help", "pack"]));
        // The report is the whole effect: the function takes no home handle,
        // so the record, its plugins and its assets cannot have changed.
        assert_eq!(value["effect"].as_str().unwrap().contains("Nothing changed"), true);
    }
}
