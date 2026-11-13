use super::policy_names::{self, HouseholdName};
use super::*;
use gents_session::session::output_render::ordered;

use acp::nac::{PermissionProvenance, PermissionVia};
use gents_store::config_client::{ConfigAccess, ConfigClientError, NodePermission};
use gents_store::identity::GrantSet;

const VERBS: [&str; 5] = ["get", "check", "explain", "grant", "revoke"];

/// What `--permission`, `--collection`, `--doc` and `--relation` name, before
/// a Household name is placed on its document.
#[derive(Debug)]
enum NamedTarget {
    Node(NodePermission),
    Relation {
        collection: String,
        doc_id: String,
        relation: String,
    },
    /// Lean `SelfConfig.resolvePolicyName`: a word of the Household policy,
    /// on the named document or, without one, on this Node.
    Household {
        word: String,
        names: Vec<HouseholdName>,
        document: Option<(String, String)>,
    },
}

#[derive(Debug)]
enum PolicyTarget {
    Node(NodePermission),
    Relation {
        collection: String,
        doc_id: String,
        relation: String,
    },
}

/// A permission of the Household policy on one document. The policy derives it
/// from relations and ownership, so it is checked, never granted.
#[derive(Debug)]
struct DocumentPermission {
    collection: String,
    doc_id: String,
    name: HouseholdName,
}

impl DocumentPermission {
    fn words(&self) -> String {
        format!(
            "permission {} on {} document {}",
            self.name.name, self.collection, self.doc_id
        )
    }

    /// How the document policy gives it, in the policy's own words.
    fn sources(&self) -> String {
        if self.name.expr.is_empty() {
            "to the document's owner only".to_owned()
        } else {
            format!("through {}, and to the document's owner", self.name.expr)
        }
    }

    fn check_argv(&self, verb: &str) -> Value {
        json!({"argv": [
            "policy", verb, "--collection", self.collection, "--doc", self.doc_id,
            "--permission", self.name.name,
        ]})
    }
}

/// A named target placed on its document.
enum Placed {
    Target(PolicyTarget),
    Permission(DocumentPermission),
}

#[derive(Debug)]
struct PolicyCall {
    subject: ::identity::Did,
    is_self: bool,
    subject_given: bool,
    target: Option<PolicyTarget>,
    permission: Option<DocumentPermission>,
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
        _ => &["subject", "permission", "collection", "doc", "relation"],
    };
    if let Some(name) = parsed
        .options
        .keys()
        .find(|name| !allowed.contains(&name.as_str()))
    {
        return Err(guidance(
            format!(
                "policy {verb} does not accept --{name}; accepted: {}",
                allowed
                    .iter()
                    .map(|o| format!("--{o}"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            json!({"argv": ["policy", "--help"]}),
        ));
    }
    anyhow::ensure!(
        parsed.positionals.is_empty() && parsed.switches.is_empty(),
        "policy {verb} takes named options only; see [\"help\",\"policy\"]"
    );
    Ok(())
}

/// A node permission or a word of the Household policy (`--permission`, the
/// latter with or without `--collection` and `--doc`), or one document
/// relationship (`--collection`, `--doc`, `--relation`).
fn parse_target(parsed: &ParsedArgs) -> Result<Option<NamedTarget>> {
    let permission = parsed.one("permission")?;
    let collection = parsed.one("collection")?;
    let doc_id = parsed.one("doc")?;
    let relation = parsed.one("relation")?;
    match (permission, collection, doc_id, relation) {
        (None, None, None, None) => Ok(None),
        (Some(name), None, None, None) if NodePermission::parse(name).is_some() => {
            Ok(NodePermission::parse(name).map(NamedTarget::Node))
        }
        (Some(word), collection, doc_id, None) if collection.is_some() == doc_id.is_some() => {
            let names = policy_names::resolve(word)?;
            if names.is_empty() {
                return Err(guidance(
                    format!("unknown permission {word:?}; --permission takes a node permission (policy get lists the ones held, e.g. read-document) or a relation or permission of the document policy, bare or as resource#name (e.g. manage_agents, nodes#agent_manager)"),
                    json!({"argv": ["policy", "get"]}),
                ));
            }
            Ok(Some(NamedTarget::Household {
                word: word.to_owned(),
                names,
                document: collection
                    .zip(doc_id)
                    .map(|(collection, doc_id)| (collection.to_owned(), doc_id.to_owned())),
            }))
        }
        (None, Some(collection), Some(doc_id), Some(relation)) => Ok(Some(NamedTarget::Relation {
            collection: collection.to_owned(),
            doc_id: doc_id.to_owned(),
            relation: relation.to_owned(),
        })),
        _ => Err(guidance(
            "name one target: --permission NAME for a node permission or a relation or permission of this Node; --collection, --doc and --permission NAME for one of another document; or --collection, --doc and --relation for a document relationship",
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

/// The `policy` argv that applies `verb` to the same subject and target.
fn change_argv(verb: &str, subject: &::identity::Did, target: &PolicyTarget) -> Value {
    let mut argv = vec![
        "policy".to_owned(),
        verb.to_owned(),
        "--subject".to_owned(),
        subject.as_str().to_owned(),
    ];
    match target {
        PolicyTarget::Node(permission) => {
            argv.extend(["--permission".to_owned(), permission.as_str().to_owned()]);
        }
        PolicyTarget::Relation {
            collection,
            doc_id,
            relation,
        } => argv.extend([
            "--collection".to_owned(),
            collection.clone(),
            "--doc".to_owned(),
            doc_id.clone(),
            "--relation".to_owned(),
            relation.clone(),
        ]),
    }
    json!({ "argv": argv })
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

/// A relationship check needs the document's relationships, which DefraDB
/// lists only for its owner or a manager of its relations. The refusal names
/// the permissions the relation gives, which this agent can check for itself.
fn relationships_unreadable(
    collection: &str,
    doc_id: &str,
    relation: &str,
    error: &ConfigClientError,
) -> anyhow::Error {
    let given = gents_protocol::schemas::identity_policy_resource(collection)
        .and_then(|resource| policy_names::permissions_given_by(resource, relation).ok())
        .unwrap_or_default();
    let unreadable = format!("cannot read the relationships of {collection} {doc_id}: {error}");
    match given.first() {
        Some(permission) => guidance(
            format!(
                "{unreadable}. Only the document's owner or a manager of its relations lists them. {relation} gives {}; policy check with that permission answers whether you hold it",
                given.join(", ")
            ),
            json!({"argv": [
                "policy", "check", "--collection", collection, "--doc", doc_id,
                "--permission", permission,
            ]}),
        ),
        None => guidance(unreadable, json!({"argv": ["policy", "get"]})),
    }
}

/// A Household word that the named document's policy resource does not
/// declare, or that no document was named for.
fn misplaced_name(word: &str, names: &[HouseholdName], collection: Option<&str>) -> anyhow::Error {
    let mut resources = names
        .iter()
        .map(|name| name.resource.as_str())
        .collect::<Vec<_>>();
    resources.dedup();
    let kind = names.first().map_or("name", HouseholdName::kind);
    let bare = names.first().map_or(word, |name| name.name.as_str());
    let message = match collection {
        Some(collection) => format!(
            "{collection} documents have no {kind} {bare:?} in the document policy; it belongs to {} documents",
            resources.join(", ")
        ),
        None => format!(
            "{word:?} is a {kind} of {} documents, not of the Node; name the document with --collection COLLECTION --doc DOC_ID --permission {bare}",
            resources.join(", ")
        ),
    };
    guidance(message, json!({"argv": ["policy", "--help"]}))
}

impl ConfigCommandTool {
    /// Lean `SelfConfig.resolvePolicyName`, placed: a word of the Household
    /// policy stands on the named document, or on this Node when none is
    /// named. A relation is the direct relationship; a permission is checked
    /// as native ACP answers it.
    async fn place_target(&self, named: NamedTarget) -> Result<Placed> {
        let (word, names, document) = match named {
            NamedTarget::Node(permission) => {
                return Ok(Placed::Target(PolicyTarget::Node(permission)))
            }
            NamedTarget::Relation {
                collection,
                doc_id,
                relation,
            } => {
                return Ok(Placed::Target(PolicyTarget::Relation {
                    collection,
                    doc_id,
                    relation,
                }))
            }
            NamedTarget::Household {
                word,
                names,
                document,
            } => (word, names, document),
        };
        let (collection, doc_id) = match document {
            Some(document) => document,
            None if names.iter().any(|name| name.resource == "nodes") => {
                ("Node".to_owned(), self.node_document().await?)
            }
            None => return Err(misplaced_name(&word, &names, None)),
        };
        let resource = gents_protocol::schemas::identity_policy_resource(&collection);
        let Some(name) = names
            .iter()
            .find(|name| Some(name.resource.as_str()) == resource)
            .cloned()
        else {
            return Err(misplaced_name(&word, &names, Some(collection.as_str())));
        };
        Ok(if name.is_permission {
            Placed::Permission(DocumentPermission {
                collection,
                doc_id,
                name,
            })
        } else {
            Placed::Target(PolicyTarget::Relation {
                collection,
                doc_id,
                relation: name.name,
            })
        })
    }

    /// What this agent natively holds of `permissions` on one document, or on
    /// this Node's document when none is named.
    async fn own_document_permissions(
        &self,
        document: Option<(&str, &str)>,
        permissions: &[String],
    ) -> Result<gents_store::identity::ObservedDocumentPermissions> {
        gents_store::identity::observe_document_permissions(
            &self.core.node_handle(),
            &self.node_did,
            &self.core.identity()?,
            document,
            permissions,
        )
        .await?
        .ok_or_else(|| {
            guidance(
                "this node's Node document is not available to the runtime; name the document with --collection Node --doc DOC_ID",
                json!({"argv": ["policy", "--help"]}),
            )
        })
    }

    async fn node_document(&self) -> Result<String> {
        Ok(self.own_document_permissions(None, &[]).await?.doc_id)
    }

    /// The Node's permissions in the Household policy, each with whether this
    /// agent holds it.
    async fn node_document_permissions(&self) -> Result<Vec<(String, bool)>> {
        let names = policy_names::household_names()?
            .into_iter()
            .filter(|name| name.is_permission && name.resource == "nodes")
            .map(|name| name.name)
            .collect::<Vec<_>>();
        let observed = self.own_document_permissions(None, &names).await?;
        Ok(names.into_iter().zip(observed.held).collect())
    }

    /// Who can give a permission this agent lacks, and with which call.
    fn permission_grant_path(call: &PolicyCall, permission: &DocumentPermission) -> String {
        let relations = permission.name.granting_relations();
        let given = format!(
            "The document policy gives {} on {} documents {}.",
            permission.name.name,
            permission.name.resource,
            permission.sources()
        );
        if relations.is_empty() {
            return format!("{given} No relationship on this document grants it.");
        }
        format!(
            "{given} An actor cannot add a relationship for itself unless it already manages that relation: the document's owner or a manager of the relation grants one with policy grant --subject {} --collection {} --doc {} --relation RELATION, where RELATION is {}.",
            call.subject.as_str(),
            permission.collection,
            permission.doc_id,
            relations.join(" or ")
        )
    }

    async fn permission_answer(
        &self,
        call: &PolicyCall,
        permission: &DocumentPermission,
    ) -> Result<String> {
        if !call.is_self {
            return Err(guidance(
                format!(
                    "a document permission is answered for this agent only; for another subject list the direct relationships of {} document {}",
                    permission.collection, permission.doc_id
                ),
                json!({"argv": [
                    "policy", "get", "--collection", permission.collection, "--doc", permission.doc_id,
                ]}),
            ));
        }
        let held = self
            .own_document_permissions(
                Some((permission.collection.as_str(), permission.doc_id.as_str())),
                std::slice::from_ref(&permission.name.name),
            )
            .await?
            .held
            == [true];
        let (answer, how) = if held {
            (
                format!("yes: you hold {}.", permission.words()),
                format!("The document policy gives it {}.", permission.sources()),
            )
        } else {
            (
                format!("no: you do not hold {}.", permission.words()),
                Self::permission_grant_path(call, permission),
            )
        };
        ordered! {
            "answer": answer,
            "how_to_proceed": how,
            "metadata": json!({
                "subject_did": call.subject.as_str(),
                "target": {
                    "kind": "document_permission",
                    "collection": permission.collection,
                    "doc_id": permission.doc_id,
                    "resource": permission.name.resource,
                    "permission": permission.name.name,
                },
            }),
        }
        .pretty()
    }

    /// A permission is derived, so a grant or revoke of it names the
    /// relations that give it and the call that explains who grants them.
    fn derived_permission(verb: &str, permission: &DocumentPermission) -> anyhow::Error {
        let relations = permission.name.granting_relations();
        let instead = if relations.is_empty() {
            "no relationship on the document gives it".to_owned()
        } else {
            format!(
                "policy {verb} a relation that gives it instead: {}",
                relations.join(" or ")
            )
        };
        guidance(
            format!(
                "{} is derived by the document policy ({}) and is never granted or revoked itself; {instead}",
                permission.words(),
                permission.sources()
            ),
            permission.check_argv("explain"),
        )
    }

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
        let access = self.core.access()?;
        match (verb, &call.permission) {
            ("get", _) => self.policy_get(&access, &parsed, &call).await,
            ("check" | "explain", Some(permission)) => {
                self.permission_answer(&call, permission).await
            }
            ("check" | "explain", None) => {
                self.policy_answer(&access, &call, verb == "explain").await
            }
            (_, Some(permission)) => Err(Self::derived_permission(verb, permission)),
            (_, None) => self.policy_change(&access, &call, verb == "grant").await,
        }
    }

    async fn policy_call(&self, parsed: &ParsedArgs, verb: &str) -> Result<PolicyCall> {
        let actor = self.core.identity()?;
        let (target, permission) = match parse_target(parsed) {
            _ if verb == "get" => (None, None),
            Ok(Some(named)) => match self.place_target(named).await? {
                Placed::Target(target) => (Some(target), None),
                Placed::Permission(permission) => (None, Some(permission)),
            },
            Ok(None) => (None, None),
            Err(error) => return Err(error),
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
            permission,
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
        let node_document = if call.is_self {
            Some(self.node_document_permissions().await)
        } else {
            None
        };
        let answer = match &node_document {
            Some(Ok(permissions)) => {
                let names = |held: bool| {
                    let names = permissions
                        .iter()
                        .filter(|(_, holds)| *holds == held)
                        .map(|(name, _)| name.as_str())
                        .collect::<Vec<_>>();
                    if names.is_empty() {
                        "none".to_owned()
                    } else {
                        names.join(", ")
                    }
                };
                format!(
                    "{answer} On this Node's document you hold: {}; you do not hold: {}.",
                    names(true),
                    names(false)
                )
            }
            _ => answer,
        };
        let node_document = node_document.map(|permissions| match permissions {
            Ok(permissions) => json!(permissions
                .into_iter()
                .map(|(permission, held)| json!({"permission": permission, "held": held}))
                .collect::<Vec<_>>()),
            Err(error) => json!({"unavailable": format!("{error:#}")}),
        });
        let next = if call.is_self {
            "policy check --permission NAME to test one, a node permission or a relation or permission of this Node's document; policy grant --subject AGENT --permission NAME to delegate a permission you hold."
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
                            format!(
                                "cannot read the relationships of {collection} {doc_id}: {error}"
                            ),
                            json!({"argv": ["policy", "get"]}),
                        )
                    })?,
            ),
            (None, None) => None,
            _ => return Err(guidance(
                "policy get lists a document's relationships only with both --collection and --doc",
                json!({"argv": ["policy", "--help"]}),
            )),
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
            "node_document_permissions": node_document,
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
                        relationships_unreadable(collection, doc_id, relation, &error)
                    })?;
                let subject = call.subject.as_str();
                let ways = rows
                    .iter()
                    .filter(|row| {
                        row.relation == *relation && (row.subject == subject || row.subject == "*")
                    })
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
        if !allowed
            && explain
            && matches!(target, PolicyTarget::Node(permission) if permission.is_admin_class())
        {
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
                access
                    .grant_node_permission(&call.subject, *permission)
                    .await
            }
            PolicyTarget::Node(permission) => {
                access
                    .revoke_node_permission(&call.subject, *permission)
                    .await
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
        // Whoever can grant can revoke (Lean `GrantSets.revoke_follows_grant`),
        // so a grant names the call that withdraws it.
        let how_to_proceed = if grant {
            json!({
                "to_withdraw": change_argv("revoke", &call.subject, target),
            })
        } else {
            json!({
                "to_restore": change_argv("grant", &call.subject, target),
            })
        };
        ordered! {
            "answer": answer,
            "effective_access": access_now,
            "how_to_proceed": how_to_proceed,
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
        let argv = words
            .iter()
            .map(|word| (*word).to_owned())
            .collect::<Vec<_>>();
        ParsedArgs::parse(&argv).expect("argv parses")
    }

    #[test]
    fn target_is_one_node_permission_or_one_relationship() {
        assert!(parse_target(&parsed(&[])).unwrap().is_none());
        assert!(matches!(
            parse_target(&parsed(&["--permission", "read-document"])).unwrap(),
            Some(NamedTarget::Node(NodePermission::DocumentRead))
        ));
        assert!(matches!(
            parse_target(&parsed(&[
                "--collection",
                "Agent",
                "--doc",
                "bae-1",
                "--relation",
                "config_writer"
            ]))
            .unwrap(),
            Some(NamedTarget::Relation { .. })
        ));
        assert!(parse_target(&parsed(&[
            "--permission",
            "read-document",
            "--doc",
            "bae-1"
        ]))
        .is_err());
        assert!(parse_target(&parsed(&["--collection", "Agent"])).is_err());
    }

    #[test]
    fn a_grant_names_the_revoke_of_the_same_target() {
        let subject = ::identity::Did::new("did:key:z6MkSubject".to_owned()).unwrap();
        let target = PolicyTarget::Relation {
            collection: "AgentSession".to_owned(),
            doc_id: "bae-1".to_owned(),
            relation: "reader".to_owned(),
        };
        assert_eq!(
            change_argv("revoke", &subject, &target),
            json!({"argv": [
                "policy", "revoke", "--subject", "did:key:z6MkSubject",
                "--collection", "AgentSession", "--doc", "bae-1", "--relation", "reader",
            ]})
        );
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
    fn contracts_policy_resource_exposure_matches_lean() {
        gents_lean_vocab::families::contracts_policy_resource_exposure::replay(|i| {
            let categories = i.categories.iter().cloned().collect::<BTreeSet<_>>();
            model_resources(&categories, false).contains(&"policy")
        });
    }

    #[test]
    fn contracts_policy_tool_words_matches_lean() {
        use gents_lean_vocab::families::contracts_policy_tool_words as family;
        family::replay(|i| {
            let names = match parse_target(&parsed(&["--permission", &i.word])) {
                Ok(Some(NamedTarget::Household { names, .. })) => names,
                _ => Vec::new(),
            };
            family::PolicyWordOutput {
                names: names
                    .into_iter()
                    .map(|name| family::PolicyWordName {
                        resource: name.resource,
                        is_permission: name.is_permission,
                    })
                    .collect(),
                catalog_refusal: gents_protocol::operations::agent::CATALOG_REFUSAL_WORDS
                    .contains(&i.word.as_str()),
            }
        });
    }

    #[test]
    fn contracts_policy_lockout_matches_lean() {
        gents_lean_vocab::families::contracts_policy_lockout::replay(|i| {
            let owned = GrantSet::AgentBaseline
                .permissions()
                .iter()
                .chain(GrantSet::PolicyDelegate.permissions())
                .map(|permission| permission.as_str().to_owned())
                .collect::<BTreeSet<_>>();
            gents_lean_vocab::families::contracts_policy_lockout::LockoutOutput {
                protected: owned.into_iter().collect(),
                kept: i
                    .rows
                    .iter()
                    .map(|row| {
                        let permission = NodePermission::parse(row.permission.as_str())
                            .expect("modeled permission");
                        keeps_lockout(
                            row.no_lockout,
                            row.is_self,
                            row.delete,
                            &PolicyTarget::Node(permission),
                        )
                    })
                    .collect(),
            }
        });
    }
}
