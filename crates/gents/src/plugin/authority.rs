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

/// What operator consent for `plugin` covers, beyond [`describe_manifold`]'s
/// own axes: whether it also reads one caller-bound directory per call
/// (never standing authority - see [`super::BoundDir`]) and what resource
/// ceiling it declares. `None` when `plugin` asks for nothing at all: no
/// manifold grant, no `bind_dir`, no `limits`.
pub fn describe_plugin_authority(plugin: &PackPlugin, manifold: &Manifold) -> Option<String> {
    let mut parts: Vec<String> = describe_manifold(manifold).into_iter().collect();
    if let Some(binding) = &plugin.bind_dir {
        parts.push(
            match binding.access {
                crate::pack::BindAccess::Read => "reads one directory its caller binds, per call",
                crate::pack::BindAccess::ReadWrite => {
                    "reads and writes one directory its caller binds, per call"
                }
            }
            .to_owned(),
        );
    }
    if let Some(limits) = &plugin.limits {
        if let Some(description) = describe_limits(limits) {
            parts.push(description);
        }
    }
    (!parts.is_empty()).then(|| parts.join("; "))
}

/// One line naming the resource ceiling `limits` declares, or `None` when it
/// declares nothing.
fn describe_limits(limits: &crate::pack::PluginLimits) -> Option<String> {
    let mut bits = Vec::new();
    if let Some(memory_mib) = limits.memory_mib {
        bits.push(format!("{memory_mib} MiB memory"));
    }
    if let Some(wall_clock_secs) = limits.wall_clock_secs {
        bits.push(format!("{wall_clock_secs}s wall clock"));
    }
    if let Some(max_output_mib) = limits.max_output_mib {
        bits.push(format!("{max_output_mib} MiB output"));
    }
    (!bits.is_empty()).then(|| format!("up to {}", bits.join(", ")))
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

pub(super) fn limits_consented(
    requested: Option<&crate::pack::PluginLimits>,
    previous: Option<&crate::pack::PluginLimits>,
    consent: bool,
) -> bool {
    let empty = crate::pack::PluginLimits::default();
    let requested = requested.unwrap_or(&empty);
    let previous = previous.unwrap_or(&empty);
    consent
        || (requested.memory_mib.unwrap_or(0) <= previous.memory_mib.unwrap_or(0)
            && requested.wall_clock_secs.unwrap_or(0) <= previous.wall_clock_secs.unwrap_or(0)
            && requested.max_output_mib.unwrap_or(0) <= previous.max_output_mib.unwrap_or(0))
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

    #[test]
    fn a_plugin_asking_for_nothing_has_no_authority_to_describe() {
        assert_eq!(
            describe_plugin_authority(&plugin(None), &Manifold::sealed()),
            None
        );
    }

    #[test]
    fn bind_dir_and_limits_are_named_even_with_a_sealed_manifold() {
        let mut declaration = plugin(None);
        declaration.bind_dir = Some(crate::pack::PluginDirBinding {
            input_field: "root".to_owned(),
            original_field: None,
            description: "The directory to scan".to_owned(),
            access: Default::default(),
        });
        declaration.limits = Some(crate::pack::PluginLimits {
            memory_mib: Some(512),
            wall_clock_secs: Some(300),
            max_output_mib: Some(4),
        });
        let description =
            describe_plugin_authority(&declaration, &Manifold::sealed()).expect("must describe");
        assert!(description.contains("reads one directory its caller binds, per call"));
        assert!(description.contains("512 MiB memory"));
        assert!(description.contains("300s wall clock"));
        assert!(description.contains("4 MiB output"));
    }

    #[test]
    fn manifold_authority_and_bind_dir_both_appear_together() {
        let mut declaration = plugin(None);
        declaration.bind_dir = Some(crate::pack::PluginDirBinding {
            input_field: "root".to_owned(),
            original_field: None,
            description: "scan target".to_owned(),
            access: Default::default(),
        });
        let manifold = Manifold {
            net: NetAccess::OutboundHttp(None),
            ..Manifold::sealed()
        };
        let description =
            describe_plugin_authority(&declaration, &manifold).expect("must describe");
        assert!(description.contains("HTTP to any host"));
        assert!(description.contains("reads one directory its caller binds, per call"));
    }
}
