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

/// One installed pack and the latest version the registry has, if any. A
/// registry or network failure is recorded per pack in `error`, never
/// collapsed into "not on registry": a lookup that failed and a pack the
/// registry has never heard of are different facts.
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

/// The installed pack rows `update` acts on: the named pack, or every
/// installed one. A named pack that matches nothing installed is an error
/// naming it, never a silent empty selection.
fn select_packs<'a>(packs: &'a [Value], package: Option<&str>) -> Result<Vec<&'a Value>> {
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
    Ok(selected)
}

pub(crate) async fn outdated(args: PackOutdatedArgs) -> Result<()> {
    let packs = outdated_packs(&args.scope, args.registry.as_deref()).await?;
    let failed = packs.iter().any(|pack| !pack["error"].is_null());
    crate::print_json(&json!({ "packs": packs }))?;
    anyhow::ensure!(
        !failed,
        "the registry lookup failed for at least one installed pack; see \"error\" in the report above"
    );
    Ok(())
}

/// Reinstalls every outdated pack among the selection, one at a time.
///
/// A failed install does not stop the batch: it is recorded under `failed`
/// and the rest still run, so a partial failure keeps the list of updates
/// already done instead of losing it. Each nested install runs through
/// [`crate::request_helpers::capture_report`] so it never prints its own
/// report; `update` prints exactly one JSON document, its own.
pub(crate) async fn update(args: PackUpdateArgs) -> Result<()> {
    let packs = outdated_packs(&args.scope, args.registry.as_deref()).await?;
    let selected = select_packs(&packs, args.package.as_deref())?;

    let mut updated = Vec::new();
    let mut failed = Vec::new();
    let mut current = Vec::new();
    for pack in selected {
        let coordinate = pack["pack"].as_str().unwrap_or_default().to_owned();
        if let Some(error) = pack["error"].as_str() {
            failed.push(json!({ "pack": coordinate, "error": error }));
            continue;
        }
        if pack["outdated"] != true {
            current.push(coordinate);
            continue;
        }
        let install_args = PackInstallArgs {
            package: coordinate.clone(),
            bindings: args.bindings.clone(),
            inference_slots: args.inference_slots.clone(),
            preview: false,
            scope: args.scope.clone(),
            output: OutputFormat::Json,
            force_rebind_concrete_did: false,
            registry: args.registry.clone(),
            drift: args.drift,
            grant_authority: false,
        };
        match crate::request_helpers::capture_report(super::install(install_args)).await {
            Ok(_) => updated.push(coordinate),
            Err(error) => failed.push(json!({ "pack": coordinate, "error": format!("{error:#}") })),
        }
    }

    let any_failed = !failed.is_empty();
    crate::print_json(&json!({ "updated": updated, "failed": failed, "current": current }))?;
    anyhow::ensure!(
        !any_failed,
        "updating failed for at least one pack; see \"failed\" in the report above"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;

    fn row(pack: &str, outdated: Option<bool>, error: Option<&str>) -> serde_json::Value {
        json!({ "pack": pack, "outdated": outdated, "error": error })
    }

    #[test]
    fn selection_is_every_pack_or_the_named_one_and_an_unknown_name_is_an_error() {
        let packs = [
            row("acme/a", Some(true), None),
            row("acme/b", Some(false), None),
        ];
        let names = |selected: Vec<&serde_json::Value>| -> Vec<String> {
            selected
                .into_iter()
                .map(|pack| pack["pack"].as_str().unwrap().to_owned())
                .collect()
        };
        assert_eq!(
            names(select_packs(&packs, None).unwrap()),
            ["acme/a", "acme/b"]
        );
        assert_eq!(
            names(select_packs(&packs, Some("acme/a")).unwrap()),
            ["acme/a"]
        );
        assert!(select_packs(&packs, Some("acme/missing")).is_err());
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

    /// A minimal document pack: an agent principal placeholder and one empty
    /// tools document, the same shape a real document pack ships (compare
    /// `packs/background_continuation/pack_config.json`). Returns its bytes
    /// and the sha256 hex a registry advertises for them.
    fn build_document_pack(version: &str) -> (Vec<u8>, String) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("demo_pack");
        std::fs::create_dir_all(&root).unwrap();
        let manifest = json!({
            "manifest_version": 1,
            "name": "demo_pack",
            "version": version,
            "description": "A minimal document pack for the update end-to-end test",
            "authors": ["gents-ai contributors"],
            "tags": ["example"],
            "kind": "documents",
            "assets": ["README.md", "pack_config.json"],
            "dependencies": [],
            "config": "pack_config.json",
        });
        std::fs::write(
            root.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        let config = json!({
            "agent_principal": { "agent_did": "${GENTS_PACK_AGENT_DID}" },
            "tools": [{ "tools_id": "demo-tools", "display_name": "No tools" }],
        });
        std::fs::write(root.join("README.md"), "# demo_pack").unwrap();
        std::fs::write(
            root.join("pack_config.json"),
            serde_json::to_vec_pretty(&config).unwrap(),
        )
        .unwrap();
        let (bytes, _) = gents::pack_archive::pack_dir(&root).unwrap();
        let digest = {
            use sha2::Digest;
            format!("{:x}", sha2::Sha256::digest(&bytes))
        };
        (bytes, digest)
    }

    /// End to end: install `demo_pack@1.0.0` from a fake registry into a
    /// fresh home, point `--registry` at a second fake advertising
    /// `1.1.0`, and confirm `update` installs it and prints exactly the
    /// `{"updated", "failed", "current"}` report.
    #[tokio::test]
    async fn update_installs_a_newer_version_the_fake_registry_advertises() {
        let (old_bytes, old_digest) = build_document_pack("1.0.0");
        let (new_bytes, new_digest) = build_document_pack("1.1.0");

        let home = tempfile::tempdir().unwrap();
        let home_path = home.path().to_path_buf();

        // A fresh home needs an enabled owner principal before a pack installs.
        let cli = crate::cli::Cli::try_parse_from([
            "gents",
            "init",
            "--agent-name",
            "updater",
            "--home",
            home_path.to_str().unwrap(),
        ])
        .unwrap();
        let crate::cli::Command::Init(init_args) = cli.command else {
            panic!("expected init")
        };
        crate::commands::init::init(init_args)
            .await
            .expect("identity-only init");

        let scope = crate::cli::GraphScopeArgs {
            home: Some(home_path),
            graphql: None,
            agent_did: None,
        };

        let (registry_a, _state_a) = crate::commands::pack::registry::tests::serve_fake_pack(
            "demo_pack",
            "1.0.0",
            old_bytes,
            old_digest,
        )
        .await;
        crate::request_helpers::capture_report(super::super::install(PackInstallArgs {
            package: "demo_pack".to_owned(),
            bindings: None,
            inference_slots: Vec::new(),
            preview: false,
            scope: scope.clone(),
            output: OutputFormat::Json,
            force_rebind_concrete_did: false,
            registry: Some(registry_a),
            drift: crate::cli::PackDriftArgs::default(),
            grant_authority: false,
        }))
        .await
        .expect("installing the old version");

        let (registry_b, _state_b) = crate::commands::pack::registry::tests::serve_fake_pack(
            "demo_pack",
            "1.1.0",
            new_bytes,
            new_digest,
        )
        .await;
        let report = crate::request_helpers::capture_report(update(PackUpdateArgs {
            package: None,
            bindings: None,
            inference_slots: Vec::new(),
            scope,
            registry: Some(registry_b),
            drift: crate::cli::PackDriftArgs::default(),
        }))
        .await
        .expect("nothing should fail");

        assert_eq!(
            report,
            json!({ "updated": ["gents/demo_pack"], "failed": [], "current": [] })
        );
    }
}
