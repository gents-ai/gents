use std::time::Duration;

pub const DEFAULT_HEARTBEAT: Duration = Duration::from_secs(30);
pub const DEFAULT_SWEEP: Duration = Duration::from_secs(30);
pub const DEFAULT_STALE_MULTIPLE: u32 = 3;

/// Cadences of the P2P reconcile loops, built by the server from its flags.
/// Every interval is non-zero: a zero interval panics `tokio::time::interval`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct P2pIntervals {
    pub registry_heartbeat: Duration,
    pub pairing_sweep: Duration,
    pub endpoint_heartbeat: Duration,
    pub registry_stale_after: Duration,
}

impl Default for P2pIntervals {
    fn default() -> Self {
        Self::with_overrides(None, None, None, None)
    }
}

impl P2pIntervals {
    /// An unset endpoint heartbeat follows the registry heartbeat, and an
    /// unset stale threshold is `DEFAULT_STALE_MULTIPLE` registry heartbeats.
    pub fn with_overrides(
        registry_heartbeat: Option<Duration>,
        pairing_sweep: Option<Duration>,
        endpoint_heartbeat: Option<Duration>,
        registry_stale_after: Option<Duration>,
    ) -> Self {
        let registry_heartbeat = registry_heartbeat.unwrap_or(DEFAULT_HEARTBEAT);
        Self {
            registry_heartbeat,
            pairing_sweep: pairing_sweep.unwrap_or(DEFAULT_SWEEP),
            endpoint_heartbeat: endpoint_heartbeat.unwrap_or(registry_heartbeat),
            registry_stale_after: registry_stale_after
                .unwrap_or(registry_heartbeat * DEFAULT_STALE_MULTIPLE),
        }
    }

    pub fn lease_renewal_interval(&self) -> Duration {
        (self.registry_stale_after / DEFAULT_STALE_MULTIPLE).max(Duration::from_millis(1))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_the_documented_cadences() {
        let intervals = P2pIntervals::default();
        assert_eq!(intervals.registry_heartbeat, Duration::from_secs(30));
        assert_eq!(intervals.pairing_sweep, Duration::from_secs(30));
        assert_eq!(intervals.endpoint_heartbeat, Duration::from_secs(30));
        assert_eq!(intervals.lease_renewal_interval(), Duration::from_secs(30));
        assert_eq!(intervals.registry_stale_after, Duration::from_secs(90));
    }

    #[test]
    fn unset_values_derive_from_the_registry_heartbeat() {
        let intervals = P2pIntervals::with_overrides(
            Some(Duration::from_millis(1250)),
            Some(Duration::from_millis(500)),
            None,
            None,
        );
        assert_eq!(intervals.pairing_sweep, Duration::from_millis(500));
        assert_eq!(intervals.endpoint_heartbeat, Duration::from_millis(1250));
        assert_eq!(intervals.registry_stale_after, Duration::from_millis(3750));
        assert_eq!(
            intervals.lease_renewal_interval(),
            Duration::from_millis(1250)
        );
    }

    #[test]
    fn explicit_values_are_used_as_given() {
        let intervals = P2pIntervals::with_overrides(
            Some(Duration::from_millis(1250)),
            Some(Duration::from_millis(500)),
            Some(Duration::from_millis(750)),
            Some(Duration::from_millis(2500)),
        );
        assert_eq!(intervals.endpoint_heartbeat, Duration::from_millis(750));
        assert_eq!(intervals.registry_stale_after, Duration::from_millis(2500));
        assert_eq!(
            intervals.lease_renewal_interval(),
            Duration::from_millis(2500) / DEFAULT_STALE_MULTIPLE
        );
    }
}
