//! `ToolServiceHealthState`: the single Rust owner of MCP health vocabulary
//! and its projection to the collapsed operator-facing status.
//!
//! One column (`status` on `ToolServiceHealthState`), one enum. Source of
//! truth: the Lean `HealthState` model in
//! `crates/gents/proofs/Proofs/MCPHealth/State.lean` — `as_str` here must
//! stay byte-identical to that model's `toDefraDB`, which the
//! `lean_vocab_test` fence in `crates/gents/src/health_checker.rs` checks
//! against this crate's `ALL`/`as_str`.

use std::error::Error;
use std::fmt::{Display, Formatter};

/// The full internal MCP service health state, persisted verbatim to the
/// `ToolServiceHealthState` DefraDB collection's `status` column.
///
/// Distinct from [`ToolServiceHealthProjection`]: operators reading the raw
/// row can tell the staleness flavor of degraded (heartbeat lag,
/// `failure_count == 0`) from the failure-count flavor
/// (`1 <= failure_count < K`), and can see `Evicted` vs. `Reconnecting`
/// separately even though both collapse to `Unreachable` downstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolServiceHealthState {
    Healthy,
    Degraded,
    Evicted,
    Reconnecting,
}

/// The collapsed three-state projection every consumer (CLI totals, desktop
/// panel, `HealthStatus`) classifies against. Owned entirely by
/// [`ToolServiceHealthState::project`] — no other layer re-derives it from
/// raw status strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolServiceHealthProjection {
    Healthy,
    Stale,
    Unreachable,
}

impl ToolServiceHealthState {
    pub const ALL: [Self; 4] = [
        Self::Healthy,
        Self::Degraded,
        Self::Evicted,
        Self::Reconnecting,
    ];

    /// String form persisted to the `ToolServiceHealthState` DefraDB
    /// collection. Mirrors `HealthState.toDefraDB` in
    /// `Proofs/MCPHealth/State.lean` exactly.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Degraded => "degraded",
            Self::Evicted => "evicted",
            Self::Reconnecting => "reconnecting",
        }
    }

    pub fn parse(value: &str) -> Result<Self, InvalidToolServiceHealthState> {
        match value {
            "healthy" => Ok(Self::Healthy),
            "degraded" => Ok(Self::Degraded),
            "evicted" => Ok(Self::Evicted),
            "reconnecting" => Ok(Self::Reconnecting),
            _ => Err(InvalidToolServiceHealthState {
                state: value.to_string(),
            }),
        }
    }

    pub fn parse_opt(value: Option<&str>) -> Option<Self> {
        value.and_then(|value| Self::parse(value).ok())
    }

    /// Collapse to the operator-facing three-state projection.
    /// `Healthy` -> `Healthy`, `Degraded` -> `Stale`,
    /// `Evicted` | `Reconnecting` -> `Unreachable`.
    pub const fn project(self) -> ToolServiceHealthProjection {
        match self {
            Self::Healthy => ToolServiceHealthProjection::Healthy,
            Self::Degraded => ToolServiceHealthProjection::Stale,
            Self::Evicted | Self::Reconnecting => ToolServiceHealthProjection::Unreachable,
        }
    }
}

impl ToolServiceHealthProjection {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Healthy => "healthy",
            Self::Stale => "stale",
            Self::Unreachable => "unreachable",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidToolServiceHealthState {
    state: String,
}

impl InvalidToolServiceHealthState {
    pub fn value(&self) -> &str {
        &self.state
    }
}

impl Display for InvalidToolServiceHealthState {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid tool service health state: {}", self.state)
    }
}

impl Error for InvalidToolServiceHealthState {}

impl TryFrom<&str> for ToolServiceHealthState {
    type Error = InvalidToolServiceHealthState;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}

impl Display for ToolServiceHealthState {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

impl Display for ToolServiceHealthProjection {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn as_str_round_trips_for_every_state() {
        for state in ToolServiceHealthState::ALL {
            assert_eq!(ToolServiceHealthState::parse(state.as_str()), Ok(state));
        }
    }

    #[test]
    fn projection_table_matches_health_checker() {
        use ToolServiceHealthProjection as P;
        use ToolServiceHealthState::*;
        assert_eq!(Healthy.project(), P::Healthy);
        assert_eq!(Degraded.project(), P::Stale);
        assert_eq!(Evicted.project(), P::Unreachable);
        assert_eq!(Reconnecting.project(), P::Unreachable);
    }

    #[test]
    fn projection_words_are_not_valid_states() {
        assert!(ToolServiceHealthState::parse("stale").is_err());
        assert!(ToolServiceHealthState::parse("unreachable").is_err());
    }

    #[test]
    fn parse_opt_rejects_missing_and_unknown() {
        assert_eq!(ToolServiceHealthState::parse_opt(None), None);
        assert_eq!(ToolServiceHealthState::parse_opt(Some("bogus")), None);
        assert_eq!(
            ToolServiceHealthState::parse_opt(Some("healthy")),
            Some(ToolServiceHealthState::Healthy)
        );
    }
}
