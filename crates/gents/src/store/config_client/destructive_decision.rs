//! Destructive admin decisions: `Identity.Configuration.destructiveAdminDecision`.

/// Where a destructive admin decision learns which documents exist
/// (`Identity.Configuration.InventoryRead`). Every Gents read is an ordinary
/// query filtered by document ACP for its own actor, and native DefraDB does
/// not apply DacBypass to ordinary reads, so no read is a complete inventory
/// for any actor, the native admin included. No complete-inventory read exists
/// yet; until one does, every destructive decision is refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InventoryRead {
    AcpFiltered,
}

/// The native observations a destructive admin decision rests on.
/// `admin` is what native NAC reports for the actor, which is every actor
/// while NAC is not enabled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DestructiveAuthority {
    pub nac_enabled: bool,
    pub admin: bool,
    pub inventory: InventoryRead,
}

/// Why a destructive admin decision was refused, in the modeled order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DestructiveRefusal {
    #[error(
        "node ACP is not enabled, so native NAC reports every actor as admin and no actor may \
         make a destructive admin decision; enable node ACP first"
    )]
    NacDisabled,
    #[error("only the native admin (the client User key) may make a destructive admin decision")]
    NotAdmin,
    #[error(
        "no complete inventory is available: every read is filtered by document ACP for its \
         actor, the native admin included, so an unreadable document is not known to be absent; \
         this stays refused until a complete-inventory read exists"
    )]
    IncompleteInventory,
}

impl DestructiveRefusal {
    /// The modeled name (`DestructiveRefusal.name`).
    pub fn name(self) -> &'static str {
        match self {
            Self::NacDisabled => "nac_disabled",
            Self::NotAdmin => "not_admin",
            Self::IncompleteInventory => "incomplete_inventory",
        }
    }
}

impl DestructiveAuthority {
    /// Whether the actor is the native admin for a destructive decision:
    /// with NAC disabled no actor is.
    pub fn native_admin(self) -> bool {
        self.nac_enabled && self.admin
    }

    /// `Identity.Configuration.destructiveAdminDecision`.
    pub fn decide(self) -> Result<(), DestructiveRefusal> {
        if !self.nac_enabled {
            return Err(DestructiveRefusal::NacDisabled);
        }
        if !self.admin {
            return Err(DestructiveRefusal::NotAdmin);
        }
        match self.inventory {
            InventoryRead::AcpFiltered => Err(DestructiveRefusal::IncompleteInventory),
        }
    }
}

#[cfg(test)]
mod tests;
