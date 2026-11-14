use crate::config_client::{ConfigAccess, ConfigClientError, NodePermission};

/// The named node-permission sets Gents issues. Lean `Identity.GrantSets`
/// owns their contents and recipients; native NAC decides every change as
/// the access's actor under its held-bounded rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
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
    /// plus read-only `NacStatus` to explain other identities' node grants and
    /// read-only `CollectionGet`, which native relationship changes outside a
    /// transaction require. It holds no schema change permission.
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
                NodePermission::DacRelationDelete,
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
                NodePermission::CollectionGet,
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

    /// The sets `recipient` receives (Lean `Identity.GrantSets.directSets` and
    /// `projectedSets`). Every admission that grants a recipient its sets asks
    /// here: init and trial installation for the init User and the default
    /// Agent, enrollment for an enrolled User, and the tool permission
    /// projection for every live managed Agent.
    pub fn for_recipient(recipient: Recipient) -> RecipientGrants {
        match recipient {
            Recipient::InitUser => RecipientGrants {
                direct: &[Self::HouseholdOwner],
                projected: &[],
            },
            Recipient::EnrolledUser => RecipientGrants {
                direct: &[Self::OperatorUser],
                projected: &[],
            },
            Recipient::DefaultAgent => RecipientGrants {
                direct: &[Self::PolicyDelegate],
                projected: &[Self::AgentBaseline],
            },
            Recipient::Agent => RecipientGrants {
                direct: &[],
                projected: &[Self::AgentBaseline],
            },
        }
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
                    source: Box::new(source),
                },
                Some(rollback) => GrantSetError::Rollback {
                    set: self.name(),
                    relation: relation.name(),
                    source: Box::new(source),
                    rollback: Box::new(rollback),
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
                    source: Box::new(source),
                })?;
        }
        Ok(())
    }
}

/// Who receives grant sets (Lean `Identity.GrantSets.Recipient`). The init
/// User creates the node (`gents init`'s client, or an evaluation trial's
/// evaluator); the default Agent is the Agent init publishes or the subject
/// Agent a trial selects; an enrolled User is admitted by verified enrollment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Recipient {
    InitUser,
    EnrolledUser,
    DefaultAgent,
    Agent,
}

impl Recipient {
    pub const ALL: [Self; 4] = [
        Self::InitUser,
        Self::EnrolledUser,
        Self::DefaultAgent,
        Self::Agent,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Self::InitUser => "init-user",
            Self::EnrolledUser => "enrolled-user",
            Self::DefaultAgent => "default-agent",
            Self::Agent => "agent",
        }
    }
}

/// What [`GrantSet::for_recipient`] answers. `direct` sets are granted once by
/// the admission that creates the recipient and persist until an explicit
/// revocation; `projected` sets follow the live Agent document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RecipientGrants {
    pub direct: &'static [GrantSet],
    pub projected: &'static [GrantSet],
}

impl RecipientGrants {
    /// Grant every direct set to `subject` as `access`'s actor, in order. Each
    /// set rolls back its own relations on failure ([`GrantSet::grant`]); sets
    /// granted before the failing one stay granted.
    pub async fn grant_direct(
        self,
        access: &ConfigAccess,
        subject: &identity::Did,
    ) -> Result<(), GrantSetError> {
        for set in self.direct {
            set.grant(access, subject).await?;
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
        source: Box<ConfigClientError>,
    },
    #[error(
        "granting {set} relation {relation} failed and revoking its earlier grants also failed \
         ({rollback}); inspect the subject's node permissions before retrying"
    )]
    Rollback {
        set: &'static str,
        relation: &'static str,
        #[source]
        source: Box<ConfigClientError>,
        rollback: Box<ConfigClientError>,
    },
    #[error("revoking {set} relation {relation} failed")]
    Revoke {
        set: &'static str,
        relation: &'static str,
        #[source]
        source: Box<ConfigClientError>,
    },
}

#[cfg(any(test, feature = "test-fixtures"))]
#[path = "grant_sets_test_support.rs"]
pub mod test_support;

#[cfg(test)]
#[path = "grant_sets_tests.rs"]
mod tests;
