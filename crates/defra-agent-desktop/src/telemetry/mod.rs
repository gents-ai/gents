mod classify;
mod layer;
mod store;

#[cfg(test)]
mod tests;

use tracing::Level;

pub use self::classify::classify_log_entry;
pub use self::layer::DesktopLogLayer;
pub use self::store::{global_log_layer, global_log_store, DesktopLogSnapshot, DesktopLogStore};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopLogCategory {
    Replication,
    Peering,
    Turns,
    Writes,
    Warnings,
}

impl DesktopLogCategory {
    pub const ALL: [Self; 5] = [
        Self::Replication,
        Self::Peering,
        Self::Turns,
        Self::Writes,
        Self::Warnings,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::Replication => "Replication",
            Self::Peering => "Peering",
            Self::Turns => "Turns",
            Self::Writes => "Writes",
            Self::Warnings => "Warnings",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopLogField {
    pub name: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopLogEntry {
    pub id: u64,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub level: Level,
    pub target: String,
    pub category: DesktopLogCategory,
    pub message: String,
    pub fields: Vec<DesktopLogField>,
}
