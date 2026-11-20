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
    /// The relations of the same resource whose declared types admit an
    /// actor (Lean `Identity.Household.typeAdmitted`); empty for a relation.
    actor_relations: Vec<String>,
}

impl HouseholdName {
    /// The relations of the same document a permission's expression names.
    fn expression_relations(&self) -> impl Iterator<Item = &str> {
        self.expr
            .split('+')
            .map(str::trim)
            .filter(|term| !term.is_empty() && !term.contains("->"))
    }

    /// The relations of the same document that give this permission and can
    /// be granted to an actor. A relation typed only by a userset (the Node's
    /// `agent_manager`) is not one: a grant of it to an actor is refused.
    pub(super) fn granting_relations(&self) -> Vec<&str> {
        self.expression_relations()
            .filter(|relation| self.actor_relations.iter().any(|held| held == relation))
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
            let actor_relations = resource
                .relations
                .iter()
                .filter(|relation| {
                    relation.types.is_empty() || relation.types.iter().any(|ty| ty == "actor")
                })
                .map(|relation| relation.name.clone())
                .collect::<Vec<_>>();
            let relations = resource.relations.iter().map(|relation| HouseholdName {
                resource: resource.name.clone(),
                name: relation.name.clone(),
                is_permission: false,
                expr: String::new(),
                actor_relations: Vec::new(),
            });
            let permissions = resource
                .permissions
                .iter()
                .map(move |permission| HouseholdName {
                    resource: resource.name.clone(),
                    name: permission.name.clone(),
                    is_permission: true,
                    expr: permission.expr.clone(),
                    actor_relations: actor_relations.clone(),
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
                && name.expression_relations().any(|named| named == relation)
        })
        .map(|name| name.name)
        .collect())
}
