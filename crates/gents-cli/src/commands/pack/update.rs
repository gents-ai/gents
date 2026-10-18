use anyhow::Result;
use serde_json::{json, Value};

use super::registry::RegistryClient;
use crate::cli::{PackInstallArgs, PackOutdatedArgs, PackUpdateArgs};
use crate::output_format::OutputFormat;

async fn outdated_packs(
    scope: &crate::cli::GraphScopeArgs,
    registry: Option<&str>,
) -> Result<Vec<Value>> {
    let home = crate::home_state::resolve_home_dir(scope.home.as_deref());
    let node = if scope.graphql.is_some() || gents::home::init_config_path(&home).is_file() {
        Some(super::resolve_scope_owner(scope).await?)
    } else {
        None
    };
    let installed = gents::pack::installed_packs(
        scope.graphql.is_none().then_some(home.as_path()),
        node.as_ref()
            .map(|(access, owner)| (access, owner.as_str())),
    )
    .await?;
    let default_registry = gents::pack_registry::resolve_registry_url_in(&home, registry)?;
    let client = RegistryClient::new(default_registry.clone());
    // An explicit --registry names where every pack is checked. Without one,
    // a file-recorded install is checked where it was installed from (a
    // vertical registry's pack is not on the master); a DB-recorded pack has
    // no record and uses the env/default.
    let recorded: std::collections::BTreeMap<String, String> = if registry.is_some() {
        Default::default()
    } else {
        gents::pack::list_home_installs(&home)?
            .into_iter()
            .filter_map(|record| Some((record.coordinate, record.registry?)))
            .collect()
    };
    let mut report = Vec::new();
    for pack in installed {
        let (namespace, name) = super::split_namespace(&pack.coordinate);
        let own_client;
        let (client, registry_url) = match recorded.get(&pack.coordinate) {
            Some(url) if url != &default_registry => {
                own_client = RegistryClient::new(url.clone());
                (&own_client, url.clone())
            }
            _ => (&client, default_registry.clone()),
        };
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
            "registry": registry_url,
            "error": error,
        }));
    }
    Ok(report)
}

fn is_newer(latest: &str, installed: &str) -> Option<bool> {
    let latest = semver::Version::parse(latest).ok()?;
    let installed = semver::Version::parse(installed).ok()?;
    Some(latest.cmp_precedence(&installed).is_gt())
}

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
        // Pinned: an unpinned install would resolve installed-first and
        // reinstall the version already there.
        let Some(latest) = pack["latest"].as_str() else {
            failed.push(json!({
                "pack": coordinate,
                "error": "the registry reported no latest version for this pack; run gents pack outdated and retry",
            }));
            continue;
        };
        let install_args = PackInstallArgs {
            package: format!("{coordinate}@{latest}"),
            bindings: args.bindings.clone(),
            inference_slots: args.inference_slots.clone(),
            preview: false,
            scope: args.scope.clone(),
            output: OutputFormat::Json,
            force_rebind_concrete_did: false,
            // the registry the outdated check found the newer version on
            registry: pack["registry"].as_str().map(str::to_owned),
            drift: args.drift,
            grant_authority: false,
            explicit: false,
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
            "node": { "node_did": "${GENTS_PACK_NODE_DID}" },
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

    #[tokio::test]
    async fn update_installs_a_newer_version_the_fake_registry_advertises() {
        let (old_bytes, old_digest) = build_document_pack("1.0.0");
        let (new_bytes, new_digest) = build_document_pack("1.1.0");

        let home = tempfile::tempdir().unwrap();
        let home_path = home.path().to_path_buf();

        let cli = crate::cli::Cli::try_parse_from([
            "gents",
            "init",
            "--store-key-custody",
            "file",
            "--node-name",
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
            node_did: None,
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
            explicit: true,
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

    /// The desktop's flow: a preview fetches the pack (into the store, no
    /// record), the install that follows resolves it from the store. The
    /// record must still name the registry the preview fetched from, and
    /// `outdated` must return there — unless `--registry` names where every
    /// pack is checked, which overrides the record.
    #[tokio::test]
    async fn a_store_hit_keeps_the_registry_the_preview_fetched_from_and_a_flag_overrides_it() {
        let fixture = super::super::test_support::fixture_dir("assets_fixture");
        let (bytes, _) = gents::pack_archive::pack_dir(&fixture).unwrap();
        let digest = {
            use sha2::Digest;
            format!("{:x}", sha2::Sha256::digest(&bytes))
        };
        let (vertical, state) = crate::commands::pack::registry::tests::serve_fake_pack(
            "assets_fixture",
            "1.0.0",
            bytes,
            digest,
        )
        .await;
        let home = tempfile::tempdir().unwrap();
        let scope = crate::cli::GraphScopeArgs {
            home: Some(home.path().to_path_buf()),
            graphql: None,
            node_did: None,
        };
        let install = |preview: bool, registry: Option<String>| PackInstallArgs {
            package: "fixture/assets_fixture".to_owned(),
            bindings: None,
            inference_slots: Vec::new(),
            preview,
            scope: scope.clone(),
            output: OutputFormat::Json,
            force_rebind_concrete_did: false,
            registry,
            drift: crate::cli::PackDriftArgs::default(),
            grant_authority: false,
            explicit: true,
        };

        crate::request_helpers::capture_report(super::super::install(install(
            true,
            Some(vertical.clone()),
        )))
        .await
        .expect("the preview fetches from the vertical registry");
        assert_eq!(state.downloads.load(std::sync::atomic::Ordering::SeqCst), 1);
        assert!(
            gents::pack::read_home_install(home.path(), "fixture/assets_fixture")
                .unwrap()
                .is_none(),
            "a preview writes no record"
        );

        crate::request_helpers::capture_report(super::super::install(install(false, None)))
            .await
            .expect("the install resolves from the store");
        assert_eq!(
            state.downloads.load(std::sync::atomic::Ordering::SeqCst),
            1,
            "no second download: the store had it"
        );
        let record = gents::pack::read_home_install(home.path(), "fixture/assets_fixture")
            .unwrap()
            .expect("installed");
        assert_eq!(record.registry.as_deref(), Some(vertical.as_str()));

        // reinstalling keeps the recorded registry rather than clobbering it
        crate::request_helpers::capture_report(super::super::install(install(false, None)))
            .await
            .expect("a reinstall resolves from the installed record");
        let record = gents::pack::read_home_install(home.path(), "fixture/assets_fixture")
            .unwrap()
            .expect("still installed");
        assert_eq!(record.registry.as_deref(), Some(vertical.as_str()));

        let packs = outdated_packs(&scope, None).await.unwrap();
        assert_eq!(packs.len(), 1, "{packs:?}");
        assert_eq!(packs[0]["registry"], vertical.as_str());
        assert_eq!(packs[0]["outdated"], false);
        assert!(packs[0]["error"].is_null(), "{:?}", packs[0]["error"]);

        let (elsewhere, _state) = crate::commands::pack::registry::tests::serve_fake_pack(
            "assets_fixture",
            "2.0.0",
            Vec::new(),
            format!("sha256:{}", "a".repeat(64)),
        )
        .await;
        let packs = outdated_packs(&scope, Some(&elsewhere)).await.unwrap();
        assert_eq!(
            packs[0]["registry"],
            elsewhere.as_str(),
            "the flag names where every pack is checked"
        );
        assert_eq!(packs[0]["outdated"], true);
    }

    #[tokio::test]
    async fn outdated_lists_home_recorded_installs_without_opening_a_node() {
        let home = tempfile::tempdir().unwrap();
        let scope = crate::cli::GraphScopeArgs {
            home: Some(home.path().to_path_buf()),
            graphql: None,
            node_did: None,
        };
        let fixture = super::super::test_support::fixture_dir("assets_fixture");
        crate::request_helpers::capture_report(super::super::install(PackInstallArgs {
            package: fixture.to_str().unwrap().to_owned(),
            bindings: None,
            inference_slots: Vec::new(),
            preview: false,
            scope: scope.clone(),
            output: OutputFormat::Json,
            force_rebind_concrete_did: false,
            registry: None,
            drift: crate::cli::PackDriftArgs::default(),
            grant_authority: false,
            explicit: true,
        }))
        .await
        .expect("installing an assets pack needs no node");
        assert!(
            !gents::home::init_config_path(home.path()).is_file(),
            "an assets install never initializes the home"
        );

        let (registry, _state) = crate::commands::pack::registry::tests::serve_fake_pack(
            "assets_fixture",
            "1.0.0",
            Vec::new(),
            format!("sha256:{}", "a".repeat(64)),
        )
        .await;
        let report = crate::request_helpers::capture_report(outdated(PackOutdatedArgs {
            scope,
            registry: Some(registry),
        }))
        .await
        .expect("outdated must list the file-recorded install");
        let packs = report["packs"].as_array().unwrap();
        assert_eq!(packs.len(), 1, "{packs:?}");
        assert_eq!(packs[0]["pack"], "fixture/assets_fixture");
        assert!(
            !home.path().join("data").exists(),
            "outdated never opened a node either"
        );
    }
}
