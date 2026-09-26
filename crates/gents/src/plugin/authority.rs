//! A plugin's authority: what it declares, what an operator granted, and the
//! consent rule between them.
//!
//! Effective authority is declared ∩ granted, enforced by the runner through
//! [`super::PluginRunner::compile_within`]. A grant is recorded at install; a
//! plugin that asks for nothing needs none, and a new version that asks for
//! more than the recorded grant needs consent again.

use afterburner_core::manifold::{EnvAccess, FsAccess, Manifold, NetAccess};
use anyhow::{Context, Result};

use crate::pack::PackPlugin;

/// The authority `plugin` declares; sealed when it declares none.
pub fn declared_manifold(plugin: &PackPlugin) -> Result<Manifold> {
    match &plugin.manifold {
        Some(value) => serde_json::from_value(value.clone()).with_context(|| {
            format!(
                "plugin {:?} declares authority that is not valid",
                plugin.name
            )
        }),
        None => Ok(Manifold::sealed()),
    }
}

/// Whether `requested` asks for nothing `granted` does not allow.
pub fn manifold_within(requested: &Manifold, granted: &Manifold) -> bool {
    super::narrow_manifold(requested, granted) == *requested
}

/// One line naming what `manifold` allows, or `None` when it allows nothing.
pub fn describe_manifold(manifold: &Manifold) -> Option<String> {
    let mut parts = Vec::new();
    match &manifold.fs {
        FsAccess::None => {}
        FsAccess::ReadOnly(paths) => parts.push(format!("read {}", paths_list(paths))),
        FsAccess::ReadWrite(paths) => parts.push(format!("read and write {}", paths_list(paths))),
    }
    match &manifold.net {
        NetAccess::None => {}
        NetAccess::OutboundHttp(None) => parts.push("HTTP to any host".to_owned()),
        NetAccess::OutboundHttp(Some(hosts)) => parts.push(format!("HTTP to {}", hosts.join(", "))),
        #[allow(unreachable_patterns)]
        _ => parts.push("network access".to_owned()),
    }
    match &manifold.env {
        EnvAccess::None => {}
        EnvAccess::AllowList(keys) => parts.push(format!("environment {}", keys.join(", "))),
        EnvAccess::Full => parts.push("the whole environment".to_owned()),
    }
    if manifold.child_process {
        parts.push("subprocesses".to_owned());
    }
    if manifold.crypto {
        parts.push("cryptography".to_owned());
    }
    (!parts.is_empty()).then(|| parts.join("; "))
}

fn paths_list(paths: &[std::path::PathBuf]) -> String {
    paths
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// The grant to record for `plugin` at install. A plugin that asks for
/// nothing gets the sealed grant; one whose request is within `previous`
/// keeps it; otherwise `consent` must be given, and the request is granted.
pub fn grant_for(
    plugin: &PackPlugin,
    previous: Option<&Manifold>,
    consent: bool,
) -> Result<Manifold> {
    let declared = declared_manifold(plugin)?;
    let Some(asks) = describe_manifold(&declared) else {
        return Ok(Manifold::sealed());
    };
    if previous.is_some_and(|previous| manifold_within(&declared, previous)) || consent {
        return Ok(declared);
    }
    anyhow::bail!(
        "plugin {} asks for {asks}; install it with --grant-authority to allow that",
        plugin.name
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plugin(manifold: Option<serde_json::Value>) -> PackPlugin {
        serde_json::from_value(serde_json::json!({
            "name": "fetcher",
            "description": "Fetches",
            "artifact": "plugins/fetcher.afb",
            "language": "rust",
            "input_schema": {"type": "object"},
            "manifold": manifold,
        }))
        .unwrap()
    }

    #[test]
    fn a_sealed_plugin_needs_no_consent() {
        assert_eq!(
            grant_for(&plugin(None), None, false).unwrap(),
            Manifold::sealed()
        );
    }

    #[test]
    fn authority_needs_consent_once_and_again_when_it_grows() {
        let net = serde_json::json!({"fs": "None", "net": {"OutboundHttp": ["api.example.com"]}, "env": "None", "crypto": false, "child_process": false});
        let error = grant_for(&plugin(Some(net.clone())), None, false).unwrap_err();
        assert!(
            format!("{error:#}").contains("HTTP to api.example.com"),
            "{error:#}"
        );
        let granted = grant_for(&plugin(Some(net.clone())), None, true).unwrap();
        // The same request is covered by the recorded grant.
        assert_eq!(
            grant_for(&plugin(Some(net)), Some(&granted), false).unwrap(),
            granted
        );
        // Asking for more is not.
        let more = serde_json::json!({"fs": "None", "net": {"OutboundHttp": null}, "env": "None", "crypto": false, "child_process": false});
        assert!(grant_for(&plugin(Some(more)), Some(&granted), false).is_err());
    }
}
