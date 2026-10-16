use crate::cli::args::P2pRelayModeArg;

pub(crate) fn default_relay_urls() -> Vec<String> {
    iroh::RelayMode::Default
        .relay_map()
        .urls::<Vec<_>>()
        .iter()
        .map(|url| url.to_string())
        .collect()
}

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
