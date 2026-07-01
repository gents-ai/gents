//! Startup diagnostics for the embedded node's iroh relay configuration.
//!
//! The default relay set is whatever the pinned iroh crate ships. Pre-release
//! iroh versions point `RelayMode::Default` at n0's *canary* infrastructure,
//! which is expected to be unstable — exactly the trigger for the relay
//! `open_path` retry flood that self-DoS'd a host in #588. These helpers make
//! the resolved relay set visible at startup and flag non-production
//! endpoints loudly.

use crate::cli::args::P2pRelayModeArg;

/// Returns the relay URLs that `--p2p-relay-mode default` resolves to under
/// the pinned iroh version.
pub(crate) fn default_relay_urls() -> Vec<String> {
    iroh::RelayMode::Default
        .relay_map()
        .urls::<Vec<_>>()
        .iter()
        .map(|url| url.to_string())
        .collect()
}

/// Filters `urls` down to endpoints that are not n0 production relays
/// (canary or staging infrastructure).
pub(crate) fn non_production_relay_urls<'a>(
    urls: impl IntoIterator<Item = &'a String>,
) -> Vec<String> {
    urls.into_iter()
        .filter(|url| {
            let url = url.to_ascii_lowercase();
            url.contains("canary") || url.contains("staging")
        })
        .cloned()
        .collect()
}

/// Logs the relay configuration the server will run with, and warns loudly
/// if the default relay set includes non-production endpoints.
pub(crate) fn log_relay_mode_diagnostics(mode: P2pRelayModeArg) {
    match mode {
        P2pRelayModeArg::Disabled => {
            tracing::info!(p2p_relay_mode = "disabled", "P2P relay disabled");
        }
        P2pRelayModeArg::Default => {
            let relay_urls = default_relay_urls();
            let non_production = non_production_relay_urls(&relay_urls);
            tracing::info!(
                ?relay_urls,
                p2p_relay_mode = "default",
                "P2P relay configuration"
            );
            if !non_production.is_empty() {
                tracing::warn!(
                    ?non_production,
                    "default iroh relay set includes non-production (canary/staging) endpoints; \
                     these are expected to be unstable and can trigger relay connect churn — \
                     the pinned iroh version likely needs updating (see #588)"
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_production_relay_urls_flags_canary_and_staging() {
        let urls = vec![
            "https://use1-1.relay.n0.iroh.link./".to_string(),
            "https://usw1-1.relay.n0.iroh-canary.iroh.link./".to_string(),
            "https://euc1-1.staging-relay.n0.iroh.link./".to_string(),
        ];

        let flagged = non_production_relay_urls(&urls);

        assert_eq!(
            flagged,
            vec![
                "https://usw1-1.relay.n0.iroh-canary.iroh.link./".to_string(),
                "https://euc1-1.staging-relay.n0.iroh.link./".to_string(),
            ]
        );
    }

    #[test]
    fn non_production_relay_urls_passes_production_set() {
        let urls = vec![
            "https://use1-1.relay.n0.iroh.link./".to_string(),
            "https://euc1-1.relay.n0.iroh.link./".to_string(),
        ];

        assert!(non_production_relay_urls(&urls).is_empty());
    }

    /// Tripwire: the iroh version pinned in Cargo.lock must resolve
    /// `RelayMode::Default` to n0 *production* relays. Pre-release iroh
    /// versions ship canary endpoints as the default relay set, which is the
    /// #588 incident trigger. If this fails, the iroh pin regressed to a
    /// pre-release default relay set.
    #[test]
    fn pinned_iroh_default_relay_set_is_production() {
        let relay_urls = default_relay_urls();

        assert!(
            !relay_urls.is_empty(),
            "default relay map should not be empty"
        );
        assert_eq!(
            non_production_relay_urls(&relay_urls),
            Vec::<String>::new(),
            "pinned iroh RelayMode::Default points at non-production relays; \
             bump the iroh pin (via sourcenetwork/defradb.rs) to a stable release"
        );
    }
}
