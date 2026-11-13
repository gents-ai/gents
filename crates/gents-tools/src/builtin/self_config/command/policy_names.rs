use anyhow::{anyhow, Result};

/// Lean `SelfConfig.PolicyName`: a relation or a permission of one resource
/// of the Household policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct HouseholdName {
    pub(super) resource: String,
    pub(super) name: String,
    pub(super) is_permission: bool,
    /// A permission's expression in the policy; empty for a relation, and for
    /// a permission only the document's owner holds.
    pub(super) expr: String,
}

impl HouseholdName {
    /// The relations of the same document a permission's expression names.
    pub(super) fn granting_relations(&self) -> Vec<&str> {
        self.expr
            .split('+')
            .map(str::trim)
            .filter(|term| !term.is_empty() && !term.contains("->"))
            .collect()
    }

    pub(super) fn kind(&self) -> &'static str {
        if self.is_permission {
            "permission"
        } else {
            "relation"
        }
    }
}

/// Lean `SelfConfig.policyNames`, read from the policy document the Lean
/// model generates, in the model's order.
pub(super) fn household_names() -> Result<Vec<HouseholdName>> {
    let policy =
        acp::policy_yaml::parse_policy_yaml(gents_protocol::schemas::AGENT_OWNERSHIP_POLICY)
            .map_err(|error| anyhow!("the Household policy does not parse: {error}"))?;
    Ok(policy
        .resources
        .iter()
        .flat_map(|resource| {
            let relations = resource.relations.iter().map(|relation| HouseholdName {
                resource: resource.name.clone(),
                name: relation.name.clone(),
                is_permission: false,
                expr: String::new(),
            });
            let permissions = resource.permissions.iter().map(|permission| HouseholdName {
                resource: resource.name.clone(),
                name: permission.name.clone(),
                is_permission: true,
                expr: permission.expr.clone(),
            });
            relations.chain(permissions)
        })
        .collect())
}

/// Lean `SelfConfig.resolvePolicyName`: `resource#name` is that resource's
/// relation or permission, a bare name every resource's of that name.
pub(super) fn resolve(word: &str) -> Result<Vec<HouseholdName>> {
    let names = household_names()?;
    Ok(match word.split('#').collect::<Vec<_>>().as_slice() {
        [name] => names
            .into_iter()
            .filter(|candidate| candidate.name == *name)
            .collect(),
        [resource, name] => names
            .into_iter()
            .filter(|candidate| candidate.resource == *resource && candidate.name == *name)
            .collect(),
        _ => Vec::new(),
    })
}

/// The permissions of `resource` whose expression names `relation`.
pub(super) fn permissions_given_by(resource: &str, relation: &str) -> Result<Vec<String>> {
    Ok(household_names()?
        .into_iter()
        .filter(|name| {
            name.is_permission
                && name.resource == resource
                && name.granting_relations().contains(&relation)
        })
        .map(|name| name.name)
        .collect())
}
