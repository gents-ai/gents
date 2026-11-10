use super::scope::resolve_plugin_slot_owner;
use super::{
    parse_inference_slot_bindings, plugin_notification_scope, resolve_pack_source,
    resolve_scope_owner, wire, PackSource,
};
use crate::cli::*;
use anyhow::{Context, Result};
use futures_util::StreamExt;
use gents::pack::{PackKind, PackManifest};
use serde_json::json;

struct LocalPackAccess {
    writer: gents::store::config_client::ConfigAccess,
    schema: gents::store::config_client::ConfigAccess,
    runtime: std::sync::Arc<dyn gents::NodeIdentity>,
}

pub(super) fn has_plugin_references(config: &serde_json::Value) -> bool {
    config
        .get("tools")
        .and_then(serde_json::Value::as_array)
        .is_some_and(|tools| {
            tools.iter().any(|tool| {
                tool.pointer("/integrations/plugins")
                    .and_then(serde_json::Value::as_array)
                    .is_some_and(|plugins| !plugins.is_empty())
            })
        })
}

pub(crate) fn parse_dependency_variables(
    values: &[String],
) -> Result<gents::pack::DependencyVariableOverrides> {
    let mut result = gents::pack::DependencyVariableOverrides::new();
    for item in values {
        let (key, value) = item
            .split_once('=')
            .context("dependency variable must be NAMESPACE/PACK:NAME=VALUE")?;
        let (coordinate, name) = key
            .rsplit_once(':')
            .context("dependency variable must include its pack coordinate and name")?;
        anyhow::ensure!(
            !coordinate.is_empty() && !name.is_empty(),
            "dependency variable needs a pack coordinate and name"
        );
        anyhow::ensure!(
            result
                .entry(coordinate.to_owned())
                .or_default()
                .insert(name.to_owned(), Some(value.to_owned()))
                .is_none(),
            "dependency variable {key:?} was provided more than once"
        );
    }
    Ok(result)
}

pub(super) fn reused_variable_preview(
    pack: &PackSource,
    config: &gents::documents::PackConfig,
) -> Result<serde_json::Value> {
    let path = pack
        .manifest()
        .config
        .as_deref()
        .context("pack has no configuration")?;
    let authored: serde_json::Value = serde_json::from_slice(pack.asset(path)?)?;
    let installed = serde_json::to_value(config)?;
    let uses =
        gents::pack::interpolate::inspect_install_variables(&authored, &|_| Some("preview".into()))
            .map_err(|names| {
                anyhow::anyhow!("cannot inspect install variables: {}", names.join(", "))
            })?;
    let uses = uses.into_iter().map(|entry| {
        let mut parts = entry.path.trim_start_matches('/').splitn(3, '/');
        let field = parts.next().unwrap_or_default();
        let next = parts.next();
        if field == "node" {
            let resolved = installed.pointer(&entry.path).cloned();
            return json!({"path": entry.path, "variables": entry.variables.keys().collect::<Vec<_>>(), "document": {"collection": "Node", "id": config.node.node_did}, "resolved": resolved});
        }
        let index = next.and_then(|value| value.parse::<usize>().ok());
        let suffix = parts.next().map(|part| format!("/{part}")).unwrap_or_default();
        let found = index.and_then(|index| {
            let collection = gents::Collection::ALL.iter().copied().find(|collection| collection.dir_name() == Some(field))?;
            let authored_id = authored.get(field)?.get(index)?.get(collection.unique_field())?.as_str()?;
            if authored_id.contains('$') { return None; }
            let row = installed.get(field)?.as_array()?.iter().find(|row| row.get(collection.unique_field()).and_then(serde_json::Value::as_str) == Some(authored_id))?;
            Some((json!({"collection": collection.graphql_type(), "id": authored_id}), row.pointer(&suffix)?.clone()))
        });
        let (document, resolved) = found.map_or((None, None), |(document, value)| (Some(document), Some(value)));
        json!({"path": entry.path, "variables": entry.variables.keys().collect::<Vec<_>>(), "document": document, "resolved": resolved})
    }).collect::<Vec<_>>();
    let runtime_environment = pack.manifest().metadata.plugins.iter().map(|plugin| {
        let manifold = gents::plugin::authority::declared_manifold(plugin)?;
        Ok(json!({"plugin": plugin.name, "declared": gents::plugin::authority::declared_environment(&manifold)}))
    }).collect::<Result<Vec<_>>>()?;
    Ok(
        json!({"pack": format!("{}/{}", pack.manifest().metadata.namespace, pack.manifest().name), "kind": "existing_installation", "uses": uses, "runtime_environment": runtime_environment}),
    )
}

pub(super) fn install_variable_preview(
    pack: &PackSource,
    owner: &str,
) -> Result<serde_json::Value> {
    install_variable_preview_with(pack, owner, &|name| std::env::var(name).ok())
}

fn install_variable_preview_with(
    pack: &PackSource,
    owner: &str,
    lookup: &dyn Fn(&str) -> Option<String>,
) -> Result<serde_json::Value> {
    let path = pack
        .manifest()
        .config
        .as_deref()
        .context("pack has no configuration")?;
    let authored: serde_json::Value = serde_json::from_slice(pack.asset(path)?)
        .context("parsing pack config for variable preview")?;
    let environment = |name: &str| {
        if name == "GENTS_PACK_NODE_DID" {
            Some(owner.to_owned())
        } else {
            lookup(name)
        }
    };
    let uses = gents::pack::interpolate::inspect_install_variables(&authored, &environment)
        .map_err(|missing| {
            anyhow::anyhow!(
                "pack config references unset environment variables: {}",
                missing.join(", ")
            )
        })?;
    let uses = uses
        .into_iter()
        .map(|entry| {
            let mut value = serde_json::to_value(&entry)?;
            let mut segments = entry.path.trim_start_matches('/').split('/');
            let collection_name = segments.next().unwrap_or_default();
            let document = if collection_name == "node" {
                Some(json!({"collection": "Node", "id": owner}))
            } else if let Some(collection) = gents::Collection::ALL
                .iter()
                .copied()
                .find(|collection| collection.dir_name() == Some(collection_name))
            {
                segments
                    .next()
                    .and_then(|index| index.parse::<usize>().ok())
                    .and_then(|index| authored.get(collection_name)?.get(index))
                    .and_then(|row| row.get(collection.unique_field()))
                    .and_then(serde_json::Value::as_str)
                    .and_then(|id| {
                        gents::pack::interpolate::interpolate_with(id, &environment).ok()
                    })
                    .map(|id| json!({"collection": collection.graphql_type(), "id": id}))
            } else {
                None
            };
            if let Some(document) = document {
                value["document"] = document;
            }
            Ok(value)
        })
        .collect::<Result<Vec<_>>>()?;
    let runtime_environment = pack
        .manifest()
        .metadata
        .plugins
        .iter()
        .map(|plugin| {
            let manifold = gents::plugin::authority::declared_manifold(plugin)?;
            Ok(json!({"plugin": plugin.name,
            "declared": gents::plugin::authority::declared_environment(&manifold)}))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(
        json!({ "pack": format!("{}/{}", pack.manifest().metadata.namespace, pack.manifest().name),
        "kind": "install_literal", "uses": uses, "runtime_environment": runtime_environment }),
    )
}

/// The documents pack install: the CLI's single site that writes a documents
/// pack and its dependencies. `explicit` is false only when the pack is
/// installed as another pack's dependency.
pub(crate) async fn install_documents_pack(
    access: &gents::store::config_client::ConfigAccess,
    owner: &str,
    pack: &PackSource,
    args: &PackInstallArgs,
    explicit: bool,
) -> Result<serde_json::Value> {
    install_documents_pack_inner(access, owner, pack, args, explicit, None).await
}

async fn install_documents_pack_inner(
    access: &gents::store::config_client::ConfigAccess,
    owner: &str,
    pack: &PackSource,
    args: &PackInstallArgs,
    explicit: bool,
    local: Option<&LocalPackAccess>,
) -> Result<serde_json::Value> {
    let home = crate::home_state::resolve_home_dir(args.scope.home.as_deref());
    let dependencies: Vec<PackSource> =
        futures_util::stream::iter(pack.manifest().metadata.dependencies.iter())
            .map(|coordinate| resolve_pack_source(coordinate, args.registry.as_deref(), &home))
            .buffered(4)
            .collect::<Vec<_>>()
            .await
            .into_iter()
            .collect::<Result<Vec<_>>>()?;
    if let gents::store::config_client::ConfigAccess::Graphql(endpoint) = access {
        let mut needs_runtime_store = false;
        for source in std::iter::once(pack).chain(dependencies.iter()) {
            if !source.manifest().metadata.plugins.is_empty()
                || !source.manifest().schemas.is_empty()
                || source
                    .manifest()
                    .metadata
                    .assets
                    .iter()
                    .any(|path| path.starts_with("schemas/"))
            {
                needs_runtime_store = true;
                break;
            }
            let Some(path) = source.manifest().config.as_deref() else {
                continue;
            };
            let config: serde_json::Value = serde_json::from_slice(source.asset(path)?)?;
            if has_plugin_references(&config) {
                needs_runtime_store = true;
                break;
            }
        }
        if needs_runtime_store && args.preview {
            return crate::http::pack_install::install_remote(
                endpoint,
                owner,
                pack,
                &dependencies,
                args,
                explicit,
            )
            .await;
        }
        if needs_runtime_store {
            return crate::http::pack_install_client::install_remote(
                endpoint,
                owner,
                pack,
                &dependencies,
                args,
                explicit,
            )
            .await;
        }
    }
    install_documents_pack_resolved_inner(access, owner, pack, args, explicit, dependencies, local)
        .await
}

pub(crate) async fn install_documents_pack_resolved(
    access: &gents::store::config_client::ConfigAccess,
    owner: &str,
    pack: &PackSource,
    args: &PackInstallArgs,
    explicit: bool,
    dependencies: Vec<PackSource>,
) -> Result<serde_json::Value> {
    install_documents_pack_resolved_inner(access, owner, pack, args, explicit, dependencies, None)
        .await
}

async fn install_documents_pack_resolved_inner(
    access: &gents::store::config_client::ConfigAccess,
    owner: &str,
    pack: &PackSource,
    args: &PackInstallArgs,
    explicit: bool,
    dependencies: Vec<PackSource>,
    local: Option<&LocalPackAccess>,
) -> Result<serde_json::Value> {
    let home = crate::home_state::resolve_home_dir(args.scope.home.as_deref());
    let requested = parse_inference_slot_bindings(&args.inference_slots)?;
    let temp = tempfile::tempdir()?;
    gents::pack::materialize::materialize(pack.archive(), temp.path())?;
    let origin_tag = gents::pack::pack_origin_tag(&format!(
        "{}/{}",
        pack.manifest().metadata.namespace,
        pack.manifest().name
    ))?;
    let shared_collections = if args.preview {
        super::review::shared_collections(access, pack.manifest(), |path| {
            Ok(pack.asset(path)?.to_vec())
        })
        .await
    } else {
        None
    };
    let review = |documents: Vec<serde_json::Value>, needs: serde_json::Value| {
        let mut preview = json!({
            "pack": pack.manifest().name,
            "source": pack.label(),
            "digest": pack.digest(),
            "owner": owner,
            "origin_tag": origin_tag,
            "dependencies": pack.manifest().metadata.dependencies,
            "kind": pack.manifest().metadata.kind,
            "declared_access": super::review::declared_access(&documents),
            "documents": documents,
            "collections": super::review::collections(pack.manifest(), |path| {
                Ok(pack.asset(path)?.to_vec())
            })?,
            "shared_collections": shared_collections,
            "inference_needs": needs,
            "would_write": false,
        });
        let dependency_manifests: Vec<_> = dependencies.iter().map(PackSource::manifest).collect();
        merge(
            &mut preview,
            super::review::identity_and_authority(pack.manifest(), &dependency_manifests)?,
        );
        Ok::<_, anyhow::Error>(preview)
    };
    let overrides = parse_dependency_variables(&args.dependency_variables)?;
    let dependency_archives = dependencies
        .iter()
        .map(PackSource::archive)
        .collect::<Vec<_>>();
    let prepared = if let Some(local) = local {
        gents::pack::prepare_document_pack_archive_with_runtime(
            access,
            owner,
            pack.archive(),
            &requested,
            &dependency_archives,
            Some(home.as_path()),
            &|name| std::env::var(name).ok(),
            local.runtime.as_ref(),
            &overrides,
        )
        .await
    } else {
        gents::pack::prepare_document_pack_archive(
            access,
            owner,
            pack.archive(),
            &requested,
            &dependency_archives,
            gents::pack::InstalledVia::Operator,
            args.scope.graphql.is_none().then_some(home.as_path()),
            &|name| std::env::var(name).ok(),
            &overrides,
        )
        .await
    };
    let mut prepared = match prepared {
        Ok(prepared) => prepared,
        // A preview reports the slots still to bind, so they can be chosen
        // and previewed again; the install itself still refuses them.
        Err(refusal) if args.preview => {
            let mut packs = Vec::new();
            for source in std::iter::once(pack).chain(&dependencies) {
                match super::review::pack_inference(access, owner, source.manifest(), &requested)
                    .await
                {
                    Some(inference) => packs.push(inference),
                    None => return Err(refusal),
                }
            }
            if packs.iter().all(super::review::PackInference::is_bound) {
                return Err(refusal);
            }
            let authored = gents::pack::load_pack_authoring_config(
                pack.manifest(),
                &gents::pack::PackInstallOptions {
                    node_did: owner.to_owned(),
                },
                &|path| Ok(pack.asset(path)?.to_vec()),
                &|name| std::env::var(name).ok(),
            )?;
            return review(
                super::review::authored_documents(&authored)?,
                super::review::inference_needs(&packs),
            );
        }
        Err(refusal) => return Err(refusal),
    };
    PackSource::record_graph_sources(&mut prepared, pack, &dependencies);
    let inference = prepared.inference.clone();
    let dependency_inference = prepared.dependency_inference.clone();
    if args.preview {
        let variables = std::iter::once(pack)
            .chain(dependencies.iter())
            .map(|source| {
                let coordinate = format!(
                    "{}/{}",
                    source.manifest().metadata.namespace,
                    source.manifest().name
                );
                if std::ptr::eq(source, pack) && prepared.preserves_root_values() {
                    let mut value = reused_variable_preview(source, &prepared.config)?;
                    value["kind"] = json!("upgrade");
                    Ok(value)
                } else if let Some(config) = prepared.reused_dependency_config(&coordinate) {
                    reused_variable_preview(source, config)
                } else if let Some(values) = overrides.get(&coordinate) {
                    install_variable_preview_with(source, owner, &|name| {
                        values
                            .get(name)
                            .cloned()
                            .unwrap_or_else(|| std::env::var(name).ok())
                    })
                } else {
                    install_variable_preview(source, owner)
                }
            })
            .collect::<Result<Vec<_>>>()?;
        let packs: Vec<_> = std::iter::once((pack.manifest(), inference.clone()))
            .chain(dependencies.iter().filter_map(|dependency| {
                let manifest = dependency.manifest();
                Some((manifest, dependency_inference.get(&manifest.name)?.clone()))
            }))
            .map(|(manifest, preview)| super::review::PackInference::bound(manifest, preview))
            .collect();
        let mut preview = review(
            super::review::documents_by_slot(&prepared.config, &prepared.agent_bindings)?,
            super::review::inference_needs(&packs),
        )?;
        preview["inference"] = json!(inference);
        preview["dependency_inference"] = json!(dependency_inference);
        preview["install_variables"] = json!(variables);
        return Ok(preview);
    }
    // A dependency installed on its own behalf (`explicit` false) writes no
    // home record, and a remote node has no local home to record in.
    let record_home = explicit && args.scope.graphql.is_none();
    // Every pack that ships an asset class is materialized before any
    // write; the leases are held until the home record is written.
    let mut cache_leases = Vec::new();
    let mut materialized_assets = String::new();
    let mut dependency_assets = Vec::new();
    if record_home {
        for dependency in &dependencies {
            if let Some((assets, lease)) =
                gents::pack::home_install::materialize_asset_pack(&home, dependency.archive())?
            {
                dependency_assets.push((
                    format!(
                        "{}/{}",
                        dependency.manifest().metadata.namespace,
                        dependency.manifest().name
                    ),
                    assets,
                ));
                cache_leases.push(lease);
            }
        }
        if let Some((assets, lease)) =
            gents::pack::home_install::materialize_asset_pack(&home, pack.archive())?
        {
            materialized_assets = assets;
            cache_leases.push(lease);
        }
    }
    let dependency_coordinates: Vec<String> = dependencies
        .iter()
        .map(|dependency| {
            format!(
                "{}/{}",
                dependency.manifest().metadata.namespace,
                dependency.manifest().name
            )
        })
        .collect();
    let schema_access = local.map_or(access, |local| &local.schema);
    let writer = local.map_or(access, |local| &local.writer);
    gents::pack::apply_prepared_dependency_schemas(schema_access, &prepared).await?;
    let schemas = super::super::schema::apply_pack_schemas_if_present(schema_access, temp.path())
        .await
        .context("pack install schemas")?;
    let (apply, plugins) = gents::pack::install_prepared_document_graph(
        writer,
        owner,
        pack.archive(),
        &prepared,
        args.drift.policy(),
        args.scope.graphql.is_none().then_some(home.as_path()),
        args.grant_authority,
        explicit,
    )
    .await?;
    let root_coordinate = format!(
        "{}/{}",
        pack.manifest().metadata.namespace,
        pack.manifest().name
    );
    let mut identity = gents::pack::PackIdentity::new(
        pack.manifest(),
        pack.digest(),
        plugins
            .iter()
            .filter(|plugin| {
                plugin.owner_pack_coordinate.as_deref() == Some(root_coordinate.as_str())
            })
            .map(|plugin| gents::pack::InstalledPackPlugin {
                name: plugin.name.clone(),
                digest: plugin.digest.clone(),
            })
            .collect(),
    );
    identity.dependencies = dependency_coordinates;
    identity.explicit = explicit;
    if record_home {
        gents::pack::record_after_install(
            &home,
            gents::pack::HomePackInstall {
                coordinate: identity.coordinate.clone(),
                version: identity.version.clone(),
                digest: identity.digest.clone(),
                kind: PackKind::Documents,
                assets: materialized_assets,
                dependency_assets,
                plugins: identity.plugins.clone(),
                ux: pack.manifest().metadata.ux.clone(),
                installed_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                installed_via: Some(gents::pack::InstalledVia::Operator),
                source: Some(pack.provenance()),
                source_history: Vec::new(),
            },
            gents::pack::InstalledVia::Operator,
            explicit,
        )
        .context("the pack documents were written, but the home install record was not")?;
    }
    Ok(json!({
        "pack": pack.manifest().name,
        "source": pack.label(),
        "digest": pack.digest(),
        "owner": owner,
        "inference": inference,
        "dependency_inference": dependency_inference,
        "origin_tag": origin_tag,
        "dependencies": pack.manifest().metadata.dependencies,
        "schemas": schemas,
        "apply": apply,
    }))
}

/// A plugins pack's slots as the review binds them, from the node its model
/// slots would run on. Without that node (offline, or no slots declared) the
/// preview still lists `inference_slots`, and the install itself checks the
/// bindings.
async fn plugin_inference_needs(
    scope: &ScopeArgs,
    manifest: &PackManifest,
    requested: &gents::pack::PackInferenceBindings,
) -> Option<serde_json::Value> {
    if manifest.metadata.inference_slots.is_empty() {
        return None;
    }
    let (access, owner) = match resolve_scope_owner(scope).await {
        Ok(resolved) => resolved,
        Err(error) => {
            tracing::warn!(error = %gents_protocol::error_chain::ErrorChain(error.as_ref()), "plugins pack preview could not read the node's inference profiles");
            return None;
        }
    };
    let inference = super::review::pack_inference(&access, &owner, manifest, requested).await;
    if let Some(node) = access.local_node() {
        if let Err(error) = node.shutdown().await {
            tracing::warn!(error = %gents_protocol::error_chain::ErrorChain(&error), "failed to shut down embedded node");
        }
    }
    inference.map(|inference| super::review::inference_needs(&[inference]))
}

fn merge(into: &mut serde_json::Value, from: serde_json::Value) {
    if let (Some(into), serde_json::Value::Object(from)) = (into.as_object_mut(), from) {
        into.extend(from);
    }
}

/// Refuses, before any write, a pack whose content digest is not the one the
/// operator reviewed with `gents config pack show`.
pub(super) fn check_expected_digest(expected: Option<&str>, actual: &str) -> Result<()> {
    match expected {
        Some(expected) if expected != actual => anyhow::bail!(
            "the pack's digest is {actual}, not the expected {expected}. Nothing was installed. \
             Next: gents config pack show ./dir"
        ),
        _ => Ok(()),
    }
}

/// `--wire` acts on plugins this install places, so a pack that cannot ship
/// one refuses it before any write.
pub(super) fn check_wire_target(manifest: &PackManifest, wire: &[String]) -> Result<()> {
    if wire.is_empty() {
        return Ok(());
    }
    anyhow::ensure!(
        matches!(
            manifest.metadata.kind,
            PackKind::Documents | PackKind::Plugins
        ),
        "--wire applies only to documents and plugins packs; {} is not one",
        manifest.name
    );
    anyhow::ensure!(
        !manifest.metadata.plugins.is_empty(),
        "--wire needs a pack that ships a plugin; {} ships none",
        manifest.name
    );
    Ok(())
}

/// Wires each plugin `pack` shipped into each of `agents`, agent by agent,
/// through the same writer as `gents config pack wire`.
pub(crate) async fn wire_pack_plugins(
    access: &gents::store::config_client::ConfigAccess,
    owner: &str,
    home: &std::path::Path,
    pack: &PackSource,
    agents: &[String],
) -> Result<Vec<serde_json::Value>> {
    let namespace = &pack.manifest().metadata.namespace;
    let mut wired = Vec::new();
    for agent in agents {
        for plugin in &pack.manifest().metadata.plugins {
            let request = wire::WireRequest {
                plugin: format!("{namespace}/{}", plugin.name),
                agent: agent.clone(),
                targets_from: Vec::new(),
                remove: false,
                preview: false,
            };
            wired.push(wire::wire_in(access, owner, home, &request).await?);
        }
    }
    Ok(wired)
}

/// Offline command access owns its node and store claim. DefraDB background
/// tasks may retain the database after a handle drops, so shutdown must finish
/// before this command releases its access. HTTP access has no owned node here.
pub(super) async fn install_documents_command_with_local(
    access: gents::store::config_client::ConfigAccess,
    pack: &PackSource,
    args: &PackInstallArgs,
    local_endpoint: Option<gents::store::config_client::GraphqlEndpoint>,
) -> Result<()> {
    let result = async {
        let home = crate::home_state::resolve_home_dir(args.scope.home.as_deref());
        let bind_mode = if args.scope.graphql.is_some() {
            Some(ManifestNodeDidBindingArg::Live)
        } else {
            Some(ManifestNodeDidBindingArg::Home)
        };
        let owner = super::super::config::binding::resolve_target_node_did(
            args.scope.node_did.as_deref(),
            if args.scope.node_did.is_some() {
                None
            } else {
                bind_mode
            },
            args.scope.home.as_deref(),
            args.scope.graphql.as_deref(),
            Some(&access),
        )
        .await?;
        let local = if let Some(endpoint) = local_endpoint {
            let config =
                crate::home_state::read_init_config(&home)?.context("home is not initialized")?;
            let runtime = crate::home_state::load_initialized_home_identity(&home, &config)?;
            anyhow::ensure!(
                runtime.did() == owner,
                "selected Node does not match the held runtime"
            );
            let node = access
                .local_node()
                .context("local pack install requires the owned Node")?
                .clone();
            Some(LocalPackAccess {
                writer: gents::store::config_client::ConfigAccess::Graphql(endpoint),
                schema: gents::store::config_client::ConfigAccess::local_as(
                    node,
                    identity::Did::new(&owner)?,
                ),
                runtime,
            })
        } else {
            None
        };
        let mut output = install_documents_pack_inner(
            &access,
            &owner,
            pack,
            args,
            args.explicit,
            local.as_ref(),
        )
        .await?;
        if !args.wire.is_empty() && !args.preview && args.scope.graphql.is_none() {
            output["wired"] = wire_pack_plugins(&access, &owner, &home, pack, &args.wire)
                .await?
                .into();
        }
        Ok::<_, anyhow::Error>(output)
    }
    .await;
    if let Some(node) = access.local_node() {
        if let Err(error) = node.shutdown().await {
            tracing::warn!(error = %gents_protocol::error_chain::ErrorChain(&error), "failed to shut down embedded node");
        }
    }
    crate::print_json(&result?)
}

pub(crate) async fn install(args: PackInstallArgs) -> Result<()> {
    let home = crate::home_state::resolve_home_dir(args.scope.home.as_deref());
    let pack = resolve_pack_source(&args.package, args.registry.as_deref(), &home).await?;
    tracing::info!(package = %args.package, source = %pack.describe(), "resolved pack");
    check_expected_digest(args.expect_digest.as_deref(), pack.digest())?;
    check_wire_target(pack.manifest(), &args.wire)?;
    args.output.ensure_supported(
        "config pack install",
        &[crate::cli::output_format::OutputFormat::Json],
    )?;
    match pack.manifest().metadata.kind {
        PackKind::Documents => {
            let (access, _, local_endpoint) = crate::resolve_pack_config_access(
                args.scope.home.as_deref(),
                args.scope.graphql.as_deref(),
            )
            .await?;
            install_documents_command_with_local(access, &pack, &args, local_endpoint).await
        }
        PackKind::Assets | PackKind::Plugins => {
            anyhow::ensure!(
                args.dependency_variables.is_empty(),
                "--dependency-variable applies only to document packs with dependencies"
            );
            let requested = parse_inference_slot_bindings(&args.inference_slots)?;
            anyhow::ensure!(
                args.scope.graphql.is_none()
                    && (args.scope.node_did.is_none() || !requested.is_empty())
                    && !args.force_rebind_concrete_did,
                "asset and plugins packs install locally with --home; identity flags do not apply"
            );
            if args.preview {
                let mut preview = json!({
                    "pack": pack.manifest().name,
                    "source": pack.label(),
                    "digest": pack.digest(),
                    "inference_slots": pack.manifest().metadata.inference_slots,
                    "kind": pack.manifest().metadata.kind,
                    "would_write": false,
                });
                merge(
                    &mut preview,
                    super::review::identity_and_authority(pack.manifest(), &[])?,
                );
                if let Some(needs) =
                    plugin_inference_needs(&args.scope, pack.manifest(), &requested).await
                {
                    preview["inference_needs"] = needs;
                }
                return crate::print_json(&preview);
            }
            let slot_owner =
                resolve_plugin_slot_owner(&args.scope, pack.manifest(), &requested).await?;
            let home = args
                .scope
                .home
                .clone()
                .context("asset and plugins packs require --home")?;
            let notification_scope = plugin_notification_scope(&home).await?;
            if let Some((gents::ConfigAccess::Graphql(endpoint), owner)) = &notification_scope {
                let dependencies =
                    futures_util::stream::iter(pack.manifest().metadata.dependencies.iter())
                        .map(|coordinate| {
                            resolve_pack_source(coordinate, args.registry.as_deref(), &home)
                        })
                        .buffered(4)
                        .collect::<Vec<_>>()
                        .await
                        .into_iter()
                        .collect::<Result<Vec<_>>>()?;
                let result = crate::http::pack_install_client::install_remote(
                    endpoint,
                    owner,
                    &pack,
                    &dependencies,
                    &args,
                    true,
                )
                .await?;
                return crate::print_json(&result);
            }
            let notify_scope = notification_scope.clone();
            let notify_home = home.clone();
            let result = gents::pack::plugin_lifecycle::install(
                &home,
                &pack.archive,
                args.grant_authority,
                slot_owner.as_deref(),
                &requested,
                pack.provenance(),
                move |identity, changes| {
                    let home = notify_home;
                    async move {
                        if let Some((access, owner)) = notify_scope {
                            gents::pack::record_staged_plugin_store_change(
                                &access,
                                &owner,
                                &home,
                                &identity.coordinate,
                                Some(&identity),
                                &changes,
                            )
                            .await
                        } else {
                            Ok(())
                        }
                    }
                },
            )
            .await;
            if let Some((access, _)) = &notification_scope {
                if let Some(node) = access.local_node() {
                    if let Err(error) = node.shutdown().await {
                        tracing::warn!(error = %gents_protocol::error_chain::ErrorChain(&error), "failed to shut down embedded node");
                    }
                }
            }
            let (record, installed_plugins) = result?;
            let root = home.join(&record.assets);
            let mut output = json!({
                "pack": pack.manifest().name,
                "digest": pack.digest(),
                "installed_assets": root,
                "installed_plugins": installed_plugins,
                "record": record,
                "source": pack.label(),
            });
            if !args.wire.is_empty() {
                let (access, owner) = resolve_scope_owner(&args.scope).await?;
                output["wired"] = wire_pack_plugins(&access, &owner, &home, &pack, &args.wire)
                    .await?
                    .into();
            }
            crate::print_json(&output)
        }
    }
}
