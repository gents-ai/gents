//! `ToolServiceHealthState`: the single Rust owner of MCP health vocabulary
//! and its projection to the collapsed operator-facing status.
//!
//! Source of truth: the Lean `HealthState` model in
//! `crates/gents/proofs/Proofs/MCPHealth/State.lean` — `as_str` here must
//! stay byte-identical to that model's `toDefraDB`.

use std::error::Error;
use std::fmt::{Display, Formatter};

/// The full internal MCP service health state, persisted verbatim to the
/// `ToolServiceHealthState` DefraDB collection's `status` column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ToolServiceHealthState {
    Healthy,
    Degraded,
    Evicted,
    Reconnecting,
}

/// The collapsed three-state projection consumers classify against. Owned
/// entirely by [`ToolServiceHealthState::project`].
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

    /// String form persisted to the `status` column. Mirrors
    /// `HealthState.toDefraDB` in `Proofs/MCPHealth/State.lean` exactly.
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