//! The documents pack install: the single site that writes a documents pack
//! and its dependencies once both are resolved, and the checks and previews
//! it reports before writing.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{Context, Result};
use serde_json::json;

use crate::pack::install_review;
use crate::pack::resolved_pack::ResolvedPack;
use crate::pack::{PackChangeError, PackKind, PackManifest};

/// How an install treats installed documents someone edited.
#[derive(Clone, Copy, Debug, Default)]
pub struct InstallDrift {
    /// Replace edited pack documents.
    pub overwrite: bool,
    /// Leave edited pack documents as they are.
    pub keep: bool,
}

impl InstallDrift {
    pub fn policy(self) -> crate::pack::DriftPolicy {
        if self.overwrite {
            crate::pack::DriftPolicy::Overwrite
        } else if self.keep {
            crate::pack::DriftPolicy::Keep
        } else {
            crate::pack::DriftPolicy::Refuse
        }
    }
}

/// What a documents pack install was asked for, as its engine reads it.
#[derive(Clone, Debug)]
pub struct DocumentsInstallRequest {
    /// The home whose asset cache, plugin store and install records the
    /// install uses.
    pub home: PathBuf,
    /// The node is reached over GraphQL: it has no local home to read plugins
    /// from or to record the install in.
    pub remote: bool,
    /// `NAME=PROFILE_ID` bindings of declared inference slots.
    pub inference_slots: Vec<String>,
    /// `NAMESPACE/PACK:NAME=VALUE` install variables of dependencies.
    pub dependency_variables: Vec<String>,
    /// Report what the install would write, writing nothing.
    pub preview: bool,
    pub force_rebind_concrete_did: bool,
    pub drift: InstallDrift,
    /// Allow the files, network or environment the pack's plugins ask for.
    pub grant_authority: bool,
    /// Agents to wire the pack's plugins into after the install.
    pub wire: Vec<String>,
}

/// The stores a local install writes through: `writer` for the documents,
/// `schema` for collections, as the held `runtime` Node.
pub struct LocalPackAccess {
    pub writer: gents_store::config_client::ConfigAccess,
    pub schema: gents_store::config_client::ConfigAccess,
    pub runtime: std::sync::Arc<dyn gents_store::identity::NodeIdentity>,
}

pub fn has_plugin_references(config: &serde_json::Value) -> bool {
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

pub fn parse_dependency_variables(
    values: &[String],
) -> Result<crate::pack::DependencyVariableOverrides> {
    let mut result = crate::pack::DependencyVariableOverrides::new();
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

pub fn parse_inference_slot_bindings(
    values: &[String],
) -> Result<crate::pack::PackInferenceBindings> {
    let mut bindings = BTreeMap::new();
    for value in values {
        let (slot, profile_id) = value.split_once('=').with_context(|| {
            format!("invalid inference slot binding {value:?}; expected NAME=PROFILE_ID")
        })?;
        anyhow::ensure!(
            !slot.trim().is_empty() && !profile_id.trim().is_empty(),
            "invalid inference slot binding {value:?}; slot and profile ID must not be blank"
        );
        anyhow::ensure!(
            bindings
                .insert(slot.trim().to_owned(), profile_id.trim().to_owned())
                .is_none(),
            "inference slot {slot:?} was bound more than once"
        );
    }
    Ok(bindings)
}

pub fn reused_variable_preview(
    pack: &ResolvedPack,
    config: &gents_documents::PackConfig,
) -> Result<serde_json::Value> {
    let path = pack
        .manifest()
        .config
        .as_deref()
        .context("pack has no configuration")?;
    let authored: serde_json::Value = serde_json::from_slice(pack.asset(path)?)?;
    let installed = serde_json::to_value(config)?;
    let uses =
        crate::pack::interpolate::inspect_install_variables(&authored, &|_| Some("preview".into()))
            .map_err(|names| {
                anyhow::anyhow!("cannot inspect install variables: {}", names.join(", "))
            })?;
    let uses = uses.into_iter().map(|entry| {
        let mut parts = entry.path.trim_start_matches('/').splitn(3, '/');
        let field = parts.next().unwrap_or_default();
        let next = parts.next();
        if field == "node" {
            return json!({"path": entry.path, "variables": installed_variables(&entry), "document": {"collection": "Node", "id": config.node.node_did}});
        }
        let index = next.and_then(|value| value.parse::<usize>().ok());
        let suffix = parts.next().map(|part| format!("/{part}")).unwrap_or_default();
        let document = index.and_then(|index| {
            let collection = gents_store::collection::Collection::ALL.iter().copied().find(|collection| collection.dir_name() == Some(field))?;
            let authored_id = authored.get(field)?.get(index)?.get(collection.unique_field())?.as_str()?;
            if authored_id.contains('$') { return None; }
            installed.get(field)?.as_array()?.iter().find(|row| row.get(collection.unique_field()).and_then(serde_json::Value::as_str) == Some(authored_id) && row.pointer(&suffix).is_some())?;
            Some(json!({"collection": collection.graphql_type(), "id": authored_id}))
        });
        json!({"path": entry.path, "variables": installed_variables(&entry), "document": document})
    }).collect::<Vec<_>>();
    let runtime_environment = pack.manifest().metadata.plugins.iter().map(|plugin| {
        let manifold = crate::plugin::authority::declared_manifold(plugin)?;
        Ok(json!({"plugin": plugin.name, "declared": crate::plugin::authority::declared_environment(&manifold)}))
    }).collect::<Result<Vec<_>>>()?;
    Ok(
        json!({"pack": format!("{}/{}", pack.manifest().metadata.namespace, pack.manifest().name), "kind": "existing_installation", "uses": uses, "runtime_environment": runtime_environment}),
    )
}

/// One install variable as a preview names it: where its value comes from and
/// whether one is present, never the value or the literal it resolves to. A
/// variable without a value resolves through its `${NAME:-default}`.
fn named_variable(name: &str, value: Option<&str>, supplied: &str) -> serde_json::Value {
    let present = value.is_some_and(|value| !value.is_empty());
    let source = if name == "GENTS_PACK_NODE_DID" {
        "runtime"
    } else if present {
        supplied
    } else {
        "default"
    };
    json!({"name": name, "source": source, "present": present})
}

/// The variables of a use the installed record already resolved.
fn installed_variables(
    entry: &crate::pack::interpolate::InstallVariableUse,
) -> Vec<serde_json::Value> {
    entry
        .variables
        .keys()
        .map(|name| named_variable(name, Some("installed"), "installed"))
        .collect()
}

pub fn install_variable_preview(pack: &ResolvedPack, owner: &str) -> Result<serde_json::Value> {
    install_variable_preview_with(pack, owner, &|name| {
        std::env::var(name).ok().map(|value| (value, "environment"))
    })
}

fn install_variable_preview_with(
    pack: &ResolvedPack,
    owner: &str,
    lookup: &dyn Fn(&str) -> Option<(String, &'static str)>,
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
            lookup(name).map(|(value, _)| value)
        }
    };
    let uses = crate::pack::interpolate::inspect_install_variables(&authored, &environment)
        .map_err(|missing| {
            anyhow::anyhow!(
                "pack config references unset environment variables: {}",
                missing.join(", ")
            )
        })?;
    let uses = uses
        .into_iter()
        .map(|entry| {
            let variables = entry
                .variables
                .iter()
                .map(|(name, value)| {
                    let supplied = lookup(name).map_or("environment", |(_, source)| source);
                    named_variable(name, value.as_deref(), supplied)
                })
                .collect::<Vec<_>>();
            let mut value = json!({"path": entry.path, "variables": variables});
            let mut segments = entry.path.trim_start_matches('/').split('/');
            let collection_name = segments.next().unwrap_or_default();
            let document = if collection_name == "node" {
                Some(json!({"collection": "Node", "id": owner}))
            } else if let Some(collection) = gents_store::collection::Collection::ALL
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
                        crate::pack::interpolate::interpolate_with(id, &environment).ok()
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
            let manifold = crate::plugin::authority::declared_manifold(plugin)?;
            Ok(json!({"plugin": plugin.name,
            "declared": crate::plugin::authority::declared_environment(&manifold)}))
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(
        json!({ "pack": format!("{}/{}", pack.manifest().metadata.namespace, pack.manifest().name),
        "kind": "install_literal", "uses": uses, "runtime_environment": runtime_environment }),
    )
}

/// Installs `pack` with its already resolved `dependencies` through `access`.
/// `explicit` is false only when the pack is installed as another pack's
/// dependency.
pub async fn install_documents_pack_resolved(
    access: &gents_store::config_client::ConfigAccess,
    owner: &str,
    pack: &ResolvedPack,
    args: &DocumentsInstallRequest,
    explicit: bool,
    dependencies: Vec<ResolvedPack>,
) -> Result<serde_json::Value, PackChangeError> {
    install_documents_pack_resolved_inner(access, owner, pack, args, explicit, dependencies, None)
        .await
}

/// [`install_documents_pack_resolved`], writing through `local`'s stores
/// when the install holds the node's own Node.
pub async fn install_documents_pack_resolved_inner(
    access: &gents_store::config_client::ConfigAccess,
    owner: &str,
    pack: &ResolvedPack,
    args: &DocumentsInstallRequest,
    explicit: bool,
    dependencies: Vec<ResolvedPack>,
    local: Option<&LocalPackAccess>,
) -> Result<serde_json::Value, PackChangeError> {
    let home = args.home.clone();
    let requested = parse_inference_slot_bindings(&args.inference_slots)?;
    let temp = tempfile::tempdir().map_err(anyhow::Error::from)?;
    crate::pack::materialize::materialize(pack.archive(), temp.path())?;
    let origin_tag = crate::pack::pack_origin_tag(&format!(
        "{}/{}",
        pack.manifest().metadata.namespace,
        pack.manifest().name
    ))?;
    let shared_collections = if args.preview {
        install_review::shared_collections(access, pack.manifest(), |path| {
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
            "declared_access": install_review::declared_access(&documents),
            "documents": documents,
            "collections": install_review::collections(pack.manifest(), |path| {
                Ok(pack.asset(path)?.to_vec())
            })?,
            "shared_collections": shared_collections,
            "inference_needs": needs,
            "would_write": false,
        });
        let dependency_manifests: Vec<_> =
            dependencies.iter().map(ResolvedPack::manifest).collect();
        merge(
            &mut preview,
            install_review::identity_and_authority(pack.manifest(), &dependency_manifests)?,
        );
        Ok::<_, anyhow::Error>(preview)
    };
    let overrides = parse_dependency_variables(&args.dependency_variables)?;
    let dependency_archives = dependencies
        .iter()
        .map(ResolvedPack::archive)
        .collect::<Vec<_>>();
    let prepared = if let Some(local) = local {
        crate::pack::prepare_document_pack_archive_with_runtime(
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
        crate::pack::prepare_document_pack_archive(
            access,
            owner,
            pack.archive(),
            &requested,
            &dependency_archives,
            crate::pack::InstalledVia::Operator,
            (!args.remote).then_some(home.as_path()),
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
                match install_review::pack_inference(access, owner, source.manifest(), &requested)
                    .await
                {
                    Some(inference) => packs.push(inference),
                    None => return Err(refusal),
                }
            }
            if packs.iter().all(install_review::PackInference::is_bound) {
                return Err(refusal);
            }
            let authored = crate::pack::load_pack_authoring_config(
                pack.manifest(),
                &crate::pack::PackInstallOptions {
                    node_did: owner.to_owned(),
                },
                &|path| Ok(pack.asset(path)?.to_vec()),
                &|name| std::env::var(name).ok(),
            )?;
            let variables = vec![install_variable_preview(pack, owner)?];
            let mut preview = review(
                install_review::authored_documents(
                    &authored,
                    &install_review::VariableFields::of(&variables),
                )?,
                install_review::inference_needs(&packs),
            )?;
            preview["install_variables"] = json!(variables);
            return Ok(preview);
        }
        Err(refusal) => return Err(refusal),
    };
    ResolvedPack::record_graph_sources(&mut prepared, pack, &dependencies);
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
                    install_variable_preview_with(source, owner, &|name| match values.get(name) {
                        Some(value) => value.clone().map(|value| (value, "provided")),
                        None => std::env::var(name).ok().map(|value| (value, "environment")),
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
            .map(|(manifest, preview)| install_review::PackInference::bound(manifest, preview))
            .collect();
        let mut preview = review(
            install_review::documents_by_slot(
                &prepared.config,
                &prepared.agent_bindings,
                &install_review::VariableFields::of(&variables),
            )?,
            install_review::inference_needs(&packs),
        )?;
        preview["inference"] = json!(inference);
        preview["dependency_inference"] = json!(dependency_inference);
        preview["install_variables"] = json!(variables);
        return Ok(preview);
    }
    // A dependency installed on its own behalf (`explicit` false) writes no
    // home record, and a remote node has no local home to record in.
    let record_home = explicit && !args.remote;
    // Every pack that ships an asset class is materialized before any
    // write; the leases are held until the home record is written.
    let mut cache_leases = Vec::new();
    let mut materialized_assets = String::new();
    let mut dependency_assets = Vec::new();
    if record_home {
        for dependency in &dependencies {
            if let Some((assets, lease)) =
                crate::pack::home_install::materialize_asset_pack(&home, dependency.archive())?
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
            crate::pack::home_install::materialize_asset_pack(&home, pack.archive())?
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
    let created_collections =
        crate::pack::collections_install_creates(schema_access, pack.archive()).await?;
    crate::pack::apply_prepared_dependency_schemas(schema_access, &prepared).await?;
    let schemas =
        crate::pack::schema_apply::apply_pack_schemas_if_present(schema_access, temp.path())
            .await
            .context("pack install schemas")?;
    let (apply, plugins) = crate::pack::install_prepared_document_graph(
        writer,
        owner,
        pack.archive(),
        &prepared,
        args.drift.policy(),
        (!args.remote).then_some(home.as_path()),
        args.grant_authority,
        explicit,
        &gents_inference::resolved::ProviderProfilePolicy,
    )
    .await?;
    let root_coordinate = format!(
        "{}/{}",
        pack.manifest().metadata.namespace,
        pack.manifest().name
    );
    let mut identity = crate::pack::PackIdentity::new(
        pack.manifest(),
        pack.digest(),
        plugins
            .iter()
            .filter(|plugin| {
                plugin.owner_pack_coordinate.as_deref() == Some(root_coordinate.as_str())
            })
            .map(|plugin| crate::pack::InstalledPackPlugin {
                name: plugin.name.clone(),
                digest: plugin.digest.clone(),
            })
            .collect(),
    );
    identity.dependencies = dependency_coordinates;
    identity.explicit = explicit;
    if record_home {
        crate::pack::record_after_install(
            &home,
            crate::pack::HomePackInstall {
                coordinate: identity.coordinate.clone(),
                version: identity.version.clone(),
                digest: identity.digest.clone(),
                kind: PackKind::Documents,
                assets: materialized_assets,
                dependency_assets,
                plugins: identity.plugins.clone(),
                ux: pack.manifest().metadata.ux.clone(),
                installed_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                installed_via: Some(crate::pack::InstalledVia::Operator),
                source: Some(pack.provenance()),
                source_history: Vec::new(),
                created_collections,
            },
            crate::pack::InstalledVia::Operator,
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

/// Adds `from`'s fields to the object `into`.
pub fn merge(into: &mut serde_json::Value, from: serde_json::Value) {
    if let (Some(into), serde_json::Value::Object(from)) = (into.as_object_mut(), from) {
        into.extend(from);
    }
}

/// Refuses, before any write, a pack whose content digest is not the one the
/// operator reviewed with `gents config pack show`.
pub fn check_expected_digest(expected: Option<&str>, actual: &str) -> Result<()> {
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
pub fn check_wire_target(manifest: &PackManifest, wire: &[String]) -> Result<()> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dependency_variable_overrides_are_scoped_and_reject_duplicates() {
        let parse = parse_dependency_variables;
        let values = parse(&["acme/alpha:HOST=one=two".into(), "acme/beta:HOST=".into()]).unwrap();
        assert_eq!(values["acme/alpha"]["HOST"].as_deref(), Some("one=two"));
        assert_eq!(values["acme/beta"]["HOST"].as_deref(), Some(""));
        assert!(parse(&["acme/alpha:HOST=one".into(), "acme/alpha:HOST=two".into()]).is_err());
        assert!(parse(&["HOST=one".into()]).is_err());
    }

    #[test]
    fn remote_plugin_routing_does_not_resolve_install_variables() {
        let config =
            json!({"tools": [{"integrations": {"plugins": [{"plugin": "${UNSET_PLUGIN}"}]}}]});
        assert!(has_plugin_references(&config));
        assert!(!has_plugin_references(
            &json!({"tools": [{"integrations": {"plugins": []}}]})
        ));
    }

    #[test]
    fn expect_digest_match_passes() {
        check_expected_digest(Some("sha256:abc"), "sha256:abc").unwrap();
        check_expected_digest(None, "sha256:abc").unwrap();
    }

    #[test]
    fn expect_digest_mismatch_names_both_digests_and_pack_show() {
        let error = format!(
            "{:#}",
            check_expected_digest(Some("sha256:expected"), "sha256:actual").unwrap_err()
        );
        for needle in ["sha256:expected", "sha256:actual", "gents config pack show"] {
            assert!(error.contains(needle), "{needle} missing from {error}");
        }
    }
}
