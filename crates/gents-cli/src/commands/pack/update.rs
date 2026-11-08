use anyhow::Result;
use serde_json::{json, Value};

use super::registry::{resolve_registry_url, RegistryClient};
use crate::cli::{PackInstallArgs, PackOutdatedArgs, PackUpdateArgs};
use crate::output_format::OutputFormat;

pub(crate) async fn outdated_packs(
    scope: &crate::cli::ScopeArgs,
    registry: Option<&str>,
) -> Result<Vec<Value>> {
    let home = crate::home_state::resolve_home_dir(scope.home.as_deref());
    let node = if scope.graphql.is_some()
        || gents::store::local::home::init_config_path(&home).is_file()
    {
        Some(super::resolve_scope_owner(scope).await?)
    } else {
        None
    };
    outdated_packs_on(
        scope.graphql.is_none().then_some(home.as_path()),
        node.as_ref()
            .map(|(access, owner)| (access, owner.as_str())),
        registry,
    )
    .await
}

pub(crate) async fn outdated_packs_on(
    home: Option<&std::path::Path>,
    node: Option<(&gents::ConfigAccess, &str)>,
    registry: Option<&str>,
) -> Result<Vec<Value>> {
    let installed = gents::pack::installed_packs(home, node).await?;
    let client = RegistryClient::new(resolve_registry_url(registry));
    let mut report = Vec::new();
    for pack in installed {
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
            inference_slots: args.inference_slots.clone(),
            preview: false,
            scope: args.scope.clone(),
            output: OutputFormat::Json,
            force_rebind_concrete_did: false,
            registry: args.registry.clone(),
            drift: args.drift,
            grant_authority: false,
            wire: Vec::new(),
            expect_digest: None,
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
        let (bytes, _) = gents::pack::archive::pack_dir(&root).unwrap();
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

        let scope = crate::cli::ScopeArgs {
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
            inference_slots: Vec::new(),
            preview: false,
            scope: scope.clone(),
            output: OutputFormat::Json,
            force_rebind_concrete_did: false,
            registry: Some(registry_a),
            drift: crate::cli::PackDriftArgs::default(),
            grant_authority: false,
            wire: Vec::new(),
            expect_digest: None,
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

    /// An update reinstalls a documents dependency with `explicit: false`; it
    /// must stay a dependency, released with its last dependent.
    #[tokio::test]
    async fn an_update_keeps_a_documents_dependency_released_with_its_dependent() {
        let home = tempfile::tempdir().unwrap();
        let scope = crate::cli::ScopeArgs {
            home: Some(home.path().to_path_buf()),
            graphql: None,
            node_did: None,
        };
        let cli = crate::cli::Cli::try_parse_from([
            "gents",
            "init",
            "--store-key-custody",
            "file",
            "--node-name",
            "dep-updater",
            "--home",
            home.path().to_str().unwrap(),
        ])
        .unwrap();
        let crate::cli::Command::Init(init_args) = cli.command else {
            panic!("expected init")
        };
        crate::commands::init::init(init_args)
            .await
            .expect("identity-only init");
        let node_did = crate::home_state::resolve_node_did(Some(home.path()), None).unwrap();
        let profile_id = format!("{node_did}:default-profile");
        // An identity-only init publishes no inference profile; the home's
        // client configures the one the worker slot names.
        {
            use gents::store::config_client::{
                apply_desired_state_plan, DesiredStateApplyDocument, DesiredStateApplyPlan,
            };
            let (access, _) = crate::resolve_config_access(Some(home.path()), None)
                .await
                .unwrap();
            let backend_id = format!("{node_did}:default-backend");
            let documents = [
                (
                    gents::Collection::InferenceBackend,
                    serde_json::json!({
                        "node_did": node_did, "backend_id": backend_id, "name": "Test inference",
                        "provider_kind": "OpenAiCompatible", "endpoint": "http://127.0.0.1:1/v1",
                        "auth": {"kind": "unauthenticated"},
                    }),
                ),
                (
                    gents::Collection::InferenceProfile,
                    serde_json::json!({
                        "node_did": node_did, "profile_id": profile_id,
                        "backend_id": backend_id, "model_name": "test-model",
                    }),
                ),
            ];
            let plan = DesiredStateApplyPlan::new(
                documents
                    .into_iter()
                    .map(|(collection, value)| DesiredStateApplyDocument {
                        collection,
                        add: value.clone(),
                        update: value,
                    })
                    .collect(),
            )
            .unwrap();
            access
                .transact_write("test.pack_update.profile", |txn| {
                    let plan = &plan;
                    Box::pin(async move { apply_desired_state_plan(txn, plan).await.map(|_| ()) })
                })
                .await
                .unwrap();
            access
                .local_node()
                .unwrap()
                .shutdown()
                .await
                .expect("node shutdown");
        }
        let slot = format!("worker={profile_id}");
        let _ = super::super::test_support::fixture_pack_source("dependee_fixture", home.path());
        let install = |fixture: &str, explicit: bool| {
            let package = super::super::test_support::fixture_dir(fixture)
                .to_str()
                .unwrap()
                .to_owned();
            crate::request_helpers::capture_report(super::super::install(PackInstallArgs {
                package,
                inference_slots: vec![slot.clone()],
                preview: false,
                scope: scope.clone(),
                output: OutputFormat::Json,
                force_rebind_concrete_did: false,
                registry: Some("http://127.0.0.1:9".to_owned()),
                drift: crate::cli::PackDriftArgs::default(),
                grant_authority: false,
                wire: Vec::new(),
                expect_digest: None,
                explicit,
            }))
        };
        install("dependent_fixture", true).await.unwrap();
        install("dependee_fixture", false).await.unwrap();

        let removed = crate::request_helpers::capture_report(super::super::remove::remove(
            crate::cli::PackRemoveArgs {
                package: "fixture/dependent_fixture".to_owned(),
                scope,
                drift: crate::cli::PackDriftArgs::default(),
            },
        ))
        .await
        .unwrap();
        let released: Vec<&Value> = removed["removed"]["dependencies"]
            .as_array()
            .unwrap()
            .iter()
            .map(|dependency| &dependency["pack"])
            .collect();
        assert_eq!(released, [&json!("fixture/dependee_fixture")], "{removed}");
    }

    #[tokio::test]
    async fn outdated_lists_home_recorded_installs_without_opening_a_node() {
        let home = tempfile::tempdir().unwrap();
        let scope = crate::cli::ScopeArgs {
            home: Some(home.path().to_path_buf()),
            graphql: None,
            node_did: None,
        };
        let fixture = super::super::test_support::fixture_dir("assets_fixture");
        crate::request_helpers::capture_report(super::super::install(PackInstallArgs {
            package: fixture.to_str().unwrap().to_owned(),
            inference_slots: Vec::new(),
            preview: false,
            scope: scope.clone(),
            output: OutputFormat::Json,
            force_rebind_concrete_did: false,
            registry: None,
            drift: crate::cli::PackDriftArgs::default(),
            grant_authority: false,
            wire: Vec::new(),
            expect_digest: None,
            explicit: true,
        }))
        .await
        .expect("installing an assets pack needs no node");
        assert!(
            !gents::store::local::home::init_config_path(home.path()).is_file(),
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
