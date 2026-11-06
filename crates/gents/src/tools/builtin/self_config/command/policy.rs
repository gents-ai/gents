use super::*;

use crate::store::config_client::{ConfigAccess, ConfigClientError, NodePermission};
use crate::store::identity::GrantSet;
use acp::nac::{PermissionProvenance, PermissionVia};

const VERBS: [&str; 5] = ["get", "check", "explain", "grant", "revoke"];

#[derive(Debug)]
enum PolicyTarget {
    Node(NodePermission),
    Relation {
        collection: String,
        doc_id: String,
        relation: String,
    },
}

#[derive(Debug)]
struct PolicyCall {
    subject: ::identity::Did,
    is_self: bool,
    subject_given: bool,
    target: Option<PolicyTarget>,
}

fn guidance(message: impl Into<String>, next_call: Value) -> anyhow::Error {
    CommandGuidance {
        message: message.into(),
        next_call,
    }
    .into()
}

fn ensure_options(parsed: &ParsedArgs, verb: &str) -> Result<()> {
    let allowed: &[&str] = match verb {
        "get" => &["subject", "collection", "doc"],
        _ => &[
            "subject",
            "permission",
            "collection",
            "doc",
            "relation",
        ],
    };
    if let Some(name) = parsed.options.keys().find(|name| !allowed.contains(&name.as_str())) {
        return Err(guidance(
            format!("policy {verb} does not accept --{name}; accepted: {}", allowed.iter().map(|o| format!("--{o}")).collect::<Vec<_>>().join(", ")),
            json!({"argv": ["policy", "--help"]}),
        ));
    }
    anyhow::ensure!(
        parsed.positionals.is_empty() && parsed.switches.is_empty(),
        "policy {verb} takes named options only; see [\"help\",\"policy\"]"
    );
    Ok(())
}

/// A node permission (`--permission`) or one document relationship
/// (`--collection`, `--doc`, `--relation`); never both.
fn parse_target(parsed: &ParsedArgs) -> Result<Option<PolicyTarget>> {
    let permission = parsed.one("permission")?;
    let collection = parsed.one("collection")?;
    let doc_id = parsed.one("doc")?;
    let relation = parsed.one("relation")?;
    match (permission, collection, doc_id, relation) {
        (None, None, None, None) => Ok(None),
        (Some(name), None, None, None) => match NodePermission::parse(name) {
            Some(permission) => Ok(Some(PolicyTarget::Node(permission))),
            None => Err(guidance(
                format!("unknown node permission {name:?}; names are the ones policy get lists, e.g. read-document"),
                json!({"argv": ["policy", "get"]}),
            )),
        },
        (None, Some(collection), Some(doc_id), Some(relation)) => Ok(Some(PolicyTarget::Relation {
            collection: collection.to_owned(),
            doc_id: doc_id.to_owned(),
            relation: relation.to_owned(),
        })),
        _ => Err(guidance(
            "name one target: --permission PERMISSION for a node permission, or all of --collection, --doc and --relation for a document relationship",
            json!({"argv": ["policy", "--help"]}),
        )),
    }
}

fn via_words(via: &PermissionVia) -> String {
    match via {
        PermissionVia::Owner => "as node owner".to_owned(),
        PermissionVia::Admin => "as admin, which includes DAC bypass".to_owned(),
        PermissionVia::Direct => "direct grant".to_owned(),
        PermissionVia::Source { path } => format!("through {}", path.join(" > ")),
    }
}

fn target_words(target: &PolicyTarget) -> String {
    match target {
        PolicyTarget::Node(permission) => format!("node permission {}", permission.as_str()),
        PolicyTarget::Relation {
            collection,
            doc_id,
            relation,
        } => format!("relation {relation} on {collection} document {doc_id}"),
    }
}

fn target_json(target: Option<&PolicyTarget>) -> Value {
    match target {
        None => Value::Null,
        Some(PolicyTarget::Node(permission)) => {
            json!({"kind": "node_permission", "permission": permission.as_str()})
        }
        Some(PolicyTarget::Relation {
            collection,
            doc_id,
            relation,
        }) => json!({
            "kind": "document_relationship",
            "collection": collection,
            "doc_id": doc_id,
            "relation": relation,
        }),
    }
}

impl PolicyCall {
    fn who(&self) -> &'static str {
        if self.is_self {
            "you"
        } else {
            "the subject"
        }
    }
}

fn read_refusal(subject: &::identity::Did, error: ConfigClientError) -> anyhow::Error {
    guidance(
        format!(
            "cannot read the node permissions of {}: {error}. DefraDB shows them to the subject itself, to an admin, or to an Agent holding NacStatus. Your own are readable with policy get.",
            subject.as_str()
        ),
        json!({"argv": ["policy", "get"]}),
    )
}

/// Node permissions this Agent keeps against its own revoke while
/// `self_config_no_lockout` is set: its baseline and policy delegation (Lean
/// `Identity.GrantSets.lockoutProtected`).
fn lockout_protected(permission: NodePermission) -> bool {
    GrantSet::AgentBaseline.permissions().contains(&permission)
        || GrantSet::PolicyDelegate.permissions().contains(&permission)
}

/// Lean `Identity.GrantSets.selfChangeKeepsLockout`. Document relationships are
/// not modeled here and stay governed by native document authorization.
fn keeps_lockout(no_lockout: bool, is_self: bool, delete: bool, target: &PolicyTarget) -> bool {
    if !(no_lockout && is_self) {
        return true;
    }
    match target {
        PolicyTarget::Node(permission) => !(delete && lockout_protected(*permission)),
        PolicyTarget::Relation { .. } => true,
    }
}

/// Permissions the subject holds, with the paths that give each one.
fn permission_ways(provenance: &[PermissionProvenance], permission: NodePermission) -> Vec<String> {
    provenance
        .iter()
        .filter(|entry| entry.permission == permission)
        .map(|entry| via_words(&entry.via))
        .collect()
}

impl ConfigCommandTool {
    pub(super) async fn policy(&self, argv: &[String]) -> Result<String> {
        let verb = argv.first().map(String::as_str).unwrap_or_default();
        if !VERBS.contains(&verb) {
            return Err(guidance(
                format!("unknown policy verb {verb:?}; policy takes get, check, explain, grant or revoke"),
                json!({"argv": ["policy", "--help"]}),
            ));
        }
        self.ensure_resource("policy")?;
        let parsed = ParsedArgs::parse(&argv[1..])?;
        ensure_options(&parsed, verb)?;
        let call = self.policy_call(&parsed, verb).await?;
        let access = self.core.managed_access()?;
        match verb {
            "get" => self.policy_get(&access, &parsed, &call).await,
            "check" | "explain" => self.policy_answer(&access, &call, verb == "explain").await,
            _ => self.policy_change(&access, &call, verb == "grant").await,
        }
    }

    async fn policy_call(&self, parsed: &ParsedArgs, verb: &str) -> Result<PolicyCall> {
        let actor = self.core.identity()?;
        let target = if verb == "get" {
            None
        } else {
            parse_target(parsed)?
        };
        let subject_given = parsed.one("subject")?.is_some();
        let (subject, is_self) = match parsed.one("subject")? {
            None | Some("self") => (actor.clone(), true),
            Some(value) if value.starts_with("did:") => {
                let subject = ::identity::Did::new(value.to_owned())
                    .context("subject must be self, an Agent name, or a DID")?;
                let is_self = subject.as_str() == actor.as_str();
                (subject, is_self)
            }
            Some(name) => {
                let did = self.resolve_agent_did(name).await?;
                let is_self = did == actor.as_str();
                (::identity::Did::new(did)?, is_self)
            }
        };
        Ok(PolicyCall {
            subject,
            is_self,
            subject_given,
            target,
        })
    }

    async fn subject_provenance(
        &self,
        access: &ConfigAccess,
        call: &PolicyCall,
    ) -> Result<Vec<PermissionProvenance>> {
        access
            .explain_node_subject(&call.subject)
            .await
            .map_err(|error| read_refusal(&call.subject, error))
    }

    async fn policy_get(
        &self,
        access: &ConfigAccess,
        parsed: &ParsedArgs,
        call: &PolicyCall,
    ) -> Result<String> {
        let provenance = self.subject_provenance(access, call).await?;
        let mut permissions = BTreeMap::<String, Vec<String>>::new();
        for entry in &provenance {
            permissions
                .entry(entry.permission.as_str().to_owned())
                .or_default()
                .push(via_words(&entry.via));
        }
        let answer = if permissions.is_empty() {
            format!("{} hold no node permissions.", call.who())
        } else {
            format!(
                "{} hold {} node permissions: {}.",
                call.who(),
                permissions.len(),
                permissions.keys().cloned().collect::<Vec<_>>().join(", ")
            )
        };
        let next = if call.is_self {
            "policy check --permission NAME to test one; policy grant --subject AGENT --permission NAME to delegate a permission you hold."
        } else {
            "policy check --subject AGENT --permission NAME to test one permission."
        };
        let relations = match (parsed.one("collection")?, parsed.one("doc")?) {
            (Some(collection), Some(doc_id)) => Some(
                access
                    .list_document_relationships(collection, doc_id)
                    .await
                    .map_err(|error| {
                        guidance(
                            format!("cannot read the relationships of {collection} {doc_id}: {error}"),
                            json!({"argv": ["policy", "get"]}),
                        )
                    })?,
            ),
            (None, None) => None,
            _ => {
                return Err(guidance(
                    "policy get lists a document's relationships only with both --collection and --doc",
                    json!({"argv": ["policy", "--help"]}),
                ))
            }
        };
        let relation_rows = relations.as_ref().map(|rows| {
            rows.iter()
                .filter(|row| {
                    !call.subject_given
                        || row.subject == call.subject.as_str()
                        || row.subject == "*"
                })
                .map(|row| json!({"relation": row.relation, "subject": row.subject}))
                .collect::<Vec<_>>()
        });
        ordered! {
            "answer": answer,
            "how_to_proceed": next,
            "node_permissions": provenance.iter().map(|entry| json!({
                "permission": entry.permission.as_str(),
                "via": via_words(&entry.via),
            })).collect::<Vec<_>>(),
            "document_relationships": relation_rows,
            "metadata": json!({
                "subject_did": call.subject.as_str(),
                "provenance": provenance,
            }),
        }
        .pretty()
    }

    async fn policy_answer(
        &self,
        access: &ConfigAccess,
        call: &PolicyCall,
        explain: bool,
    ) -> Result<String> {
        let target = call.target.as_ref().ok_or_else(|| {
            guidance(
                "policy check and explain need a target: --permission NAME, or --collection, --doc and --relation",
                json!({"argv": ["policy", "--help"]}),
            )
        })?;
        let (allowed, ways) = match target {
            PolicyTarget::Node(permission) => {
                let provenance = self.subject_provenance(access, call).await?;
                let ways = permission_ways(&provenance, *permission);
                (!ways.is_empty(), ways)
            }
            PolicyTarget::Relation {
                collection,
                doc_id,
                relation,
            } => {
                let rows = access
                    .list_document_relationships(collection, doc_id)
                    .await
                    .map_err(|error| {
                        guidance(
                            format!("cannot read the relationships of {collection} {doc_id}: {error}"),
                            json!({"argv": ["policy", "get"]}),
                        )
                    })?;
                let subject = call.subject.as_str();
                let ways = rows
                    .iter()
                    .filter(|row| row.relation == *relation && (row.subject == subject || row.subject == "*"))
                    .map(|row| {
                        if row.subject == "*" {
                            "granted to everyone".to_owned()
                        } else {
                            "direct relationship".to_owned()
                        }
                    })
                    .collect::<Vec<_>>();
                (!ways.is_empty(), ways)
            }
        };
        let relation = matches!(target, PolicyTarget::Relation { .. });
        let answer = match (allowed, relation) {
            (true, false) => format!(
                "yes: {} hold {} ({}).",
                call.who(),
                target_words(target),
                ways.join("; ")
            ),
            (false, false) => format!("no: {} do not hold {}.", call.who(), target_words(target)),
            (true, true) => format!(
                "yes: {} hold a direct {} ({}).",
                call.who(),
                target_words(target),
                ways.join("; ")
            ),
            (false, true) => format!(
                "no direct {}: {} have no such relationship.",
                target_words(target),
                call.who()
            ),
        };
        let mut how = match (allowed, relation) {
            (true, true) => "policy revoke with the same target removes this direct relationship; owner, implied-relation and admin paths are not evaluated here.".to_owned(),
            (false, true) => "policy grant with the same target adds it only if this agent manages the relation on that document; owner, implied-relation and admin paths are not evaluated here.".to_owned(),
            (true, false) => "policy revoke with the same target removes a direct grant; paths through Tools or admin remain.".to_owned(),
            (false, false) if explain => self.missing_grant_path(access, call, target).await?,
            (false, false) => "policy explain with the same target names the path or the grant that would allow it.".to_owned(),
        };
        if !allowed && explain && matches!(target, PolicyTarget::Node(permission) if permission.is_admin_class()) {
            how = format!("{how} Admin-class permissions are never delegated; only a native admin can change them.");
        }
        ordered! {
            "answer": answer,
            "how_to_proceed": how,
            "metadata": json!({
                "subject_did": call.subject.as_str(),
                "target": target_json(call.target.as_ref()),
            }),
        }
        .pretty()
    }

    /// Names the grant that would allow a missing permission. This Agent can
    /// delegate a node permission only by holding it and `add-nac-relation`,
    /// and never an admin-class permission.
    async fn missing_grant_path(
        &self,
        access: &ConfigAccess,
        call: &PolicyCall,
        target: &PolicyTarget,
    ) -> Result<String> {
        let PolicyTarget::Node(permission) = target else {
            return Ok("Grant the relationship with policy grant; DefraDB allows it only if this Agent manages the relation on that document.".to_owned());
        };
        if permission.is_admin_class() {
            return Ok(format!(
                "{} is admin-class: a non-admin Agent never delegates it, and only a native admin can change it.",
                permission.as_str()
            ));
        }
        let actor = self.core.identity()?;
        let own = access
            .explain_node_subject(&actor)
            .await
            .map_err(|error| read_refusal(&actor, error))?;
        let holds_permission = !permission_ways(&own, *permission).is_empty();
        let holds_operation = !permission_ways(&own, NodePermission::NacRelationAdd).is_empty();
        Ok(match (holds_permission, holds_operation) {
            (true, true) => format!(
                "You hold {} and add-nac-relation, so this grant would be allowed: policy grant --subject {} --permission {}.",
                permission.as_str(),
                call.subject.as_str(),
                permission.as_str()
            ),
            (false, _) => format!(
                "You do not hold {}, so you cannot delegate it. Ask a User with native admin to grant it, or name a permission you hold in policy grant.",
                permission.as_str()
            ),
            (true, false) => "You hold the permission but not add-nac-relation, so you cannot delegate any node permission. Ask a User with native admin to grant add-nac-relation.".to_owned(),
        })
    }

    async fn policy_change(
        &self,
        access: &ConfigAccess,
        call: &PolicyCall,
        grant: bool,
    ) -> Result<String> {
        let verb = if grant { "grant" } else { "revoke" };
        let target = call.target.as_ref().ok_or_else(|| {
            guidance(
                format!("policy {verb} needs a target: --permission NAME, or --collection, --doc, --relation"),
                json!({"argv": ["policy", "--help"]}),
            )
        })?;
        let actor = self.core.identity()?;
        if !keeps_lockout(self.no_lockout, call.is_self, !grant, target) {
            return Err(guidance(
                format!(
                    "self_config_no_lockout keeps {} on this agent, the permissions its own configuration needs; revoke it from another agent, or ask a User with native admin to change this agent.",
                    target_words(target)
                ),
                json!({"argv": ["policy", "get"]}),
            ));
        }
        self.execution.enter_mutation();
        let changed = match target {
            PolicyTarget::Node(permission) if grant => {
                access.grant_node_permission(&call.subject, *permission).await
            }
            PolicyTarget::Node(permission) => {
                access.revoke_node_permission(&call.subject, *permission).await
            }
            PolicyTarget::Relation {
                collection,
                doc_id,
                relation,
            } if grant => {
                access
                    .add_actor_relationship(collection, doc_id, relation, &call.subject)
                    .await
            }
            PolicyTarget::Relation {
                collection,
                doc_id,
                relation,
            } => {
                access
                    .delete_actor_relationship(collection, doc_id, relation, &call.subject)
                    .await
            }
        };
        if let Err(error) = changed {
            let own = match access.explain_node_subject(&actor).await {
                Ok(provenance) => {
                    let mut names = provenance
                        .iter()
                        .map(|entry| entry.permission.as_str().to_owned())
                        .collect::<Vec<_>>();
                    names.sort();
                    names.dedup();
                    if names.is_empty() {
                        "no node permissions".to_owned()
                    } else {
                        names.join(", ")
                    }
                }
                Err(_) => "not readable".to_owned(),
            };
            return Err(guidance(
                format!(
                    "policy {verb} refused for {}: {error}. Delegation accepts only permissions this Agent holds, never admin-class permissions or a wildcard subject. You hold: {own}.",
                    target_words(target)
                ),
                json!({"argv": ["policy", "get"]}),
            ));
        }
        tracing::info!(
            target: "gents::tools::builtin::self_config::policy",
            acting_agent = %actor.as_str(),
            subject = %call.subject.as_str(),
            change = %target_words(target),
            verb,
            "policy change committed"
        );
        let (still, through) = match target {
            PolicyTarget::Node(permission) => match self.subject_provenance(access, call).await {
                Ok(provenance) => {
                    let ways = permission_ways(&provenance, *permission);
                    (!ways.is_empty(), ways.join("; "))
                }
                Err(error) => (false, format!("not readable: {error:#}")),
            },
            PolicyTarget::Relation {
                collection,
                doc_id,
                relation,
            } => match access.list_document_relationships(collection, doc_id).await {
                Ok(rows) => {
                    let subject = call.subject.as_str();
                    let present = rows.iter().any(|row| {
                        row.relation == *relation && (row.subject == subject || row.subject == "*")
                    });
                    (present, String::new())
                }
                Err(error) => (false, format!("not readable: {error}")),
            },
        };
        let answer = format!(
            "{} {} {}{}.",
            if grant { "Granted" } else { "Revoked" },
            target_words(target),
            if grant { "to" } else { "from" },
            if call.is_self {
                " you".to_owned()
            } else {
                format!(" {}", call.subject.as_str())
            }
        );
        let relation = matches!(target, PolicyTarget::Relation { .. });
        let access_now = match (still, through.is_empty(), relation) {
            (true, true, false) => "The subject still holds it.".to_owned(),
            (true, false, false) => format!("The subject still holds it through: {through}."),
            (false, _, false) if through.starts_with("not readable") => format!("The subject's access after this change is {through}."),
            (false, _, false) => "The subject no longer holds it through any path.".to_owned(),
            (true, _, true) => "A direct relationship of this kind remains for the subject on this document.".to_owned(),
            (false, _, true) if through.starts_with("not readable") => format!("The direct relationship's state after this change is {through}."),
            (false, _, true) => "The direct relationship was removed. Access through the document owner, another relation, admin or Tools sources was not evaluated.".to_owned(),
        };
        let mut assumptions = vec![
            "Grants carry no provenance: a revoke removes the direct grant even if someone else made it.".to_owned(),
            "Native DefraDB applied the held-bounded rule to this Agent's own signer; this receipt reports what it accepted.".to_owned(),
            "The requesting User is not carried into this tool; the acting Agent is recorded in metadata.".to_owned(),
        ];
        if relation {
            assumptions.push("Document relationships are read as direct tuples only: owner, implied-relation, admin and Tools paths are not evaluated.".to_owned());
        }
        ordered! {
            "answer": answer,
            "effective_access": access_now,
            "assumptions": assumptions,
            "metadata": json!({
                "acting_agent_did": actor.as_str(),
                "subject_did": call.subject.as_str(),
                "node_did": self.node_did,
                "change": target_json(call.target.as_ref()),
            }),
        }
        .pretty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parsed(words: &[&str]) -> ParsedArgs {
        let argv = words.iter().map(|word| (*word).to_owned()).collect::<Vec<_>>();
        ParsedArgs::parse(&argv).expect("argv parses")
    }

    #[test]
    fn target_is_one_node_permission_or_one_relationship() {
        assert!(parse_target(&parsed(&[])).unwrap().is_none());
        assert!(matches!(
            parse_target(&parsed(&["--permission", "read-document"])).unwrap(),
            Some(PolicyTarget::Node(NodePermission::DocumentRead))
        ));
        assert!(matches!(
            parse_target(&parsed(&[
                "--collection", "Agent", "--doc", "bae-1", "--relation", "config_writer"
            ]))
            .unwrap(),
            Some(PolicyTarget::Relation { .. })
        ));
        assert!(parse_target(&parsed(&["--permission", "read-document", "--doc", "bae-1"])).is_err());
        assert!(parse_target(&parsed(&["--collection", "Agent"])).is_err());
    }

    #[test]
    fn unknown_permission_names_the_next_call() {
        let error = parse_target(&parsed(&["--permission", "not-a-permission"])).unwrap_err();
        let guidance = error.downcast_ref::<CommandGuidance>().expect("guidance");
        assert_eq!(guidance.next_call, json!({"argv": ["policy", "get"]}));
    }

    #[test]
    fn admin_class_permissions_are_named_by_the_native_class() {
        assert!(NodePermission::NacRelationAdd.is_admin_class());
        assert!(NodePermission::DacBypass.is_admin_class());
        assert!(!NodePermission::DocumentRead.is_admin_class());
    }

    #[test]
    fn modeled_policy_exposure_controls_the_resource() {
        for case in &crate::lean_vocab_test::lean_contract_snapshot().policy_resource_exposure_cases {
            let categories = case["categories"]
                .as_array()
                .unwrap()
                .iter()
                .map(|value| value.as_str().unwrap().to_owned())
                .collect::<BTreeSet<_>>();
            let exposed = model_resources(&categories, false).contains(&"policy");
            assert_eq!(exposed, case["exposed"].as_bool().unwrap(), "{categories:?}");
        }
    }

    #[test]
    fn modeled_lockout_guard_matches_the_grant_owner() {
        let lean = crate::lean_vocab_test::lean_contract_snapshot();
        let protected = lean.policy_lockout_cases["protected"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap().to_owned())
            .collect::<BTreeSet<_>>();
        let owned = GrantSet::AgentBaseline
            .permissions()
            .iter()
            .chain(GrantSet::PolicyDelegate.permissions())
            .map(|permission| permission.as_str().to_owned())
            .collect::<BTreeSet<_>>();
        assert_eq!(owned, protected);
        for row in lean.policy_lockout_cases["rows"].as_array().unwrap() {
            let permission = NodePermission::parse(row["permission"].as_str().unwrap())
                .expect("modeled permission");
            assert_eq!(
                keeps_lockout(
                    row["no_lockout"].as_bool().unwrap(),
                    row["is_self"].as_bool().unwrap(),
                    row["delete"].as_bool().unwrap(),
                    &PolicyTarget::Node(permission),
                ),
                row["kept"].as_bool().unwrap(),
                "{row}"
            );
        }
    }
}
