use std::fmt::Write as _;

use serde::Deserialize;

/// A document access policy an Agent writes for application collections.
/// Lean `Identity.ApplicationPolicy` decides which definitions are admitted
/// (`refusal`) and the policy text registered for one (`yaml`).
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApplicationPolicy {
    pub name: String,
    pub resources: Vec<PolicyResource>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyResource {
    pub name: String,
    #[serde(default)]
    pub relations: Vec<PolicyRelation>,
    #[serde(default)]
    pub permissions: Vec<PolicyPermission>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyRelation {
    pub name: String,
    #[serde(default)]
    pub types: Vec<String>,
    #[serde(default)]
    pub manages: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyPermission {
    pub name: String,
    #[serde(default)]
    pub relations: Vec<String>,
    #[serde(default)]
    pub parents: Vec<PolicyParent>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PolicyParent {
    pub relation: String,
    pub permission: String,
}

/// The first rule a definition breaks (Lean `ApplicationPolicy.Refusal`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum PolicyRefusal {
    #[error("the name must be 1-64 ASCII letters, digits, spaces, hyphens or underscores, start with a letter and not end with a space")]
    Name,
    #[error("names beginning \"Gents\" belong to the runtime's own policies; choose another name")]
    ReservedName,
    #[error("a policy needs at least one resource")]
    NoResources,
    #[error("resource, relation and permission names are lowercase identifiers: a-z, 0-9 and _, starting with a letter, at most 64, not true, false or null")]
    Identifier,
    #[error("owner is implicit: a document's creator owns it; declare no relation or permission named owner")]
    Owner,
    #[error(
        "resource names, and relation and permission names within one resource, must all differ"
    )]
    Duplicate,
    #[error("every resource declares the permissions read, update and delete")]
    DocumentPermissions,
    #[error("every relation declares types")]
    Untyped,
    #[error("a relation type is actor, a resource of this policy, or RESOURCE->RELATION for a relation of that resource")]
    RelationType,
    #[error("manages lists relations of the same resource")]
    Managed,
    #[error("a permission's relations are relations of the same resource")]
    PermissionRelation,
    #[error("a parent names a relation of the same resource whose types are resources of this policy, and a relation or permission each of those declares")]
    PermissionParent,
}

impl PolicyRefusal {
    /// The rule's wire name (Lean `Refusal.wire`).
    pub fn kind(self) -> &'static str {
        match self {
            Self::Name => "name",
            Self::ReservedName => "reserved-name",
            Self::NoResources => "no-resources",
            Self::Identifier => "identifier",
            Self::Owner => "owner",
            Self::Duplicate => "duplicate",
            Self::DocumentPermissions => "document-permissions",
            Self::Untyped => "untyped",
            Self::RelationType => "relation-type",
            Self::Managed => "managed",
            Self::PermissionRelation => "permission-relation",
            Self::PermissionParent => "permission-parent",
        }
    }
}

const DOCUMENT_PERMISSIONS: [&str; 3] = ["read", "update", "delete"];

fn valid_name(name: &str) -> bool {
    name.chars().count() <= 64
        && name.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ' ' | '-' | '_'))
        && !name.ends_with(' ')
}

fn identifier(word: &str) -> bool {
    word.chars().count() <= 64
        && word.chars().next().is_some_and(|c| c.is_ascii_lowercase())
        && word
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && !matches!(word, "true" | "false" | "null")
}

fn distinct<'a>(words: impl Iterator<Item = &'a str>) -> bool {
    let mut words: Vec<_> = words.collect();
    let count = words.len();
    words.sort_unstable();
    words.dedup();
    words.len() == count
}

impl PolicyResource {
    fn relation_names(&self) -> impl Iterator<Item = &str> {
        self.relations.iter().map(|relation| relation.name.as_str())
    }

    fn names(&self) -> impl Iterator<Item = &str> {
        self.relation_names().chain(
            self.permissions
                .iter()
                .map(|permission| permission.name.as_str()),
        )
    }

    fn declares(&self, name: &str) -> bool {
        self.names().any(|declared| declared == name)
    }
}

impl ApplicationPolicy {
    fn resource(&self, name: &str) -> Option<&PolicyResource> {
        self.resources.iter().find(|resource| resource.name == name)
    }

    fn type_inside(&self, kind: &str) -> bool {
        kind == "actor"
            || self.resources.iter().any(|resource| {
                kind == resource.name
                    || resource
                        .relation_names()
                        .any(|name| kind == format!("{}->{name}", resource.name))
            })
    }

    fn parent_inside(&self, resource: &PolicyResource, parent: &PolicyParent) -> bool {
        resource
            .relations
            .iter()
            .find(|relation| relation.name == parent.relation)
            .is_some_and(|relation| {
                !relation.types.is_empty()
                    && relation.types.iter().all(|kind| {
                        self.resource(kind)
                            .is_some_and(|target| target.declares(&parent.permission))
                    })
            })
    }

    /// Lean `ApplicationPolicy.refusal`: the rules in order, refusing with
    /// the first one broken.
    pub fn admit(&self) -> Result<(), PolicyRefusal> {
        let all = &self.resources;
        let relations = || {
            all.iter()
                .flat_map(|r| r.relations.iter().map(move |rel| (r, rel)))
        };
        let permissions = || {
            all.iter()
                .flat_map(|r| r.permissions.iter().map(move |p| (r, p)))
        };
        let checks = [
            (PolicyRefusal::Name, valid_name(&self.name)),
            (PolicyRefusal::ReservedName, !self.name.starts_with("Gents")),
            (PolicyRefusal::NoResources, !all.is_empty()),
            (
                PolicyRefusal::Identifier,
                all.iter()
                    .all(|r| identifier(&r.name) && r.names().all(identifier)),
            ),
            (
                PolicyRefusal::Owner,
                all.iter().all(|r| !r.declares("owner")),
            ),
            (
                PolicyRefusal::Duplicate,
                distinct(all.iter().map(|r| r.name.as_str()))
                    && all.iter().all(|r| distinct(r.names())),
            ),
            (
                PolicyRefusal::DocumentPermissions,
                all.iter().all(|r| {
                    DOCUMENT_PERMISSIONS
                        .iter()
                        .all(|p| r.permissions.iter().any(|declared| declared.name == *p))
                }),
            ),
            (
                PolicyRefusal::Untyped,
                relations().all(|(_, rel)| !rel.types.is_empty()),
            ),
            (
                PolicyRefusal::RelationType,
                relations().all(|(_, rel)| rel.types.iter().all(|kind| self.type_inside(kind))),
            ),
            (
                PolicyRefusal::Managed,
                relations().all(|(r, rel)| {
                    rel.manages
                        .iter()
                        .all(|managed| r.relation_names().any(|name| name == managed))
                }),
            ),
            (
                PolicyRefusal::PermissionRelation,
                permissions().all(|(r, p)| {
                    p.relations
                        .iter()
                        .all(|named| r.relation_names().any(|name| name == named))
                }),
            ),
            (
                PolicyRefusal::PermissionParent,
                permissions()
                    .all(|(r, p)| p.parents.iter().all(|parent| self.parent_inside(r, parent))),
            ),
        ];
        match checks.into_iter().find(|(_, passed)| !passed) {
            Some((refusal, _)) => Err(refusal),
            None => Ok(()),
        }
    }

    /// Lean `ApplicationPolicy.yaml`: the policy text DefraDB registers.
    pub fn yaml(&self) -> String {
        let mut out = format!("name: \"{}\"\nresources:\n", self.name);
        for resource in &self.resources {
            let _ = writeln!(out, "  - name: {}", resource.name);
            out.push_str(if resource.relations.is_empty() {
                "    relations: []\n"
            } else {
                "    relations:\n"
            });
            for relation in &resource.relations {
                let _ = writeln!(out, "      - name: {}", relation.name);
                if !relation.types.is_empty() {
                    let _ = writeln!(out, "        types: [{}]", relation.types.join(", "));
                }
                if !relation.manages.is_empty() {
                    let _ = writeln!(out, "        manages: [{}]", relation.manages.join(", "));
                }
            }
            out.push_str("    permissions:\n");
            for permission in &resource.permissions {
                let _ = writeln!(out, "      - name: {}", permission.name);
                let expression = permission
                    .relations
                    .iter()
                    .cloned()
                    .chain(
                        permission
                            .parents
                            .iter()
                            .map(|parent| format!("{}->{}", parent.relation, parent.permission)),
                    )
                    .collect::<Vec<_>>()
                    .join(" + ");
                if !expression.is_empty() {
                    let _ = writeln!(out, "        expr: {expression}");
                }
            }
        }
        out
    }
}

#[cfg(test)]
#[path = "application_policy_tests.rs"]
mod tests;
