use crate::store::config_client::{ConfigAccess, ConfigClientError, NodePermission};

/// The named node-permission sets Gents issues. Lean `Identity.GrantSets`
/// owns their contents and recipients; native NAC decides every change as
/// the access's actor under its held-bounded rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GrantSet {
    /// Projected from each live managed Agent document, never granted directly
    /// in production.
    AgentBaseline,
    /// Granted to a User admitted by verified enrollment.
    OperatorUser,
    /// The native `admin` relation, granted only to the User `gents init`
    /// creates. It includes DAC bypass.
    HouseholdOwner,
    /// Relationship-management authority bounded by what the holder holds,
    /// plus read-only `NacStatus` to explain other identities' node grants.
    PolicyDelegate,
}

impl GrantSet {
    pub const ALL: [Self; 4] = [
        Self::AgentBaseline,
        Self::OperatorUser,
        Self::HouseholdOwner,
        Self::PolicyDelegate,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::AgentBaseline => "agent-baseline",
            Self::OperatorUser => "operator-user",
            Self::HouseholdOwner => "household-owner",
            Self::PolicyDelegate => "policy-delegate",
        }
    }

    pub fn permissions(self) -> &'static [NodePermission] {
        match self {
            Self::AgentBaseline => &[
                NodePermission::DocumentRead,
                NodePermission::DocumentUpdate,
                NodePermission::DacRelationAdd,
            ],
            Self::OperatorUser => &[
                NodePermission::CollectionGet,
                NodePermission::DocumentRead,
                NodePermission::DocumentUpdate,
                NodePermission::DocumentDelete,
                NodePermission::DacRelationAdd,
                NodePermission::DacRelationDelete,
                NodePermission::NacRelationAdd,
                NodePermission::NacRelationDelete,
            ],
            Self::HouseholdOwner => &[],
            Self::PolicyDelegate => &[
                NodePermission::NacRelationAdd,
                NodePermission::NacRelationDelete,
                NodePermission::DacRelationAdd,
                NodePermission::DacRelationDelete,
                NodePermission::NacStatus,
            ],
        }
    }

    pub fn native_admin(self) -> bool {
        matches!(self, Self::HouseholdOwner)
    }

    fn relations(self) -> impl Iterator<Item = Relation> {
        self.native_admin()
            .then_some(Relation::Admin)
            .into_iter()
            .chain(self.permissions().iter().copied().map(Relation::Permission))
    }

    /// Grant every relation of this set to `subject` as `access`'s actor. A
    /// refused or failed grant revokes the ones this call already issued,
    /// including the failed one, whose outcome may be unknown. Grants carry no
    /// provenance, so a rollback also removes an identical grant the subject
    /// held before this call.
    pub async fn grant(
        self,
        access: &ConfigAccess,
        subject: &identity::Did,
    ) -> Result<(), GrantSetError> {
        let relations: Vec<_> = self.relations().collect();
        for (index, relation) in relations.iter().enumerate() {
            let Err(source) = relation.change(access, subject, false).await else {
                continue;
            };
            let mut rollback = None;
            for issued in relations[..=index].iter().rev() {
                if let Err(error) = issued.change(access, subject, true).await {
                    if issued != relation {
                        rollback.get_or_insert(error);
                    }
                }
            }
            return Err(match rollback {
                None => GrantSetError::Grant {
                    set: self.name(),
                    relation: relation.name(),
                    source,
                },
                Some(rollback) => GrantSetError::Rollback {
                    set: self.name(),
                    relation: relation.name(),
                    source,
                    rollback,
                },
            });
        }
        Ok(())
    }

    /// Revoke every relation of this set from `subject` as `access`'s actor.
    /// Absent relations are already revoked.
    pub async fn revoke(
        self,
        access: &ConfigAccess,
        subject: &identity::Did,
    ) -> Result<(), GrantSetError> {
        for relation in self.relations() {
            relation
                .change(access, subject, true)
                .await
                .map_err(|source| GrantSetError::Revoke {
                    set: self.name(),
                    relation: relation.name(),
                    source,
                })?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Relation {
    Admin,
    Permission(NodePermission),
}

impl Relation {
    fn name(self) -> &'static str {
        match self {
            Self::Admin => "admin",
            Self::Permission(permission) => permission.as_str(),
        }
    }

    async fn change(
        self,
        access: &ConfigAccess,
        subject: &identity::Did,
        delete: bool,
    ) -> Result<(), ConfigClientError> {
        match (self, delete) {
            (Self::Admin, false) => access.grant_node_admin(subject).await,
            (Self::Admin, true) => access.revoke_node_admin(subject).await,
            (Self::Permission(permission), false) => {
                access.grant_node_permission(subject, permission).await
            }
            (Self::Permission(permission), true) => {
                access.revoke_node_permission(subject, permission).await
            }
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum GrantSetError {
    #[error("granting {set} relation {relation} failed; the grants this call issued were revoked")]
    Grant {
        set: &'static str,
        relation: &'static str,
        #[source]
        source: ConfigClientError,
    },
    #[error(
        "granting {set} relation {relation} failed and revoking its earlier grants also failed \
         ({rollback}); inspect the subject's node permissions before retrying"
    )]
    Rollback {
        set: &'static str,
        relation: &'static str,
        #[source]
        source: ConfigClientError,
        rollback: ConfigClientError,
    },
    #[error("revoking {set} relation {relation} failed")]
    Revoke {
        set: &'static str,
        relation: &'static str,
        #[source]
        source: ConfigClientError,
    },
}

#[cfg(test)]
#[path = "grant_sets_tests.rs"]
mod tests;
