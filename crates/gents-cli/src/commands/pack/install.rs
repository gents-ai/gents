use super::scope::resolve_plugin_slot_owner;
use super::{plugin_notification_scope, resolve_scope_owner};
use crate::cli::*;
use anyhow::{Context, Result};
use futures_util::StreamExt;
use gents_pack::operations::pack::{admit_digest, parse_inference_slots};
use gents_pack::pack::install_flow::{
    check_wire_target, has_plugin_references, install_documents_pack_resolved_inner, merge,
    DocumentsInstallRequest, LocalPackAccess,
};
use gents_pack::pack::installation_roots;
use gents_pack::pack::plugin_wire::wire_pack_plugins;
use gents_pack::pack::resolved_pack::{resolve_pack_source, ResolvedPack};
use gents_pack::pack::{PackChangeError, PackKind, PackManifest};
use gents_protocol::operations::pack::{PackActor, PackOperation};
use serde_json::json;

/// The engine's request for `args`, with its home resolved.
pub(crate) fn install_request(args: &PackInstallArgs) -> DocumentsInstallRequest {
    DocumentsInstallRequest {
        home: gents_service::home_state::resolve_home_dir(args.scope.home.as_deref()),
        remote: args.scope.graphql.is_some(),
        inference_slots: args.inference_slots.clone(),
        dependency_variables: args.dependency_variables.clone(),
        bind_agents: args.bind_agents.clone(),
        preview: args.preview,
        force_rebind_concrete_did: args.force_rebind_concrete_did,
        edited: args.drift.edited(),
        grant_authority: args.grant_authority,
        wire: args.wire.clone(),
    }
}

/// The documents pack install: the CLI's single site that writes a documents
/// pack and its dependencies. `explicit` is false only when the pack is
/// installed as another pack's dependency.
pub(crate) async fn install_documents_pack(
    access: &gents_store::config_client::ConfigAccess,
    owner: &str,
    pack: &ResolvedPack,
    args: &PackInstallArgs,
    explicit: bool,
) -> Result<serde_json::Value, PackChangeError> {
    let dependencies = resolve_dependencies(pack, args).await?;
    install_documents_pack_inner(
        access,
        owner,
        pack,
        args,
        explicit,
        dependencies,
        None,
        false,
    )
    .await
}

/// The archives of the packs `pack` declares as dependencies.
async fn resolve_dependencies(
    pack: &ResolvedPack,
    args: &PackInstallArgs,
) -> Result<Vec<ResolvedPack>> {
    let home = gents_service::home_state::resolve_home_dir(args.scope.home.as_deref());
    futures_util::stream::iter(pack.manifest().metadata.dependencies.iter())
        .map(|coordinate| resolve_pack_source(coordinate, args.registry.as_deref(), &home))
        .buffered(4)
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect()
}

#[expect(
    clippy::too_many_arguments,
    reason = "the install inputs each entry point supplies, and whether it asks approval for a root grant"
)]
async fn install_documents_pack_inner(
    access: &gents_store::config_client::ConfigAccess,
    owner: &str,
    pack: &ResolvedPack,
    args: &PackInstallArgs,
    explicit: bool,
    dependencies: Vec<ResolvedPack>,
    local: Option<&LocalPackAccess>,
    refuse_unapproved_root: bool,
) -> Result<serde_json::Value, PackChangeError> {
    let request = install_request(args);
    if let gents_store::config_client::ConfigAccess::Graphql(endpoint) = access {
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
            let config: serde_json::Value =
                serde_json::from_slice(source.asset(path)?).map_err(anyhow::Error::from)?;
            if has_plugin_references(&config) {
                needs_runtime_store = true;
                break;
            }
        }
        if needs_runtime_store && args.preview {
            return gents_host::http::pack_install::install_remote(
                endpoint,
                owner,
                pack,
                &dependencies,
                &request,
                explicit,
            )
            .await
            .map_err(Into::into);
        }
        if needs_runtime_store {
            return gents_host::http::pack_install_client::install_remote(
                endpoint,
                owner,
                pack,
                &dependencies,
                &request,
                explicit,
            )
            .await
            .map_err(Into::into);
        }
    }
    install_documents_pack_resolved_inner(
        access,
        owner,
        pack,
        &request,
        explicit,
        dependencies,
        local,
        &gents_tools::surface::PackRoots,
        refuse_unapproved_root,
    )
    .await
}

/// A plugins pack's slots as the review binds them, from the node its model
/// slots would run on. Without that node (offline, or no slots declared) the
/// preview still lists `inference_slots`, and the install itself checks the
/// bindings.
async fn plugin_inference_needs(
    scope: &ScopeArgs,
    manifest: &PackManifest,
    requested: &gents_pack::pack::PackInferenceBindings,
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
    let inference =
        gents_pack::pack::install_review::pack_inference(&access, &owner, manifest, requested)
            .await;
    if let Some(node) = access.local_node() {
        if let Err(error) = node.shutdown().await {
            tracing::warn!(error = %gents_protocol::error_chain::ErrorChain(&error), "failed to shut down embedded node");
        }
    }
    inference.map(|inference| gents_pack::pack::install_review::inference_needs(&[inference]))
}

/// Offline command access owns its node and store claim. DefraDB background
/// tasks may retain the database after a handle drops, so shutdown must finish
/// before this command releases its access. HTTP access has no owned node here.
pub(super) async fn install_documents_command_with_local(
    access: gents_store::config_client::ConfigAccess,
    pack: &ResolvedPack,
    args: &PackInstallArgs,
    local_endpoint: Option<gents_store::config_client::GraphqlEndpoint>,
) -> Result<(), PackChangeError> {
    let (receipt, failed_step) =
        install_documents_report(access, pack, args, local_endpoint).await?;
    crate::print_json(&receipt)?;
    match failed_step {
        Some(failed) => Err(anyhow::anyhow!(
            "the pack is installed, but {failed}; the receipt above lists each step that failed and the command that completes it"
        )
        .into()),
        None => Ok(()),
    }
}

/// The install's receipt, and the step that failed after the install
/// committed: the pack is on the node by then, so its receipt is returned
/// with the failed step named rather than lost with the error.
pub(super) async fn install_documents_report(
    access: gents_store::config_client::ConfigAccess,
    pack: &ResolvedPack,
    args: &PackInstallArgs,
    local_endpoint: Option<gents_store::config_client::GraphqlEndpoint>,
) -> Result<(serde_json::Value, Option<String>), PackChangeError> {
    let result = async {
        let mut failed_step = None;
        let (home, owner, local) = async {
            let home = gents_service::home_state::resolve_home_dir(args.scope.home.as_deref());
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
                let config = gents_service::home_state::read_init_config(&home)?
                    .context("home is not initialized")?;
                let runtime =
                    gents_service::home_state::load_initialized_home_identity(&home, &config)?;
                anyhow::ensure!(
                    runtime.did() == owner,
                    "selected Node does not match the held runtime"
                );
                let node = access
                    .local_node()
                    .context("local pack install requires the owned Node")?
                    .clone();
                Some(LocalPackAccess {
                    writer: gents_store::config_client::ConfigAccess::Graphql(endpoint),
                    schema: gents_store::config_client::ConfigAccess::local_as(
                        node,
                        identity::Did::new(&owner)?,
                    ),
                    runtime,
                })
            } else {
                None
            };
            anyhow::Ok((home, owner, local))
        }
        .await?;
        // Refused before anything is resolved or written.
        let reviewed = args
            .root_agents
            .as_deref()
            .map(installation_roots::reviewed_root_agents)
            .transpose()?;
        let dependencies = resolve_dependencies(pack, args).await?;
        // The root pack and its dependencies: the packs whose records the
        // grant reads, whichever install first put them on the node.
        let coordinates: Vec<String> = std::iter::once(pack)
            .chain(&dependencies)
            .map(|source| {
                format!(
                    "{}/{}",
                    source.manifest().metadata.namespace,
                    source.manifest().name
                )
            })
            .collect();
        let mut output = install_documents_pack_inner(
            &access,
            &owner,
            pack,
            args,
            args.explicit,
            dependencies,
            local.as_ref(),
            true,
        )
        .await?;
        if !args.preview {
            // What the approver was shown: the rows a separate review named
            // back, or with none given the rows of this install's own plan,
            // which its receipt lists and `--grant-authority` approves.
            let planned = installation_roots::take_planned_root_agents(&mut output);
            let shown = reviewed.unwrap_or(planned);
            let readers = match gents_pack::pack::installer_tool_root(&access, &owner, None).await
            {
                Ok(supplied) => {
                    gents_tools::surface::grant_pack_root_readers(
                        &access,
                        &owner,
                        &coordinates,
                        &shown,
                        supplied.as_deref(),
                        args.grant_authority,
                    )
                    .await
                }
                Err(error) => Err(error),
            };
            match readers {
                Ok(readers) => {
                    if readers.iter().any(|reader| reader.get("failed").is_some()) {
                        failed_step =
                            Some("an agent it ships was not granted its tool root".to_owned());
                    }
                    if !readers.is_empty() {
                        output["host_tool_roots"] = readers.into();
                    }
                }
                Err(error) => {
                    failed_step =
                        Some("the tool roots its agents need could not be read".to_owned());
                    output["host_tool_roots_failed"] = json!({
                        "error": format!("{error:#}"),
                        "next": "repeat the install with --grant-authority once the cause is resolved",
                    });
                }
            }
        }
        if !args.wire.is_empty() && !args.preview && args.scope.graphql.is_none() {
            output["wired"] = wire_pack_plugins(
                &access,
                &owner,
                &home,
                pack,
                &args.wire,
                gents_service::home_state::load_held_user_identity,
            )
            .await?
            .into();
        }
        Ok::<_, PackChangeError>((output, failed_step))
    }
    .await;
    if let Some(node) = access.local_node() {
        if let Err(error) = node.shutdown().await {
            tracing::warn!(error = %gents_protocol::error_chain::ErrorChain(&error), "failed to shut down embedded node");
        }
    }
    result
}

pub(crate) async fn install(args: PackInstallArgs) -> Result<(), PackChangeError> {
    let home = gents_service::home_state::resolve_home_dir(args.scope.home.as_deref());
    let pack = resolve_pack_source(&args.package, args.registry.as_deref(), &home).await?;
    tracing::info!(package = %args.package, source = %pack.describe(), "resolved pack");
    admit_digest(
        PackActor::User,
        PackOperation::Install,
        !args.preview,
        args.expect_digest.as_deref(),
        pack.digest(),
    )
    .map_err(|error| super::refusal(PackOperation::Install, &args.package, error))?;
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
            if !args.dependency_variables.is_empty() {
                return Err(anyhow::anyhow!(
                    "--dependency-variable applies only to document packs with dependencies"
                )
                .into());
            }
            let requested = parse_inference_slots(&args.inference_slots)
                .map_err(|error| super::refusal(PackOperation::Install, &args.package, error))?;
            if args.scope.graphql.is_some()
                || (args.scope.node_did.is_some() && requested.is_empty())
                || args.force_rebind_concrete_did
            {
                return Err(anyhow::anyhow!(
                    "asset and plugins packs install locally with --home; identity flags do not apply"
                )
                .into());
            }
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
                    gents_pack::pack::install_review::identity_and_authority(pack.manifest(), &[])?,
                );
                if let Some(needs) =
                    plugin_inference_needs(&args.scope, pack.manifest(), &requested).await
                {
                    preview["inference_needs"] = needs;
                }
                return Ok(crate::print_json(&preview)?);
            }
            let slot_owner =
                resolve_plugin_slot_owner(&args.scope, pack.manifest(), &requested).await?;
            let home = args
                .scope
                .home
                .clone()
                .context("asset and plugins packs require --home")?;
            let notification_scope = plugin_notification_scope(&home).await?;
            if let Some((gents_store::config_client::ConfigAccess::Graphql(endpoint), owner)) =
                &notification_scope
            {
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
                let result = gents_host::http::pack_install_client::install_remote(
                    endpoint,
                    owner,
                    &pack,
                    &dependencies,
                    &install_request(&args),
                    true,
                )
                .await?;
                return Ok(crate::print_json(&result)?);
            }
            let notify_scope = notification_scope.clone();
            let notify_home = home.clone();
            let result = gents_pack::pack::plugin_lifecycle::install(
                &home,
                pack.archive(),
                args.grant_authority,
                slot_owner.as_deref(),
                &requested,
                gents_pack::pack::InstalledVia::Operator,
                pack.provenance(),
                move |identity, changes| {
                    let home = notify_home;
                    async move {
                        if let Some((access, owner)) = notify_scope {
                            gents_pack::pack::record_staged_plugin_store_change(
                                &access,
                                &owner,
                                &home,
                                &identity.coordinate,
                                Some(&identity),
                                &changes,
                                &gents_inference::resolved::ProviderProfilePolicy,
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
                output["wired"] = wire_pack_plugins(
                    &access,
                    &owner,
                    &home,
                    &pack,
                    &args.wire,
                    gents_service::home_state::load_held_user_identity,
                )
                .await?
                .into();
            }
            Ok(crate::print_json(&output)?)
        }
    }
}
