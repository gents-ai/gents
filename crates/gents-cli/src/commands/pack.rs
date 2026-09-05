//! One package-facing CLI; install writes stay with their existing owners.
use crate::cli::*;
use anyhow::{Context, Result};
use gents::pack::{pack_catalog, resolve_pack, PackKind, ResolvedPack};
use serde_json::json;

pub(crate) async fn dispatch(command: PackCommand) -> Result<()> {
    match command {
        PackCommand::List => {
            let entries: Vec<_> = pack_catalog()?.into_iter().map(|pack| json!({
                "name":pack.name,"version":pack.version,"description":pack.description,
                "kind":pack.metadata.kind,"authors":pack.metadata.authors,"tags":pack.metadata.tags
            })).collect();
            crate::print_json(&json!({"packs":entries}))
        }
        PackCommand::Show(args) => {
            let pack = resolve_pack(&args.package)?;
            let graph = if matches!(pack.manifest.metadata.kind, PackKind::Graph) {
                Some(
                    gents::graph_package::load_bundled_graph_package(&args.package)?
                        .catalog_entry(),
                )
            } else {
                None
            };
            crate::print_json(
                &json!({"manifest": pack.manifest, "digest": pack.digest, "graph": graph}),
            )
        }
        PackCommand::Install(args) => install(args).await,
        PackCommand::Run(args) => super::demo::pack::run(args).await,
        PackCommand::Init(args) => super::demo::pack::init_pack(args).await,
        PackCommand::Seed(args) => super::demo::pack::seed(args).await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_bundled_document_pack_materializes_a_valid_configuration() {
        for manifest in pack_catalog().unwrap() {
            if manifest.metadata.kind != PackKind::Documents {
                continue;
            }
            let pack = resolve_pack(&manifest.name).unwrap();
            let root = tempfile::tempdir().unwrap();
            materialize(&pack, root.path()).unwrap();
            let (_, report) = crate::desired_state::load_manifest_root(root.path());
            assert!(
                report.errors.is_empty(),
                "{}: {:?}",
                manifest.name,
                report.errors
            );
        }
    }
}

fn materialize(pack: &ResolvedPack, root: &std::path::Path) -> Result<()> {
    use std::io::Write;
    for path in std::iter::once("manifest.json")
        .chain(pack.manifest.metadata.assets.iter().map(String::as_str))
    {
        let destination = root.join(path);
        std::fs::create_dir_all(destination.parent().context("asset parent")?)?;
        let bytes = pack.asset(path)?;
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&destination)
        {
            Ok(mut file) => file.write_all(bytes)?,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                anyhow::ensure!(
                    std::fs::read(&destination)? == bytes,
                    "installed asset was modified: {}",
                    destination.display()
                );
            }
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

pub(crate) fn materialize_named_pack(name: &str) -> Result<std::path::PathBuf> {
    let pack = resolve_pack(name)?;
    let root = crate::home_state::resolve_home_dir(None)
        .join("packs")
        .join(name)
        .join(&pack.digest);
    materialize(&pack, &root)?;
    Ok(root)
}

async fn install(args: PackInstallArgs) -> Result<()> {
    args.output.ensure_supported(
        "pack install",
        &[
            crate::cli::output_format::OutputFormat::Text,
            crate::cli::output_format::OutputFormat::Json,
        ],
    )?;
    let pack = resolve_pack(&args.package)?;
    match pack.manifest.metadata.kind {
        PackKind::Graph => {
            anyhow::ensure!(
                !args.force_rebind_concrete_did,
                "--force-rebind-concrete-did applies only to document packs"
            );
            super::graph::install(args, true).await
        }
        PackKind::Documents => {
            anyhow::ensure!(args.bindings.is_none() && args.scope.agent_did.is_none(), "document packs bind to the target node; --bindings/--agent-did are graph installation options");
            // Resolve every dependency before writing any configuration.
            let dependencies = pack
                .manifest
                .metadata
                .dependencies
                .iter()
                .map(|name| resolve_pack(name))
                .collect::<Result<Vec<_>>>()?;
            for dependency in &dependencies {
                anyhow::ensure!(
                    matches!(dependency.manifest.metadata.kind, PackKind::Graph),
                    "only graph dependencies are currently installable"
                );
            }
            let temp = tempfile::tempdir()?;
            materialize(&pack, temp.path())?;
            let (_, report) = crate::desired_state::load_manifest_root(temp.path());
            anyhow::ensure!(
                report.errors.is_empty(),
                "invalid pack configuration: {:?}",
                report.errors
            );
            for dependency in dependencies {
                super::graph::install(
                    PackInstallArgs {
                        package: dependency.manifest.name,
                        bindings: None,
                        scope: args.scope.clone(),
                        output: args.output,
                        force_rebind_concrete_did: false,
                    },
                    false,
                )
                .await?;
            }
            let binding = if args.scope.graphql.is_some() {
                ManifestAgentDidBindingArg::Live
            } else {
                ManifestAgentDidBindingArg::Home
            };
            super::config::dispatch(ConfigCommand::Apply(ConfigApplyArgs {
                root: temp.path().to_owned(),
                home: args.scope.home,
                graphql: args.scope.graphql,
                bind_agent_did: Some(binding),
                force_rebind_concrete_did: args.force_rebind_concrete_did,
                prune: false,
            }))
            .await
        }
        PackKind::Assets => {
            anyhow::ensure!(
                args.bindings.is_none()
                    && args.scope.graphql.is_none()
                    && args.scope.agent_did.is_none()
                    && !args.force_rebind_concrete_did,
                "asset packs install locally with --home; identity and graph binding flags do not apply"
            );
            let home = args.scope.home.context("asset packs require --home")?;
            let root = home
                .join("packs")
                .join(&pack.manifest.name)
                .join(&pack.digest);
            materialize(&pack, &root)?;
            crate::print_json(
                &json!({"pack":pack.manifest.name,"digest":pack.digest,"installed_assets":root}),
            )
        }
    }
}
