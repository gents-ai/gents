//! `gents pack outdated` and `gents pack update`: installed packs against the
//! registry's latest versions.
//!
//! What is installed comes from the node's installation records; an update
//! is an ordinary install of the latest version, so the drift rules and
//! `--overwrite` / `--keep` apply exactly as they do to any install.

use anyhow::Result;
use serde_json::{json, Value};

use super::registry::{resolve_registry_url, RegistryClient};
use crate::cli::{PackInstallArgs, PackOutdatedArgs, PackUpdateArgs};
use crate::output_format::OutputFormat;

/// One installed pack and the latest version the registry has, if any.
async fn outdated_packs(
    scope: &crate::cli::GraphScopeArgs,
    registry: Option<&str>,
) -> Result<Vec<Value>> {
    let (access, owner) = super::resolve_scope_owner(scope).await?;
    let client = RegistryClient::new(resolve_registry_url(registry));
    let mut report = Vec::new();
    for pack in gents::pack::list_installed_packs(&access, &owner).await? {
        let (namespace, name) = super::split_namespace(&pack.coordinate);
        let (latest, outdated, error) = match client.package(namespace, name).await {
            Err(error) => (None, None, Some(format!("{error:#}"))),
            Ok(package) => match package["latest"].as_str() {
                None => (
                    None,
                    None,
                    Some("the registry reported no latest version".to_owned()),
                ),
                Some(latest) => match is_newer(latest, &pack.version) {
                    Some(newer) => (Some(latest.to_owned()), Some(newer), None),
                    None => (
                        Some(latest.to_owned()),
                        None,
                        Some(format!(
                            "cannot compare registry version {latest} with installed {}: not semver",
                            pack.version
                        )),
                    ),
                },
            },
        };
        report.push(json!({
            "pack": pack.coordinate,
            "installed": pack.version,
            "latest": latest,
            "outdated": outdated,
            "error": error,
        }));
    }
    Ok(report)
}

/// Whether `latest` is a strictly newer semver than `installed`; `None` when
/// either does not parse. The registry's latest skips yanked versions, so it
/// can be older than what is installed, and an update must never downgrade.
fn is_newer(latest: &str, installed: &str) -> Option<bool> {
    let latest = semver::Version::parse(latest).ok()?;
    let installed = semver::Version::parse(installed).ok()?;
    // Build metadata carries no precedence; plain `>` would order it lexically.
    Some(latest.cmp_precedence(&installed).is_gt())
}

/// The coordinates `update` reinstalls: the named pack, or every installed
/// one, when the registry has a strictly newer version. A selected pack
/// whose comparison is unknown (failed lookup, no latest, non-semver) stops
/// the update rather than reading as "up to date".
fn packs_to_update(packs: &[Value], package: Option<&str>) -> Result<Vec<String>> {
    let selected: Vec<&Value> = packs
        .iter()
        .filter(|pack| {
            package.is_none_or(|package| {
                let (namespace, name) = super::split_namespace(package);
                pack["pack"] == format!("{namespace}/{name}").as_str()
            })
        })
        .collect();
    if let Some(package) = package {
        anyhow::ensure!(!selected.is_empty(), "{package} is not installed");
    }
    let failed: Vec<String> = selected
        .iter()
        .filter(|pack| !pack["outdated"].is_boolean())
        .map(|pack| {
            format!(
                "{}: {}",
                pack["pack"].as_str().unwrap_or("?"),
                pack["error"]
                    .as_str()
                    .unwrap_or("version comparison unavailable")
            )
        })
        .collect();
    anyhow::ensure!(
        failed.is_empty(),
        "could not check the registry for {}",
        failed.join("; ")
    );
    Ok(selected
        .iter()
        .filter(|pack| pack["outdated"] == true)
        .filter_map(|pack| pack["pack"].as_str().map(str::to_owned))
        .collect())
}

pub(crate) async fn outdated(args: PackOutdatedArgs) -> Result<()> {
    let packs = outdated_packs(&args.scope, args.registry.as_deref()).await?;
    crate::print_json(&json!({ "packs": packs }))
}

pub(crate) async fn update(args: PackUpdateArgs) -> Result<()> {
    let packs = outdated_packs(&args.scope, args.registry.as_deref()).await?;
    let wanted = packs_to_update(&packs, args.package.as_deref())?;
    for coordinate in &wanted {
        super::install(PackInstallArgs {
            package: coordinate.clone(),
            bindings: args.bindings.clone(),
            inference_slots: args.inference_slots.clone(),
            preview: false,
            scope: args.scope.clone(),
            output: OutputFormat::Json,
            force_rebind_concrete_did: false,
            registry: args.registry.clone(),
            drift: args.drift,
        })
        .await?;
    }
    crate::print_json(&json!({ "updated": wanted }))
}

#[cfg(test)]
mod tests {
    use super::{is_newer, packs_to_update};
    use serde_json::json;

    fn row(pack: &str, outdated: Option<bool>, error: Option<&str>) -> serde_json::Value {
        json!({ "pack": pack, "outdated": outdated, "error": error })
    }

    #[test]
    fn a_failed_registry_lookup_stops_the_update() {
        let packs = [
            row("acme/a", Some(true), None),
            row("acme/b", None, Some("the registry is unreachable")),
        ];
        let error = packs_to_update(&packs, None).unwrap_err().to_string();
        assert!(
            error.contains("acme/b: the registry is unreachable"),
            "{error}"
        );
        // A named pack whose own lookup succeeded is not blocked by another's.
        assert_eq!(packs_to_update(&packs, Some("acme/a")).unwrap(), ["acme/a"]);
    }

    #[test]
    fn only_selected_outdated_packs_update_and_unknown_names_fail() {
        let packs = [
            row("acme/a", Some(true), None),
            row("acme/b", Some(false), None),
        ];
        assert_eq!(packs_to_update(&packs, None).unwrap(), ["acme/a"]);
        assert!(packs_to_update(&packs, Some("acme/b")).unwrap().is_empty());
        assert!(packs_to_update(&packs, Some("acme/missing")).is_err());
    }

    #[test]
    fn an_unknown_comparison_is_a_failure_not_up_to_date() {
        let packs = [row("acme/a", Some(true), None), row("acme/c", None, None)];
        let error = packs_to_update(&packs, None).unwrap_err().to_string();
        assert!(
            error.contains("acme/c: version comparison unavailable"),
            "{error}"
        );
        assert_eq!(packs_to_update(&packs, Some("acme/a")).unwrap(), ["acme/a"]);
    }

    #[test]
    fn only_a_strictly_newer_registry_version_is_outdated() {
        assert_eq!(is_newer("1.2.0", "1.1.9"), Some(true));
        assert_eq!(is_newer("1.2.0", "1.2.0"), Some(false));
        // Installed version yanked, or installed from an unpublished local pack.
        assert_eq!(is_newer("1.1.0", "1.2.0"), Some(false));
        assert_eq!(is_newer("1.10.0", "1.9.0"), Some(true));
        assert_eq!(is_newer("latest", "1.0.0"), None);
        assert_eq!(is_newer("1.2.0+build.2", "1.2.0+build.1"), Some(false));
        assert_eq!(is_newer("1.2.0", "1.2.0-rc.1"), Some(true));
    }
}
