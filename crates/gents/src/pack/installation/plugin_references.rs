use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::config_client::ConfigAccess;
use crate::document_config::PackConfig;
use crate::pack::PackManifest;

pub const CHECK_PATH: &str = "/plugins/check";

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Check {
    pub plugins: BTreeSet<String>,
}

#[derive(Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct CheckResult {
    pub missing: BTreeSet<String>,
}

/// The plugin store, not pack-installation documents, owns installed resources.
pub fn check_installed(home: &Path, request: &Check) -> Result<CheckResult> {
    let mut missing = BTreeSet::new();
    for coordinate in &request.plugins {
        if !installed(home, coordinate)? {
            missing.insert(coordinate.clone());
        }
    }
    Ok(CheckResult { missing })
}

fn installed(home: &Path, coordinate: &str) -> Result<bool> {
    let (namespace, name) = crate::plugin::store::parse_coordinate(coordinate)?;
    match crate::plugin::store::read_record(home, namespace, name) {
        Ok(_) => Ok(true),
        Err(error) if crate::plugin::store::is_not_found(&error) => Ok(false),
        Err(error) => Err(error).with_context(|| format!("reading installed plugin {coordinate}")),
    }
}

pub(super) fn ensure_local(
    packs: &[(&PackManifest, &PackConfig)],
    home: Option<&Path>,
) -> Result<()> {
    ensure_available(packs, |coordinate| {
        installed(
            home.context("plugin references require the node's plugin store")?,
            coordinate,
        )
    })
}

pub(super) async fn ensure(
    access: &ConfigAccess,
    packs: &[(&PackManifest, &PackConfig)],
    home: Option<&Path>,
) -> Result<()> {
    if home.is_some() {
        return ensure_local(packs, home);
    }
    let mut plugins = BTreeSet::new();
    ensure_available(packs, |coordinate| {
        plugins.insert(coordinate.to_owned());
        Ok(true)
    })?;
    if plugins.is_empty() {
        return Ok(());
    }
    let ConfigAccess::Graphql(endpoint) = access else {
        anyhow::bail!("plugin references require the node's plugin store");
    };
    let mut url = reqwest::Url::parse(endpoint.url())?;
    anyhow::ensure!(
        matches!(url.scheme(), "http" | "https")
            && url.username().is_empty()
            && url.password().is_none()
            && url.query().is_none()
            && url.fragment().is_none(),
        "runtime endpoint must be an HTTP URL without credentials"
    );
    let base = url
        .path()
        .strip_suffix("/api/v0/graphql")
        .context("remote plugin verification requires a Gents runtime GraphQL endpoint")?;
    let path = format!("{base}{CHECK_PATH}");
    url.set_path(&path);
    let client = tokio::task::spawn_blocking(|| {
        reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(std::time::Duration::from_secs(30))
            .build()
    })
    .await??;
    let request = Check { plugins };
    let response = endpoint
        .authorize(client.post(url))?
        .json(&request)
        .send()
        .await
        .context("checking the remote runtime's installed plugins")?;
    anyhow::ensure!(response.status().is_success(),
        "remote plugin verification returned HTTP {}; install through the runtime's local CLI or update and authorize the remote runtime before retrying", response.status());
    let result: CheckResult = response
        .json()
        .await
        .context("decoding remote plugin verification")?;
    anyhow::ensure!(
        result.missing.is_subset(&request.plugins),
        "remote plugin verification returned unexpected plugin coordinates"
    );
    ensure_available(packs, |coordinate| Ok(!result.missing.contains(coordinate)))
}

fn ensure_available(
    packs: &[(&PackManifest, &PackConfig)],
    mut available: impl FnMut(&str) -> Result<bool>,
) -> Result<()> {
    let Some((root, _)) = packs.first() else {
        return Ok(());
    };
    let shipped = packs
        .iter()
        .flat_map(|(manifest, _)| {
            manifest
                .metadata
                .plugins
                .iter()
                .map(|plugin| format!("{}/{}", manifest.metadata.namespace, plugin.name))
        })
        .collect::<BTreeSet<_>>();
    for (manifest, config) in packs {
        for tools in &config.tools {
            let references = tools
                .integrations
                .as_ref()
                .and_then(|integrations| integrations.plugins.as_deref())
                .unwrap_or_default();
            for reference in references {
                crate::plugin::store::parse_coordinate(&reference.plugin)?;
                if shipped.contains(&reference.plugin) || available(&reference.plugin)? {
                    continue;
                }
                anyhow::bail!(
                    "pack {} Tools {:?} names plugin {}, which neither the pack nor its dependencies ship and this node has not installed; install the pack that ships {} first (`gents pack install <namespace>/<pack>`), or declare that pack as a dependency of {}",
                    manifest.name, tools.tools_id, reference.plugin, reference.plugin, root.name
                );
            }
        }
    }
    Ok(())
}
