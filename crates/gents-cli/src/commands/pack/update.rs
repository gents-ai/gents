use anyhow::Result;
use gents_pack::pack::archive::OpenError;
use gents_pack::pack::core_packs::{self, PluginInterfaceError, Release, UpdateDecision};
use gents_pack::pack::registry::{advertised_release, open_candidate, CandidateError};
use gents_pack::pack::PackChangeError;
use serde_json::{json, Value};

use super::registry::{resolve_registry_url, RegistryClient};
use crate::cli::{PackInstallArgs, PackOutdatedArgs, PackUpdateArgs};
use crate::output_format::OutputFormat;

pub(crate) async fn outdated_packs(
    scope: &crate::cli::ScopeArgs,
    registry: Option<&str>,
) -> Result<Vec<Value>> {
    let home = gents_service::home_state::resolve_home_dir(scope.home.as_deref());
    let node =
        if scope.graphql.is_some() || gents_store::local::home::init_config_path(&home).is_file() {
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
    node: Option<(&gents_store::config_client::ConfigAccess, &str)>,
    registry: Option<&str>,
) -> Result<Vec<Value>> {
    let installed = gents_pack::pack::installed_packs(home, node).await?;
    let client = RegistryClient::new(resolve_registry_url(registry));
    let runtime = core_packs::runtime_release();
    let mut report = Vec::new();
    for pack in installed {
        let (namespace, name) = gents_pack::pack::registry::split_pack_coordinate(&pack.coordinate);
        let (latest, outdated, mut error) = match client.package(namespace, name).await {
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
        let mut entry = json!({
            "pack": pack.coordinate,
            "installed": pack.version,
            "digest": pack.digest,
            "source": pack.source,
            "latest": latest,
            "outdated": outdated,
        });
        let newer = match (&latest, outdated, &error) {
            (Some(latest), Some(true), None) => Some(latest.clone()),
            _ => None,
        };
        if let Some(latest) = newer {
            match built_for(&client, &pack.coordinate, &latest).await {
                Err(lookup) => error = Some(lookup),
                Ok(None) => {}
                Ok(Some(release)) => {
                    entry["gents_version"] = json!(release.to_string());
                    let refusal = PluginInterfaceError::refusal(
                        &pack.coordinate,
                        &latest,
                        &runtime,
                        Some(&release),
                    );
                    entry["loadable"] = json!(refusal.is_none());
                    if let Some(refusal) = refusal {
                        entry["reason"] = json!(refusal.to_string());
                    }
                }
            }
        }
        entry["error"] = json!(error);
        report.push(entry);
    }
    Ok(report)
}

/// The release the registry says `coordinate` at `version` was built for;
/// `None` when it does not say.
async fn built_for(
    client: &RegistryClient,
    coordinate: &str,
    version: &str,
) -> Result<Option<Release>, String> {
    let (namespace, name) = gents_pack::pack::registry::split_pack_coordinate(coordinate);
    let entry = client
        .version(namespace, name, version)
        .await
        .map_err(|error| format!("{error:#}"))?;
    advertised_release(&entry).transpose().map_err(|error| {
        format!("the registry's gents_version for {coordinate} {version}: {error}")
    })
}

/// What the update does with `latest` of `pack` before installing it: keep
/// the installed version, with the refusal, when this runtime does not load
/// the candidate. The registry's `gents_version` decides without a download;
/// without it the candidate is downloaded and opened.
async fn keep_reason(
    client: &RegistryClient,
    pack: &Value,
    latest: &str,
) -> Option<PluginInterfaceError> {
    let coordinate = pack["pack"].as_str().unwrap_or_default();
    let refusal = match pack["gents_version"].as_str().map(Release::parse) {
        Some(Ok(release)) => PluginInterfaceError::refusal(
            coordinate,
            latest,
            &core_packs::runtime_release(),
            Some(&release),
        )?,
        Some(Err(_)) | None => {
            let (namespace, name) = gents_pack::pack::registry::split_pack_coordinate(coordinate);
            match open_candidate(client, namespace, name, latest).await {
                Err(CandidateError::Open(OpenError::Interface(refusal))) => refusal,
                _ => return None,
            }
        }
    };
    refusal
        .admission()
        .is_some_and(|admission| {
            core_packs::update_decision(admission) == UpdateDecision::KeepInstalled
        })
        .then_some(refusal)
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
                let (namespace, name) = gents_pack::pack::registry::split_pack_coordinate(package);
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

pub(crate) async fn update(args: PackUpdateArgs) -> Result<(), PackChangeError> {
    let packs = outdated_packs(&args.scope, args.registry.as_deref()).await?;
    let client = RegistryClient::new(resolve_registry_url(args.registry.as_deref()));
    let selected = select_packs(&packs, args.package.as_deref())?;
    // A caller of one named pack, such as the shell or the desktop, drops the report, so its
    // failure is returned itself, after the report a person at the command line reads.
    let named = args.package.is_some();
    let mut own_failure = None;

    let mut updated = Vec::new();
    let mut failed = Vec::new();
    let mut current = Vec::new();
    let mut kept = Vec::new();
    for pack in selected {
        let coordinate = pack["pack"].as_str().unwrap_or_default().to_owned();
        if let Some(error) = pack["error"].as_str() {
            if named {
                own_failure = Some(anyhow::Error::msg(error.to_owned()).into());
            }
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
                "error": "the registry reported no latest version for this pack; run gents config pack outdated and retry",
            }));
            continue;
        };
        if let Some(refusal) = keep_reason(&client, pack, latest).await {
            kept.push(json!({
                "pack": coordinate,
                "installed": pack["installed"],
                "candidate": latest,
                "reason": refusal.to_string(),
            }));
            continue;
        }
        let install_args = PackInstallArgs {
            package: format!("{coordinate}@{latest}"),
            inference_slots: args.inference_slots.clone(),
            dependency_variables: Vec::new(),
            bind_agents: Vec::new(),
            preview: false,
            scope: args.scope.clone(),
            output: OutputFormat::Json,
            force_rebind_concrete_did: false,
            registry: args.registry.clone(),
            drift: args.drift,
            grant_authority: false,
            wire: Vec::new(),
            expect_digest: None,
            root_agents: None,
            explicit: false,
        };
        match crate::request_helpers::capture_report(super::install(install_args)).await {
            Ok(_) => updated.push(coordinate),
            Err(error) => {
                failed.push(json!({ "pack": coordinate, "error": format!("{error:#}") }));
                if named {
                    own_failure = Some(error);
                }
            }
        }
    }

    let any_failed = !failed.is_empty();
    let mut report = json!({ "updated": updated, "failed": failed, "current": current });
    if !kept.is_empty() {
        report["kept"] = kept.into();
    }
    crate::print_json(&report)?;
    if let Some(error) = own_failure {
        return Err(error);
    }
    if any_failed {
        return Err(anyhow::anyhow!(
            "updating failed for at least one pack; see \"failed\" in the report above"
        )
        .into());
    }
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
        let (bytes, _) = gents_pack::pack::archive::pack_dir(&root).unwrap();
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
        let Some(crate::cli::Command::Init(init_args)) = cli.command else {
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
            dependency_variables: Vec::new(),
            bind_agents: Vec::new(),
            preview: false,
            scope: scope.clone(),
            output: OutputFormat::Json,
            force_rebind_concrete_did: false,
            registry: Some(registry_a),
            drift: crate::cli::PackDriftArgs::default(),
            grant_authority: false,
            wire: Vec::new(),
            expect_digest: None,
            root_agents: None,
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
        let Some(crate::cli::Command::Init(init_args)) = cli.command else {
            panic!("expected init")
        };
        crate::commands::init::init(init_args)
            .await
            .expect("identity-only init");
        let node_did =
            gents_service::home_state::resolve_node_did(Some(home.path()), None).unwrap();
        let profile_id = format!("{node_did}:default-profile");
        // An identity-only init publishes no inference profile; the home's
        // client configures the one the worker slot names.
        {
            use gents_documents::apply_desired_state_plan;
            use gents_documents::{DesiredStateApplyDocument, DesiredStateApplyPlan};
            let (access, _) = crate::resolve_config_access(Some(home.path()), None)
                .await
                .unwrap();
            let backend_id = format!("{node_did}:default-backend");
            let documents = [
                (
                    gents_store::collection::Collection::InferenceBackend,
                    serde_json::json!({
                        "node_did": node_did, "backend_id": backend_id, "name": "Test inference",
                        "provider_kind": "OpenAiCompatible", "endpoint": "http://127.0.0.1:1/v1",
                        "auth": {"kind": "unauthenticated"},
                    }),
                ),
                (
                    gents_store::collection::Collection::InferenceProfile,
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
                    Box::pin(async move {
                        apply_desired_state_plan(
                            txn,
                            plan,
                            &gents_inference::resolved::ProviderProfilePolicy,
                        )
                        .await
                        .map(|_| ())
                    })
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
                dependency_variables: Vec::new(),
                bind_agents: Vec::new(),
                preview: false,
                scope: scope.clone(),
                output: OutputFormat::Json,
                force_rebind_concrete_did: false,
                registry: Some("http://127.0.0.1:9".to_owned()),
                drift: crate::cli::PackDriftArgs::default(),
                grant_authority: false,
                wire: Vec::new(),
                expect_digest: None,
                root_agents: None,
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

    /// A caller of one named pack's update, such as the shell or the desktop, drops the report, so
    /// its failure is the error itself rather than a pointer to "failed".
    #[tokio::test]
    async fn a_named_update_tells_its_own_failure() {
        let home = tempfile::tempdir().unwrap();
        let scope = crate::cli::ScopeArgs {
            home: Some(home.path().to_path_buf()),
            graphql: None,
            node_did: None,
        };
        let fixture = super::super::test_support::fixture_dir("assets_fixture");
        crate::request_helpers::capture_report(super::super::install(PackInstallArgs {
            package: fixture.to_str().unwrap().to_owned(),
            dependency_variables: Vec::new(),
            bind_agents: Vec::new(),
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
            root_agents: None,
            explicit: true,
        }))
        .await
        .expect("installing an assets pack needs no node");
        let serve = |name: &'static str| {
            let digest = "0".repeat(64);
            let broken = b"not a pack".to_vec();
            crate::commands::pack::registry::tests::serve_fake_pack(name, "1.1.0", broken, digest)
        };
        let (broken, _broken) = serve("assets_fixture").await;
        let (elsewhere, _elsewhere) = serve("another_pack").await;
        let update_from = |registry| {
            let failed = crate::request_helpers::capture_report(update(PackUpdateArgs {
                package: Some("fixture/assets_fixture".to_owned()),
                inference_slots: Vec::new(),
                scope: scope.clone(),
                registry: Some(registry),
                drift: crate::cli::PackDriftArgs::default(),
            }));
            async move { format!("{:#}", failed.await.expect_err("the update fails")) }
        };

        let install = update_from(broken).await;
        let lookup = update_from(elsewhere).await;

        assert!(install.contains("refusing to install"), "{install}");
        assert!(lookup.contains("the registry has nothing at"), "{lookup}");
    }

    fn install_from(
        package: String,
        scope: &crate::cli::ScopeArgs,
        registry: Option<String>,
    ) -> PackInstallArgs {
        PackInstallArgs {
            package,
            inference_slots: Vec::new(),
            dependency_variables: Vec::new(),
            bind_agents: Vec::new(),
            preview: false,
            scope: scope.clone(),
            output: OutputFormat::Json,
            force_rebind_concrete_did: false,
            registry,
            drift: crate::cli::PackDriftArgs::default(),
            grant_authority: false,
            wire: Vec::new(),
            expect_digest: None,
            root_agents: None,
            explicit: true,
        }
    }

    fn home_scope(home: &std::path::Path) -> crate::cli::ScopeArgs {
        crate::cli::ScopeArgs {
            home: Some(home.to_path_buf()),
            graphql: None,
            node_did: None,
        }
    }

    /// `fixture/assets_fixture` at `version`, its manifest declaring
    /// `declared` as the Gents release it was built for.
    fn assets_pack_declaring(version: &str, declared: Option<&Release>) -> tempfile::TempDir {
        let dir = super::super::test_support::assets_pack_dir("fixture", "assets_fixture", version);
        let path = dir.path().join("manifest.json");
        let mut manifest: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        if let Some(declared) = declared {
            manifest["gents_version"] = declared.to_string().into();
        }
        std::fs::write(&path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        dir
    }

    /// Each case runs twice: once with a registry that advertises the
    /// candidate's `gents_version`, which decides before any download, and
    /// once with one that does not, which decides on the downloaded archive.
    #[tokio::test]
    async fn update_decision_conforms_through_the_update_handler() {
        use gents_lean_vocab::families::pack_core_packs_update_decision as family;
        use gents_pack::pack::core_packs::{
            interface_admission, runtime_release, InterfaceAdmission,
        };

        let runtime = runtime_release();
        for case in family::cases() {
            let candidate: InterfaceAdmission = case.input.candidate;
            let declared = match candidate {
                InterfaceAdmission::Compatible => runtime,
                InterfaceAdmission::OlderMajor => Release {
                    major: runtime.major - 1,
                    minor: 0,
                    patch: 0,
                    rc: None,
                },
                InterfaceAdmission::TooNew => Release {
                    patch: runtime.patch + 1,
                    rc: None,
                    ..runtime
                },
            };
            assert_eq!(interface_admission(&runtime, Some(&declared)), candidate);

            for advertised in [false, true] {
                let home = tempfile::tempdir().unwrap();
                let scope = home_scope(home.path());
                let installed = assets_pack_declaring("1.0.0", None);
                crate::request_helpers::capture_report(super::super::install(install_from(
                    installed.path().to_str().unwrap().to_owned(),
                    &scope,
                    None,
                )))
                .await
                .expect("installing the current build");

                let newer = assets_pack_declaring("1.1.0", Some(&declared));
                let (bytes, _) =
                    gents_pack::pack::archive::format::pack_dir_for_any_interface(newer.path())
                        .unwrap();
                let digest = {
                    use sha2::Digest;
                    format!("{:x}", sha2::Sha256::digest(&bytes))
                };
                let (registry, state) =
                    crate::commands::pack::registry::tests::serve_fake_pack_built_for(
                        "assets_fixture",
                        "1.1.0",
                        bytes,
                        digest,
                        advertised.then(|| declared.to_string()),
                    )
                    .await;
                let report = crate::request_helpers::capture_report(update(PackUpdateArgs {
                    package: None,
                    inference_slots: Vec::new(),
                    scope,
                    registry: Some(registry),
                    drift: crate::cli::PackDriftArgs::default(),
                }))
                .await
                .unwrap_or_else(|error| {
                    panic!("{candidate:?} (advertised: {advertised}): the update failed: {error:#}")
                });

                let named = |list: &str| {
                    report[list].as_array().into_iter().flatten().any(|entry| {
                        entry == "fixture/assets_fixture"
                            || entry["pack"] == "fixture/assets_fixture"
                    })
                };
                let decision = match (named("updated"), named("kept")) {
                    (true, false) => UpdateDecision::Take,
                    (false, true) => UpdateDecision::KeepInstalled,
                    _ => panic!(
                        "{candidate:?} (advertised: {advertised}): the update neither took nor kept the pack: {report}"
                    ),
                };
                let version = gents_pack::pack::list_home_installs(home.path())
                    .unwrap()
                    .into_iter()
                    .find(|record| record.coordinate == "fixture/assets_fixture")
                    .map(|record| record.version);
                let expected = match decision {
                    UpdateDecision::Take => "1.1.0",
                    UpdateDecision::KeepInstalled => "1.0.0",
                };
                assert_eq!(
                    version.as_deref(),
                    Some(expected),
                    "{candidate:?} (advertised: {advertised}): {report}"
                );
                if advertised && decision == UpdateDecision::KeepInstalled {
                    assert_eq!(
                        state.downloads.load(std::sync::atomic::Ordering::SeqCst),
                        0,
                        "{candidate:?}: an advertised release decides before any download"
                    );
                }
                case.check(decision);
            }
        }
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
            dependency_variables: Vec::new(),
            bind_agents: Vec::new(),
            preview: false,
            scope: scope.clone(),
            output: OutputFormat::Json,
            force_rebind_concrete_did: false,
            registry: None,
            drift: crate::cli::PackDriftArgs::default(),
            grant_authority: false,
            wire: Vec::new(),
            expect_digest: None,
            root_agents: None,
            explicit: true,
        }))
        .await
        .expect("installing an assets pack needs no node");
        assert!(
            !gents_store::local::home::init_config_path(home.path()).is_file(),
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
        assert_eq!(
            packs[0]["source"]["kind"], "local",
            "a pack installed from a path reports that it came from a file: {packs:?}"
        );
        assert!(
            !home.path().join("data").exists(),
            "outdated never opened a node either"
        );
    }
}
