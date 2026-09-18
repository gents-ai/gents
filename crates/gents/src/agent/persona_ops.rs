//! Shared authenticated persona configuration composer. Admission validates the
//! principal's published profile/root/preset choices; application publishes
//! canonical behavior/context/tools documents through the desired-state owner.
//! Inference selection references an existing profile, including model and effort.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use anyhow::{Context, Result};
use defra_node::EmbeddedNode;
use gents_protocol::persona::LocalPersonaRequestRecord;

use super::persona_presets;
use crate::config_client::{
    apply_desired_state_plan, read_desired_state_document_in_txn, ConfigAccess, ConfigApplyTxn,
    DesiredStateApplyDocument, DesiredStateApplyPlan,
};
use crate::document_config::{
    AgentBehavior as AgentBehaviorDocument, AgentContext, AgentPrincipal, BashTools, BuiltInTools,
    FileTools, HostTools, Tools,
};
use crate::Collection;

/// The published options and this agent's current behaviors — everything
/// [`decide_persona_request`] validates a request against. Callers (the
/// reconciler, the self-config tool, the CLI) assemble this once per
/// decision from the directory-catalog projection and this agent's
/// `AgentBehavior` rows; this module does not load it itself.
#[derive(Debug, Clone, Default)]
pub struct PersonaCatalogView {
    /// Published global operator-local cwd choices after the process-ceiling
    /// meet. WorkspaceRoot has no principal identity in its schema.
    pub allowed_roots: BTreeSet<String>,
    /// Presence of any WorkspaceRoot document, including disabled rows. When
    /// true, a blank requested root is a widening attempt, not permission to
    /// inherit the broader process ceiling.
    pub root_policy_configured: bool,
    /// Process ceiling used to refresh WorkspaceRoot policy inside the apply
    /// transaction. Observation only; it is not another persisted config.
    pub operator_ceiling: Option<std::path::PathBuf>,
    /// Inference profile ids published for this deployment.
    pub available_profile_ids: BTreeSet<String>,
    /// Enabled `AgentPrincipal` DIDs on this deployment. Every op requires
    /// the request's `agent_did` to be in this set (Lean `agentOk`): a
    /// paired device cannot mint orphan behaviors/selections for a phantom
    /// or foreign agent.
    pub known_agent_dids: BTreeSet<String>,
    /// This agent's own `AgentBehavior` rows, keyed by `behavior_id`.
    pub behaviors: BTreeMap<String, BehaviorRef>,
}

/// The slice of an `AgentBehavior` row admission and apply need: whether it
/// is a legal clone/edit/disable target. Config payloads are read by the
/// shared loader inside the apply transaction.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BehaviorRef {
    pub enabled: bool,
    pub protected: bool,
    /// Canonical Tools.host.root currently stored for this behavior, when it
    /// has a Tools document. Omitted edit fields preserve and re-admit it.
    pub root: Option<String>,
    /// True when the stored Tools selection has a host-facing capability that
    /// needs the shared filesystem root.
    pub root_required: bool,
}

pub const SETUP_STEWARD_BEHAVIOR_TAG: &str = "gents:setup-steward";

/// The explicit first-run configurator grant shared with desktop and CLI.
/// Working behaviors do not inherit this grant.
pub fn setup_steward_self_config() -> crate::document_config::SelfConfigTools {
    serde_json::from_str(gents_protocol::SETUP_SELF_CONFIG_JSON)
        .expect("bundled Setup grant must match canonical SelfConfigTools")
}

/// The requested operation, with `clone_from` folded in for `create` (the
/// only op it applies to).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PersonaOp {
    Create { clone_from: Option<String> },
    Edit,
    Disable,
}

impl PersonaOp {
    /// Parse a `PersonaConfigRequest.op` column value. `None` means the
    /// request is malformed and [`decide_persona_request`] must reject it —
    /// callers building a [`PersonaRequestDoc`] from a raw row should set
    /// both `op_raw` (for the rejection message) and this parsed result.
    pub fn parse(op_raw: &str, clone_from: Option<String>) -> Option<Self> {
        match op_raw {
            "create" => Some(PersonaOp::Create { clone_from }),
            "edit" => Some(PersonaOp::Edit),
            "disable" => Some(PersonaOp::Disable),
            _ => None,
        }
    }
}

/// A typed `PersonaConfigRequest` row. `op_raw` is kept alongside the parsed
/// `op` so a bad op string survives into the rejection message instead of
/// being discarded during parsing.
#[derive(Debug, Clone, Default)]
pub struct PersonaRequestDoc {
    /// Exact physical row identity used for terminal outcome mutation.
    pub doc_id: String,
    pub request_key: String,
    pub requester_did: String,
    pub agent_did: String,
    pub authority_kind: String,
    pub local_signer_did: String,
    pub local_signature: Vec<u8>,
    pub local_signature_valid: bool,
    pub network_id: String,
    pub member_peer: String,
    pub enrollment_request_digest: String,
    pub authorization_sequence: u64,
    pub authorization_expires_at: String,
    /// Set only by the runtime after a fresh query through the single
    /// enrollment-authority owner matches every immutable generation field.
    pub current_enrollment_authorized: bool,
    pub op_raw: String,
    pub op: Option<PersonaOp>,
    pub behavior_id: Option<String>,
    pub persona_name: Option<String>,
    pub description: Option<String>,
    pub system_prompt: Option<String>,
    pub root: Option<String>,
    pub preset: Option<String>,
    pub profile_id: Option<String>,
    pub edit_fields: Vec<String>,
    pub make_default: bool,
    pub created_at: Option<String>,
    pub status: Option<String>,
    pub status_detail: Option<String>,
    pub applied_behavior_id: Option<String>,
    pub processed_at: Option<String>,
}

impl PersonaRequestDoc {
    pub fn edits(&self, field: &str) -> bool {
        self.edit_fields.iter().any(|candidate| candidate == field)
    }
}

/// Render the one canonical local/self request shape. The signature is over
/// all semantic fields and the GraphQL mutation only transports that record.
pub fn local_persona_request_mutation(record: &LocalPersonaRequestRecord) -> String {
    fn nullable(value: Option<&str>) -> String {
        value
            .map(crate::graphql::escape_graphql_string)
            .map(|value| format!("\"{value}\""))
            .unwrap_or_else(|| "null".to_string())
    }
    let signature = bs58::encode(&record.local_signature).into_string();
    format!(
        r#"mutation {{
            create_PersonaConfigRequest(input: {{
                request_key: "{}", requester_did: "{}", agent_did: "{}",
                authority_kind: "{}", local_signer_did: "{}", local_signature: "{}",
                network_id: null, member_peer: null, enrollment_request_digest: null,
                authorization_sequence: null, authorization_expires_at: null,
                op: "{}", behavior_id: {}, clone_from: {},
                persona_name: {}, description: {}, system_prompt: {},
                root: {}, preset: {}, profile_id: {}, edit_fields: {}, make_default: {},
                created_at: "{}", status: "pending"
            }}) {{ _docID }}
        }}"#,
        crate::graphql::escape_graphql_string(&record.request_key),
        crate::graphql::escape_graphql_string(&record.requester_did),
        crate::graphql::escape_graphql_string(&record.agent_did),
        crate::graphql::escape_graphql_string(&record.authority_kind),
        crate::graphql::escape_graphql_string(&record.local_signer_did),
        crate::graphql::escape_graphql_string(&signature),
        crate::graphql::escape_graphql_string(&record.op),
        nullable(record.behavior_id.as_deref()),
        nullable(record.clone_from.as_deref()),
        nullable(record.persona_name.as_deref()),
        nullable(record.description.as_deref()),
        nullable(record.system_prompt.as_deref()),
        nullable(record.root.as_deref()),
        nullable(record.preset.as_deref()),
        nullable(record.profile_id.as_deref()),
        if record.edit_fields.is_empty() {
            "null".to_string()
        } else {
            format!(
                "[{}]",
                record
                    .edit_fields
                    .iter()
                    .map(|field| format!("\"{}\"", crate::graphql::escape_graphql_string(field)))
                    .collect::<Vec<_>>()
                    .join(",")
            )
        },
        record.make_default,
        crate::graphql::escape_graphql_string(&record.created_at),
    )
}

/// The result of admission. `Reject`'s message is user-facing: it becomes
/// `PersonaConfigRequest.status_detail` and surfaces verbatim in CLI error
/// output, so every rejection names the offending value and where the valid
/// options came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PersonaVerdict {
    Admit,
    Reject(String),
}

const PERSONA_NAME_MAX_LEN: usize = 64;

fn validate_persona_name(name: Option<&str>) -> Option<String> {
    let name = name.unwrap_or("");
    let len = name.chars().count();
    if len == 0 || len > PERSONA_NAME_MAX_LEN {
        return Some(format!(
            r#"display_name "{name}" must be 1-{PERSONA_NAME_MAX_LEN} characters (got {len})"#
        ));
    }
    None
}

fn validate_system_prompt(prompt: Option<&str>, required: bool) -> Option<String> {
    match prompt {
        Some(prompt) if prompt.trim().is_empty() => {
            Some("system_prompt must not be blank when supplied".to_string())
        }
        None if required => Some(
            "system_prompt is required when creating a behavior from a permission preset"
                .to_string(),
        ),
        _ => None,
    }
}

/// The most entries an enumerated rejection lists before summarizing the
/// remainder — enough to usually settle the question in one round trip
/// without dumping an unbounded catalog into `status_detail` (which renders
/// on phone cards and CLI errors, and must stay one line).
const ENUMERATION_LIMIT: usize = 10;

/// Render a catalog set as `[a, b, c]`, bounded to [`ENUMERATION_LIMIT`]
/// entries with `… and N more` appended when there are more. Iterates a
/// `BTreeSet`, so the shown entries (and thus the message) are deterministic.
fn enumerate_behavior_ids(behaviors: &BTreeMap<String, BehaviorRef>) -> String {
    let ids: BTreeSet<String> = behaviors.keys().cloned().collect();
    enumerate_bounded(&ids)
}

fn enumerate_bounded(values: &BTreeSet<String>) -> String {
    let total = values.len();
    let shown: Vec<&str> = values
        .iter()
        .take(ENUMERATION_LIMIT)
        .map(String::as_str)
        .collect();
    if total > ENUMERATION_LIMIT {
        format!(
            "[{}] … and {} more",
            shown.join(", "),
            total - ENUMERATION_LIMIT
        )
    } else {
        format!("[{}]", shown.join(", "))
    }
}

fn validate_root(root: Option<&str>, catalog: &PersonaCatalogView) -> Option<String> {
    let root = root.unwrap_or("").trim();
    if root.is_empty() {
        if catalog.root_policy_configured {
            return Some(format!(
                "root is required while explicit WorkspaceRoot policy is configured — pick a descendant of the published allowed_roots: {}",
                enumerate_bounded(&catalog.allowed_roots)
            ));
        }
        return None;
    }
    if !std::path::Path::new(root).is_absolute() {
        return Some(format!(
            r#"root "{root}" must be absolute — pick a descendant of the published allowed_roots: {}"#,
            enumerate_bounded(&catalog.allowed_roots)
        ));
    }
    let policy = crate::tool_surface::WorkspaceRootPolicy {
        configured: catalog.root_policy_configured,
        published: catalog.allowed_roots.iter().map(Into::into).collect(),
    };
    match policy.admit(std::path::Path::new(root)) {
        Ok(crate::tool_surface::RootAdmission::Admitted(_)) => None,
        Ok(denied @ crate::tool_surface::RootAdmission::Denied { .. }) => Some(format!(
            r#"root "{root}" is not allowed ({}) — pick a descendant of the published allowed_roots: {}"#,
            denied.denial_reason().expect("denied outcome has a reason"),
            enumerate_bounded(&catalog.allowed_roots)
        )),
        Err(error) => Some(format!(
            r#"root "{root}" could not be resolved safely against published allowed_roots: {error:#}"#
        )),
    }
}

fn validate_root_before_persistence(
    doc: &PersonaRequestDoc,
    catalog: &PersonaCatalogView,
) -> Result<Option<std::path::PathBuf>> {
    let validates_root = matches!(doc.op, Some(PersonaOp::Create { .. }))
        || matches!(doc.op, Some(PersonaOp::Edit)) && doc.edits("root");
    if validates_root {
        if let Some(message) = validate_root(doc.root.as_deref(), catalog) {
            return Err(persona_root_rejection(format!(
                "persona root rejected immediately before persistence: {message}"
            )));
        }
        let root = doc.root.as_deref().unwrap_or("").trim();
        if !root.is_empty() {
            let policy = crate::tool_surface::WorkspaceRootPolicy {
                configured: catalog.root_policy_configured,
                published: catalog.allowed_roots.iter().map(Into::into).collect(),
            };
            return match policy.admit(std::path::Path::new(root)).map_err(|error| {
                persona_root_rejection(format!(
                    "persona root rejected immediately before persistence: {error:#}"
                ))
            })? {
                crate::tool_surface::RootAdmission::Admitted(root) => Ok(Some(root)),
                denied @ crate::tool_surface::RootAdmission::Denied { .. } => {
                    Err(persona_root_rejection(format!(
                        "persona root rejected immediately before persistence: {}",
                        denied.denial_reason().expect("denied outcome has a reason")
                    )))
                }
            };
        }
    }
    Ok(None)
}

#[derive(Debug, thiserror::Error)]
#[error("{0}")]
struct PersonaRootPolicyRejection(String);

fn persona_root_rejection(detail: String) -> anyhow::Error {
    anyhow::Error::new(PersonaRootPolicyRejection(detail))
}

pub(crate) fn persona_root_rejection_detail(error: &anyhow::Error) -> Option<String> {
    error
        .downcast_ref::<PersonaRootPolicyRejection>()
        .map(ToString::to_string)
}

fn validate_profile(profile_id: Option<&str>, catalog: &PersonaCatalogView) -> Option<String> {
    let profile_id = profile_id.unwrap_or("");
    if profile_id.trim().is_empty() || !catalog.available_profile_ids.contains(profile_id) {
        return Some(format!(
            r#"unknown profile "{profile_id}" — pick from the published available_profile_ids: {}"#,
            enumerate_bounded(&catalog.available_profile_ids)
        ));
    }
    None
}

fn validate_preset_name(preset: &str) -> Option<String> {
    if persona_presets::preset_fields(preset).is_none() {
        return Some(format!(
            r#"unknown preset "{preset}" — pick from {}"#,
            persona_presets::builtin_preset_names().join("|")
        ));
    }
    None
}

/// Pure admission gate. Every conjunct's rejection message names the
/// offending value and the source of truth for valid values, per the
/// contract in [`PersonaVerdict`].
pub fn decide_persona_request(
    doc: &PersonaRequestDoc,
    catalog: &PersonaCatalogView,
) -> PersonaVerdict {
    let Some(op) = doc.op.as_ref() else {
        return PersonaVerdict::Reject(format!(
            r#"unknown op "{}" — pick from create|edit|disable"#,
            doc.op_raw
        ));
    };

    match doc.authority_kind.as_str() {
        gents_protocol::persona::PERSONA_AUTHORITY_ENROLLMENT => {
            if !doc.current_enrollment_authorized {
                return PersonaVerdict::Reject(
                    "persona request has no exact current enrollment authorization".to_string(),
                );
            }
        }
        gents_protocol::persona::PERSONA_AUTHORITY_LOCAL_SELF => {
            if !doc.local_signature_valid
                || doc.local_signer_did != doc.requester_did
                || doc.requester_did != doc.agent_did
            {
                return PersonaVerdict::Reject(
                    "persona request has no valid local-self principal signature".to_string(),
                );
            }
        }
        _ => {
            return PersonaVerdict::Reject(format!(
                "persona request has unknown authority_kind {:?}",
                doc.authority_kind
            ));
        }
    }

    // Lean `agentOk`: every op requires a known enabled principal, so a
    // request can never mint or touch config for a phantom/foreign agent.
    if !catalog.known_agent_dids.contains(&doc.agent_did) {
        return PersonaVerdict::Reject(format!(
            r#"unknown agent_did "{}" — no enabled AgentPrincipal with this DID on this deployment"#,
            doc.agent_did
        ));
    }

    match op {
        PersonaOp::Create { clone_from } => {
            if let Some(msg) = validate_persona_name(doc.persona_name.as_deref()) {
                return PersonaVerdict::Reject(msg);
            }
            if let Some(msg) = validate_root(doc.root.as_deref(), catalog) {
                return PersonaVerdict::Reject(msg);
            }
            if let Some(msg) = validate_profile(doc.profile_id.as_deref(), catalog) {
                return PersonaVerdict::Reject(msg);
            }

            let preset = doc.preset.as_deref().unwrap_or("").trim();
            match clone_from {
                Some(source_id) => {
                    if let Some(msg) = validate_system_prompt(doc.system_prompt.as_deref(), false) {
                        return PersonaVerdict::Reject(msg);
                    }
                    if !preset.is_empty() {
                        return PersonaVerdict::Reject(format!(
                            r#"create with clone_from must not also set preset "{preset}" — omit preset when cloning"#
                        ));
                    }
                    match catalog.behaviors.get(source_id) {
                        None => {
                            return PersonaVerdict::Reject(format!(
                                r#"unknown clone_from "{source_id}" — pick from this agent's behaviors: {}"#,
                                enumerate_behavior_ids(&catalog.behaviors)
                            ));
                        }
                        Some(source) if !source.enabled => {
                            return PersonaVerdict::Reject(format!(
                                r#"clone_from "{source_id}" is disabled — pick an enabled behavior_id"#
                            ));
                        }
                        Some(_) => {}
                    }
                }
                None => {
                    if let Some(msg) = validate_system_prompt(doc.system_prompt.as_deref(), true) {
                        return PersonaVerdict::Reject(msg);
                    }
                    if preset.is_empty() {
                        return PersonaVerdict::Reject(format!(
                            r#"unknown preset "" — pick from {}"#,
                            persona_presets::builtin_preset_names().join("|")
                        ));
                    }
                    if let Some(msg) = validate_preset_name(preset) {
                        return PersonaVerdict::Reject(msg);
                    }
                }
            }
            PersonaVerdict::Admit
        }
        PersonaOp::Edit => {
            let behavior_id = doc.behavior_id.as_deref().unwrap_or("");
            let Some(target) = catalog.behaviors.get(behavior_id) else {
                return PersonaVerdict::Reject(format!(
                    r#"unknown behavior_id "{behavior_id}" — pick from this agent's behaviors: {}"#,
                    enumerate_behavior_ids(&catalog.behaviors)
                ));
            };
            if target.protected {
                return PersonaVerdict::Reject(format!(
                    r#"behavior_id "{behavior_id}" is a protected configurator and cannot be edited through config behavior"#
                ));
            }
            if doc.edits("display_name") {
                if let Some(name) = doc.persona_name.as_deref() {
                    if let Some(msg) = validate_persona_name(Some(name)) {
                        return PersonaVerdict::Reject(msg);
                    }
                }
            }
            if doc.edits("root") {
                if let Some(msg) = validate_root(doc.root.as_deref(), catalog) {
                    return PersonaVerdict::Reject(msg);
                }
            } else if target.root_required {
                if let Some(msg) = validate_root(target.root.as_deref(), catalog) {
                    return PersonaVerdict::Reject(format!(
                        "stored Tools root is no longer admissible: {msg}"
                    ));
                }
            }
            if doc.edits("profile_id") {
                if let Some(msg) = validate_profile(doc.profile_id.as_deref(), catalog) {
                    return PersonaVerdict::Reject(msg);
                }
            }
            if doc.edits("system_prompt") {
                if let Some(msg) = validate_system_prompt(doc.system_prompt.as_deref(), false) {
                    return PersonaVerdict::Reject(msg);
                }
            }
            if doc.edits("preset") {
                let Some(preset) = doc
                    .preset
                    .as_deref()
                    .map(str::trim)
                    .filter(|v| !v.is_empty())
                else {
                    return PersonaVerdict::Reject(
                        "preset cannot be cleared because it is a materialization choice, not stored configuration; omit it to preserve Tools"
                            .to_string(),
                    );
                };
                if let Some(msg) = validate_preset_name(preset) {
                    return PersonaVerdict::Reject(msg);
                }
            }
            PersonaVerdict::Admit
        }
        PersonaOp::Disable => {
            if doc.make_default {
                return PersonaVerdict::Reject("disable must not request make_default".to_string());
            }
            let behavior_id = doc.behavior_id.as_deref().unwrap_or("");
            let Some(target) = catalog.behaviors.get(behavior_id) else {
                return PersonaVerdict::Reject(format!(
                    r#"unknown behavior_id "{behavior_id}" — pick from this agent's behaviors: {}"#,
                    enumerate_behavior_ids(&catalog.behaviors)
                ));
            };
            if target.protected {
                return PersonaVerdict::Reject(format!(
                    r#"behavior_id "{behavior_id}" is a protected configurator and cannot be disabled"#
                ));
            }
            PersonaVerdict::Admit
        }
    }
}

fn slugify(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut pending_sep = false;
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() {
            if pending_sep && !out.is_empty() {
                out.push('-');
            }
            out.push(ch.to_ascii_lowercase());
            pending_sep = false;
        } else {
            pending_sep = true;
        }
    }
    out
}

/// Derive a globally-unique `AgentBehavior.behavior_id` from a persona name:
/// `{agent_did}:{slug}`, with `-2`, `-3`, … appended on collision against
/// `existing` (this agent's current behaviors). The `agent_did` prefix keeps
/// ids globally unique across agents without needing to scan the whole
/// collection; collision detection only needs to consider this agent's own
/// behaviors.
///
/// Callers must run the repair scan (does a behavior already exist with
/// `context_id == context-{request_key}`?) BEFORE calling this — deriving
/// an id on every retry of an already-applied create would see its own prior
/// output in `existing` and mint a `-2` duplicate instead of recognizing the
/// repair.
pub fn derive_behavior_id(
    agent_did: &str,
    persona_name: &str,
    existing: &BTreeMap<String, BehaviorRef>,
) -> String {
    let slug = slugify(persona_name);
    let base = format!("{agent_did}:{slug}");
    if !existing.contains_key(&base) {
        return base;
    }
    let mut n = 2;
    loop {
        let candidate = format!("{agent_did}:{slug}-{n}");
        if !existing.contains_key(&candidate) {
            return candidate;
        }
        n += 1;
    }
}

/// Built-in presets author the canonical nested capabilities. Other groups
/// remain absent, so no discovered tools or self-configuration are implicitly enabled.
fn tools_from_preset(
    tools_id: String,
    owner: &str,
    name: &str,
    preset: &str,
    root: Option<String>,
) -> Result<Tools> {
    let fields = persona_presets::preset_fields(preset).context("unknown persona preset")?;
    let write = preset == persona_presets::PRESET_WRITE;
    Ok(Tools {
        tools_id,
        agent_did: owner.into(),
        display_name: Some(format!("{name} tools")),
        host: Some(HostTools {
            root,
            files: Some(FileTools {
                mode: crate::tool_surface::FileToolMode::parse(&fields.file_tools_mode)?,
                ..Default::default()
            }),
            bash: Some(BashTools {
                mode: crate::tool_surface::BashMode::parse(&fields.bash_mode)?,
                execution_mode: write.then_some(if cfg!(target_os = "macos") {
                    crate::toolset::CommandExecutionMode::WorkspaceWrite
                } else {
                    crate::toolset::CommandExecutionMode::Unrestricted
                }),
                background_enabled: write,
                ..Default::default()
            }),
            ..Default::default()
        }),
        built_ins: Some(BuiltInTools {
            enable_context_budget: Some(true),
            ..Default::default()
        }),
        ..Default::default()
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersonaApplyOutcome {
    pub behavior_id: String,
    pub repaired: bool,
}

async fn load_config<T: serde::de::DeserializeOwned>(
    txn: &ConfigApplyTxn<'_>,
    collection: Collection,
    owner: &str,
    id: &str,
) -> Result<T> {
    serde_json::from_value(
        read_desired_state_document_in_txn(txn, collection, owner, id)
            .await?
            .with_context(|| format!("missing {} {owner}/{id}", collection.graphql_type()))?,
    )
    .context("decode persona configuration")
}

fn replacement(
    collection: Collection,
    document: &impl serde::Serialize,
) -> Result<DesiredStateApplyDocument> {
    let value = serde_json::to_value(document)?;
    Ok(DesiredStateApplyDocument {
        collection,
        add: value.clone(),
        update: value,
    })
}

/// Publish the complete persona candidate through the shared transaction owner.
/// Configuration references are validated after staging the whole candidate;
/// cloning or changing tools cannot leave half-applied context/behavior documents.
pub async fn apply_persona_request(
    node: &Arc<EmbeddedNode>,
    actor: identity::Did,
    doc: &PersonaRequestDoc,
    catalog: &PersonaCatalogView,
) -> Result<PersonaApplyOutcome> {
    ConfigAccess::transact_local(node, Some(actor), "persona.apply", |txn| Box::pin(async move {
        let op = doc.op.as_ref().context("persona operation missing")?;
        let owner = &doc.agent_did;
        let context_id = format!("context-{}", doc.request_key);
        let tools_id = format!("tools-{}", doc.request_key);
        // Use the same principal-scoped canonical codec as the desired-state loader.
        let (fields, _) = crate::config_client::config_projection(Collection::AgentBehavior, None)?;
        let response = txn.execute(&format!("{{ AgentBehavior(filter: {{agent_did: {{_eq: \"{}\"}}}}) {{ {} }} }}",
            crate::graphql::escape_graphql_string(owner), fields.join(" "))).await?;
        let mut current = BTreeMap::<String, AgentBehaviorDocument>::new();
        for value in gents_protocol::graphql::graphql_rows_from_response(&response, "AgentBehavior") {
            let behavior: AgentBehaviorDocument = serde_json::from_value(value)?;
            anyhow::ensure!(current.insert(behavior.behavior_id.clone(), behavior).is_none(), "ambiguous persona behavior within principal");
        }
        if matches!(op, PersonaOp::Create {..}) {
            let repaired: Vec<_> = current.values().filter(|b| b.context_id.as_deref() == Some(&context_id)).collect();
            anyhow::ensure!(repaired.len() <= 1, "persona request context is bound by multiple behaviors");
            if let Some(behavior) = repaired.first() {
                return Ok(PersonaApplyOutcome {behavior_id:behavior.behavior_id.clone(),repaired:true});
            }
        }
        let source_id = match op {
            PersonaOp::Create {clone_from} => clone_from.as_deref(),
            PersonaOp::Edit | PersonaOp::Disable => Some(doc.behavior_id.as_deref().context("persona behavior missing")?),
        };
        let source = source_id.map(|id| current.get(id).cloned().with_context(|| format!("persona behavior {id} missing"))).transpose()?;
        if matches!(op, PersonaOp::Create {clone_from:Some(_)}) {
            anyhow::ensure!(source.as_ref().is_some_and(|source| source.enabled), "clone source is disabled");
        }
        let mut behavior = if let Some(source) = source.clone() { source } else {
            serde_json::from_value(serde_json::json!({"behavior_id":derive_behavior_id(owner, doc.persona_name.as_deref().context("persona name missing")?, &current.iter().map(|(id,b)| (id.clone(), BehaviorRef {enabled:b.enabled, protected:b.tags.iter().any(|tag| tag == SETUP_STEWARD_BEHAVIOR_TAG), ..Default::default()})).collect()), "agent_did":owner,
                "inference_profile_id":doc.profile_id.as_deref().context("persona profile missing")?}))?
        };
        if matches!(op, PersonaOp::Disable) {
            behavior.enabled = false;
            apply_desired_state_plan(txn, &DesiredStateApplyPlan::new(vec![replacement(Collection::AgentBehavior, &behavior)?])?).await?;
            return Ok(PersonaApplyOutcome {behavior_id:behavior.behavior_id,repaired:false});
        }
        let create = matches!(op, PersonaOp::Create {..});
        if create {
            let name = doc.persona_name.as_deref().context("behavior display_name missing")?;
            let catalog = current.iter().map(|(id,b)| (id.clone(), BehaviorRef {enabled:b.enabled, protected:b.tags.iter().any(|tag| tag == SETUP_STEWARD_BEHAVIOR_TAG), ..Default::default()})).collect();
            behavior.behavior_id = derive_behavior_id(owner, name, &catalog);
            behavior.created_at = None;
            behavior.enabled = true;
            behavior
                .tags
                .retain(|tag| tag != SETUP_STEWARD_BEHAVIOR_TAG);
        }
        if create || doc.edits("display_name") {
            behavior.display_name = doc.persona_name.clone();
        }
        // A clone inherits its source description unless the request supplies
        // an override. A preset-based create has no source to inherit from.
        if source.is_none() || doc.edits("description") || (create && doc.description.is_some()) {
            behavior.description = doc.description.clone();
        }
        if create || doc.edits("profile_id") {
            let profile = doc.profile_id.as_deref().context("behavior profile_id cannot be cleared")?;
            anyhow::ensure!(!profile.trim().is_empty(), "behavior profile_id must not be blank");
            behavior.inference_profile_id = profile.into();
        }
        let mut context: AgentContext = match source.as_ref().and_then(|b| b.context_id.as_deref()) {
            Some(id) => load_config(txn, Collection::AgentContext, owner, id).await?,
            None => serde_json::from_value(serde_json::json!({"context_id":context_id,"agent_did":owner}))?,
        };
        let existing_context = context.clone();
        let existing_tools: Option<Tools> = match context.tools_id.as_deref() {
            Some(id) => Some(load_config(txn, Collection::Tools, owner, id).await?), None => None,
        };
        if create {
            if let Some(system_prompt) = &doc.system_prompt {
                context.system_prompt = Some(system_prompt.clone());
            }
        } else if doc.edits("system_prompt") {
            context.system_prompt = doc.system_prompt.clone();
        }
        // The command exposes one concise description because Behavior and
        // Context are materialized as one reusable interface. Keep both
        // canonical owners coherent instead of leaving the context opaque in
        // later inspect/edit flows.
        if source.is_none() || doc.edits("description") || (create && doc.description.is_some()) {
            context.description = doc.description.clone();
        }
        // Refresh the global operator-local WorkspaceRoot policy inside this
        // same configuration transaction. The preview/admission catalog may
        // be stale by publication time; disabled or changed roots must win
        // before any persona documents are persisted.
        let fresh_root_policy = crate::tool_surface::load_workspace_root_policy_in_txn(
            txn,
            catalog.operator_ceiling.as_deref(),
        )
        .await?;
        let fresh_catalog = PersonaCatalogView {
            allowed_roots: fresh_root_policy.published_strings().collect(),
            root_policy_configured: fresh_root_policy.configured,
            operator_ceiling: catalog.operator_ceiling.clone(),
            available_profile_ids: catalog.available_profile_ids.clone(),
            known_agent_dids: catalog.known_agent_dids.clone(),
            behaviors: catalog.behaviors.clone(),
        };
        let root = validate_root_before_persistence(doc, &fresh_catalog)?
            .map(|root| root.to_string_lossy().into_owned());
        let preset = doc.preset.as_deref().unwrap_or("").trim();
        let effective_name = behavior.display_name.as_deref().unwrap_or(&behavior.behavior_id);
        let mut tools = if create && !preset.is_empty() || doc.edits("preset") {
            let preset_root = if create || doc.edits("root") {
                root.clone()
            } else {
                existing_tools
                    .as_ref()
                    .and_then(|tools| tools.host.as_ref())
                    .and_then(|host| host.root.clone())
            };
            Some(tools_from_preset(
                tools_id.clone(),
                owner,
                effective_name,
                preset,
                preset_root,
            )?)
        } else { existing_tools.clone() };
        // Omitted edit fields preserve their canonical values. A present root
        // with a null payload explicitly clears only the root narrowing.
        if root.is_some() || (!create && doc.edits("root")) {
            if tools.is_none() && root.is_some() { tools = Some(Tools {tools_id:tools_id.clone(),agent_did:owner.clone(),..Default::default()}); }
            if let Some(tools) = &mut tools {
                if let Some(host) = &mut tools.host { host.root = root.clone(); }
                else if root.is_some() { tools.host = Some(HostTools {root:root.clone(),..Default::default()}); }
            }
        }
        if let Some(tools) = &mut tools {
            crate::tool_surface::canonicalize_tools_root(tools, &fresh_root_policy).map_err(
                |error| persona_root_rejection(format!(
                    "persona root rejected immediately before persistence: {error:#}"
                )),
            )?;
        }
        let change_context = create || context != existing_context || tools != existing_tools;
        let mut documents = Vec::new();
        if change_context {
            context.context_id = context_id;
            if let Some(mut tools) = tools {
                tools.tools_id = tools_id.clone();
                context.tools_id = Some(tools_id);
                documents.push(replacement(Collection::Tools, &tools)?);
            } else { context.tools_id = None; }
            behavior.context_id = Some(context.context_id.clone());
            documents.push(replacement(Collection::AgentContext, &context)?);
        }
        documents.push(replacement(Collection::AgentBehavior, &behavior)?);
        if doc.make_default {
            let mut principal: AgentPrincipal =
                load_config(txn, Collection::AgentPrincipal, owner, owner).await?;
            principal.default_behavior_id = Some(behavior.behavior_id.clone());
            documents.push(replacement(Collection::AgentPrincipal, &principal)?);
        }
        apply_desired_state_plan(txn, &DesiredStateApplyPlan::new(documents)?).await?;
        Ok(PersonaApplyOutcome {behavior_id:behavior.behavior_id,repaired:false})
    })).await
}

#[cfg(test)]
pub(crate) async fn seed_persona_validation_references(
    node: &EmbeddedNode,
    owner: &str,
) -> Result<()> {
    crate::document_config::ensure_agent_principal(node, owner).await?;
    let owner = crate::graphql::escape_graphql_string(owner);
    crate::config_client::ConfigAccess::write_local(node,"test.persona.references", &format!(r#"mutation {{
        create_InferenceBackend(input: {{backend_id:"backend",agent_did:"{owner}",name:"Fixture",provider_kind:"OpenAiCompatible",endpoint:"http://127.0.0.1:8000/v1",auth:{{kind:"unauthenticated"}},enabled:true}}) {{_docID}}
        create_InferenceProfile(input: {{profile_id:"profile-1",agent_did:"{owner}",backend_id:"backend",model_name:"model-1"}}) {{_docID}}
        create_InferenceProfile(input: {{profile_id:"profile-2",agent_did:"{owner}",backend_id:"backend",model_name:"model-2"}}) {{_docID}}
    }}"#)).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_ACTOR_DID: &str = "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK";

    fn test_actor() -> identity::Did {
        identity::Did::new(TEST_ACTOR_DID.to_string()).expect("valid test ACP actor")
    }

    fn catalog_with(
        roots: &[&str],
        profiles: &[&str],
        behaviors: &[(&str, bool, &str)],
    ) -> PersonaCatalogView {
        PersonaCatalogView {
            allowed_roots: roots.iter().map(|s| s.to_string()).collect(),
            root_policy_configured: !roots.is_empty(),
            operator_ceiling: roots.first().map(std::path::PathBuf::from),
            available_profile_ids: profiles.iter().map(|s| s.to_string()).collect(),
            known_agent_dids: BTreeSet::from(["did:key:agent".to_string()]),
            behaviors: behaviors
                .iter()
                .map(|(id, enabled, _context_id)| {
                    (
                        id.to_string(),
                        BehaviorRef {
                            enabled: *enabled,
                            protected: false,
                            ..Default::default()
                        },
                    )
                })
                .collect(),
        }
    }

    fn base_catalog() -> PersonaCatalogView {
        catalog_with(
            &["/workspace/root"],
            &["profile-1"],
            &[
                ("existing-enabled", true, "sel-existing-enabled"),
                ("existing-disabled", false, "sel-existing-disabled"),
                ("without-context", true, ""),
            ],
        )
    }

    fn create_doc(op: PersonaOp) -> PersonaRequestDoc {
        PersonaRequestDoc {
            request_key: "req-1".to_string(),
            requester_did: "did:key:requester".to_string(),
            agent_did: "did:key:agent".to_string(),
            authority_kind: gents_protocol::persona::PERSONA_AUTHORITY_ENROLLMENT.to_string(),
            current_enrollment_authorized: true,
            op_raw: "create".to_string(),
            op: Some(op),
            persona_name: Some("Research Assistant".to_string()),
            description: Some("Researches a focused question".to_string()),
            system_prompt: Some("Research the question and cite evidence.".to_string()),
            root: Some("/workspace/root".to_string()),
            preset: Some(persona_presets::PRESET_WRITE.to_string()),
            profile_id: Some("profile-1".to_string()),
            ..Default::default()
        }
    }

    // -- decide_persona_request: one conjunct per test --

    #[test]
    fn rejects_bad_op() {
        let doc = PersonaRequestDoc {
            op_raw: "yeet".to_string(),
            op: None,
            ..Default::default()
        };
        let verdict = decide_persona_request(&doc, &base_catalog());
        assert_eq!(
            verdict,
            PersonaVerdict::Reject(
                r#"unknown op "yeet" — pick from create|edit|disable"#.to_string()
            )
        );
    }

    #[test]
    fn rejects_unknown_agent_did() {
        let mut doc = create_doc(PersonaOp::Create { clone_from: None });
        doc.agent_did = "did:key:phantom".to_string();
        let verdict = decide_persona_request(&doc, &base_catalog());
        assert_eq!(
            verdict,
            PersonaVerdict::Reject(
                r#"unknown agent_did "did:key:phantom" — no enabled AgentPrincipal with this DID on this deployment"#
                    .to_string()
            )
        );
    }

    #[test]
    fn rejects_disabling_and_promoting_the_same_behavior() {
        let mut doc = create_doc(PersonaOp::Disable);
        doc.op_raw = "disable".to_string();
        doc.behavior_id = Some("existing-enabled".to_string());
        doc.make_default = true;
        assert_eq!(
            decide_persona_request(&doc, &base_catalog()),
            PersonaVerdict::Reject("disable must not request make_default".to_string())
        );
    }

    #[test]
    fn rejects_editing_or_disabling_a_protected_configurator() {
        let mut catalog = base_catalog();
        catalog
            .behaviors
            .get_mut("existing-enabled")
            .expect("fixture behavior")
            .protected = true;

        let mut edit = create_doc(PersonaOp::Edit);
        edit.op_raw = "edit".to_string();
        edit.behavior_id = Some("existing-enabled".to_string());
        assert!(matches!(
            decide_persona_request(&edit, &catalog),
            PersonaVerdict::Reject(detail) if detail.contains("protected configurator")
        ));

        let mut disable = create_doc(PersonaOp::Disable);
        disable.op_raw = "disable".to_string();
        disable.behavior_id = Some("existing-enabled".to_string());
        disable.make_default = false;
        assert!(matches!(
            decide_persona_request(&disable, &catalog),
            PersonaVerdict::Reject(detail) if detail.contains("protected configurator")
        ));
    }

    #[tokio::test]
    async fn persona_create_can_atomically_become_default_without_mutating_setup() -> Result<()> {
        let node = Arc::new(EmbeddedNode::builder().build().await?);
        crate::ensure_runtime_schemas(&node).await?;
        let owner = "did:key:agent";
        seed_persona_validation_references(&node, owner).await?;
        let setup = AgentBehaviorDocument {
            behavior_id: "setup".into(),
            agent_did: owner.into(),
            display_name: Some("Setup".into()),
            context_id: None,
            inference_profile_id: "profile-1".into(),
            enabled: true,
            description: None,
            tags: Vec::new(),
            created_at: None,
        };
        ConfigAccess::Local(node.clone())
            .transact("test.persona.setup", |txn| {
                let setup = setup.clone();
                Box::pin(async move {
                    apply_desired_state_plan(
                        txn,
                        &DesiredStateApplyPlan::new(vec![replacement(
                            Collection::AgentBehavior,
                            &setup,
                        )?])?,
                    )
                    .await?;
                    Ok(())
                })
            })
            .await?;

        let mut doc = create_doc(PersonaOp::Create { clone_from: None });
        doc.request_key = "promoted".into();
        doc.make_default = true;
        let outcome = apply_persona_request(&node, test_actor(), &doc, &base_catalog()).await?;
        let principal: AgentPrincipal =
            read(&node, Collection::AgentPrincipal, owner, owner).await?;
        assert_eq!(
            principal.default_behavior_id.as_deref(),
            Some(outcome.behavior_id.as_str())
        );
        assert_eq!(
            read::<AgentBehaviorDocument>(&node, Collection::AgentBehavior, owner, "setup").await?,
            setup
        );
        Ok(())
    }

    #[test]
    fn rejects_root_not_allowed() {
        let allowed = tempfile::tempdir().expect("allowed root");
        let outside = tempfile::tempdir().expect("outside root");
        let allowed = allowed.path().to_string_lossy().into_owned();
        let outside = outside.path().to_string_lossy().into_owned();
        let catalog = catalog_with(&[allowed.as_str()], &["profile-1"], &[]);
        let mut doc = create_doc(PersonaOp::Create { clone_from: None });
        doc.root = Some(outside.clone());
        let verdict = decide_persona_request(&doc, &catalog);
        assert_eq!(
            verdict,
            PersonaVerdict::Reject(format!(
                r#"root "{outside}" is not allowed (resolved path is outside the allowed roots) — pick a descendant of the published allowed_roots: [{allowed}]"#
            ))
        );
    }

    #[test]
    fn empty_root_is_rejected_under_explicit_policy() {
        let mut doc = create_doc(PersonaOp::Create { clone_from: None });
        doc.root = Some("".to_string());
        let verdict = decide_persona_request(&doc, &base_catalog());
        assert!(matches!(
            verdict,
            PersonaVerdict::Reject(detail) if detail.contains("root is required")
        ));
    }

    #[test]
    fn relative_root_is_rejected() {
        let mut doc = create_doc(PersonaOp::Create { clone_from: None });
        doc.root = Some("relative/project".to_string());
        let verdict = decide_persona_request(&doc, &base_catalog());
        assert!(matches!(
            verdict,
            PersonaVerdict::Reject(detail) if detail.contains("must be absolute")
        ));
    }

    #[test]
    fn rejects_unknown_profile() {
        let mut doc = create_doc(PersonaOp::Create { clone_from: None });
        doc.profile_id = Some("no-such-profile".to_string());
        let verdict = decide_persona_request(&doc, &base_catalog());
        assert_eq!(
            verdict,
            PersonaVerdict::Reject(
                r#"unknown profile "no-such-profile" — pick from the published available_profile_ids: [profile-1]"#
                    .to_string()
            )
        );
    }

    #[test]
    fn rejects_empty_persona_name() {
        let mut doc = create_doc(PersonaOp::Create { clone_from: None });
        doc.persona_name = Some("".to_string());
        let verdict = decide_persona_request(&doc, &base_catalog());
        assert_eq!(
            verdict,
            PersonaVerdict::Reject(
                r#"display_name "" must be 1-64 characters (got 0)"#.to_string()
            )
        );
    }

    #[test]
    fn rejects_65_char_persona_name() {
        let mut doc = create_doc(PersonaOp::Create { clone_from: None });
        let name = "a".repeat(65);
        doc.persona_name = Some(name.clone());
        let verdict = decide_persona_request(&doc, &base_catalog());
        assert_eq!(
            verdict,
            PersonaVerdict::Reject(format!(
                r#"display_name "{name}" must be 1-64 characters (got 65)"#
            ))
        );
    }

    #[test]
    fn rejects_create_clone_with_named_preset() {
        let mut doc = create_doc(PersonaOp::Create {
            clone_from: Some("existing-enabled".to_string()),
        });
        doc.preset = Some(persona_presets::PRESET_WRITE.to_string());
        let verdict = decide_persona_request(&doc, &base_catalog());
        assert_eq!(
            verdict,
            PersonaVerdict::Reject(
                r#"create with clone_from must not also set preset "write" — omit preset when cloning"#
                    .to_string()
            )
        );
    }

    #[test]
    fn rejects_unknown_clone_from() {
        let mut doc = create_doc(PersonaOp::Create {
            clone_from: Some("no-such-behavior".to_string()),
        });
        doc.preset = None;
        let verdict = decide_persona_request(&doc, &base_catalog());
        assert_eq!(
            verdict,
            PersonaVerdict::Reject(
                r#"unknown clone_from "no-such-behavior" — pick from this agent's behaviors: [existing-disabled, existing-enabled, without-context]"#
                    .to_string()
            )
        );
    }

    #[test]
    fn rejects_disabled_clone_from() {
        let mut doc = create_doc(PersonaOp::Create {
            clone_from: Some("existing-disabled".to_string()),
        });
        doc.preset = None;
        let verdict = decide_persona_request(&doc, &base_catalog());
        assert_eq!(
            verdict,
            PersonaVerdict::Reject(
                r#"clone_from "existing-disabled" is disabled — pick an enabled behavior_id"#
                    .to_string()
            )
        );
    }

    #[test]
    fn admits_clone_without_context() {
        let mut doc = create_doc(PersonaOp::Create {
            clone_from: Some("without-context".to_string()),
        });
        doc.preset = None;
        let verdict = decide_persona_request(&doc, &base_catalog());
        assert_eq!(verdict, PersonaVerdict::Admit);
    }

    #[test]
    fn admits_edit_without_context() {
        let mut doc = create_doc(PersonaOp::Edit);
        doc.op_raw = "edit".to_string();
        doc.behavior_id = Some("without-context".to_string());
        doc.preset = None;
        let verdict = decide_persona_request(&doc, &base_catalog());
        assert_eq!(verdict, PersonaVerdict::Admit);
    }

    #[test]
    fn name_only_edit_does_not_require_profile() {
        let mut doc = create_doc(PersonaOp::Edit);
        doc.op_raw = "edit".to_string();
        doc.behavior_id = Some("existing-enabled".to_string());
        doc.persona_name = Some("Renamed".to_string());
        doc.profile_id = None;
        doc.edit_fields = vec!["display_name".to_string()];
        assert_eq!(
            decide_persona_request(&doc, &base_catalog()),
            PersonaVerdict::Admit
        );
    }

    #[test]
    fn profile_edit_distinguishes_omission_from_clear() {
        let mut doc = create_doc(PersonaOp::Edit);
        doc.op_raw = "edit".to_string();
        doc.behavior_id = Some("existing-enabled".to_string());
        doc.profile_id = None;
        assert_eq!(
            decide_persona_request(&doc, &base_catalog()),
            PersonaVerdict::Admit
        );
        doc.edit_fields = vec!["profile_id".to_string()];
        assert!(matches!(
            decide_persona_request(&doc, &base_catalog()),
            PersonaVerdict::Reject(detail) if detail.contains("unknown profile")
        ));
    }

    #[test]
    fn admits_preset_edit_without_context() {
        let mut doc = create_doc(PersonaOp::Edit);
        doc.op_raw = "edit".to_string();
        doc.behavior_id = Some("without-context".to_string());
        doc.preset = Some(persona_presets::PRESET_READONLY.to_string());
        doc.edit_fields = vec!["preset".to_string()];
        assert_eq!(
            decide_persona_request(&doc, &base_catalog()),
            PersonaVerdict::Admit
        );
    }

    #[test]
    fn rejects_edit_unknown_behavior_id() {
        let mut doc = create_doc(PersonaOp::Edit);
        doc.behavior_id = Some("no-such-behavior".to_string());
        let verdict = decide_persona_request(&doc, &base_catalog());
        assert_eq!(
            verdict,
            PersonaVerdict::Reject(
                r#"unknown behavior_id "no-such-behavior" — pick from this agent's behaviors: [existing-disabled, existing-enabled, without-context]"#
                    .to_string()
            )
        );
    }

    #[test]
    fn rejects_disable_unknown_behavior_id() {
        let mut doc = create_doc(PersonaOp::Disable);
        doc.op_raw = "disable".to_string();
        doc.behavior_id = Some("no-such-behavior".to_string());
        let verdict = decide_persona_request(&doc, &base_catalog());
        assert_eq!(
            verdict,
            PersonaVerdict::Reject(
                r#"unknown behavior_id "no-such-behavior" — pick from this agent's behaviors: [existing-disabled, existing-enabled, without-context]"#
                    .to_string()
            )
        );
    }

    #[test]
    fn rejects_unknown_preset_on_plain_create() {
        let mut doc = create_doc(PersonaOp::Create { clone_from: None });
        doc.preset = Some("bogus".to_string());
        let verdict = decide_persona_request(&doc, &base_catalog());
        assert_eq!(
            verdict,
            PersonaVerdict::Reject(
                r#"unknown preset "bogus" — pick from readonly|write"#.to_string()
            )
        );
    }

    #[test]
    fn admits_happy_create() {
        let doc = create_doc(PersonaOp::Create { clone_from: None });
        assert_eq!(
            decide_persona_request(&doc, &base_catalog()),
            PersonaVerdict::Admit
        );
    }

    #[test]
    fn admits_happy_clone() {
        let mut doc = create_doc(PersonaOp::Create {
            clone_from: Some("existing-enabled".to_string()),
        });
        doc.preset = None;
        assert_eq!(
            decide_persona_request(&doc, &base_catalog()),
            PersonaVerdict::Admit
        );
    }

    #[test]
    fn admits_happy_edit() {
        let mut doc = create_doc(PersonaOp::Edit);
        doc.behavior_id = Some("existing-enabled".to_string());
        assert_eq!(
            decide_persona_request(&doc, &base_catalog()),
            PersonaVerdict::Admit
        );
    }

    #[test]
    fn omitted_edit_revalidates_active_stored_root_without_requiring_inactive_tools() {
        let allowed = tempfile::tempdir().expect("allowed root");
        let descendant = allowed.path().join("project");
        std::fs::create_dir_all(&descendant).expect("descendant");
        let outside = tempfile::tempdir().expect("outside root");
        let mut catalog = base_catalog();
        catalog.allowed_roots = BTreeSet::from([std::fs::canonicalize(allowed.path())
            .expect("canonical allowed")
            .to_string_lossy()
            .into_owned()]);
        catalog.root_policy_configured = true;
        let target = catalog
            .behaviors
            .get_mut("existing-enabled")
            .expect("fixture behavior");
        target.root_required = true;
        target.root = Some(descendant.to_string_lossy().into_owned());

        let mut doc = create_doc(PersonaOp::Edit);
        doc.behavior_id = Some("existing-enabled".to_string());
        doc.root = None;
        doc.edit_fields = vec!["display_name".to_string()];
        assert_eq!(
            decide_persona_request(&doc, &catalog),
            PersonaVerdict::Admit
        );

        catalog
            .behaviors
            .get_mut("existing-enabled")
            .expect("fixture behavior")
            .root = Some(outside.path().to_string_lossy().into_owned());
        assert!(matches!(
            decide_persona_request(&doc, &catalog),
            PersonaVerdict::Reject(detail) if detail.contains("stored Tools root is no longer admissible")
        ));

        catalog
            .behaviors
            .get_mut("existing-enabled")
            .expect("fixture behavior")
            .root = None;
        assert!(matches!(
            decide_persona_request(&doc, &catalog),
            PersonaVerdict::Reject(detail) if detail.contains("root is required")
        ));
        catalog
            .behaviors
            .get_mut("existing-enabled")
            .expect("fixture behavior")
            .root_required = false;
        assert_eq!(
            decide_persona_request(&doc, &catalog),
            PersonaVerdict::Admit
        );
    }

    #[test]
    fn admits_happy_disable() {
        let mut doc = create_doc(PersonaOp::Disable);
        doc.op_raw = "disable".to_string();
        doc.behavior_id = Some("existing-enabled".to_string());
        assert_eq!(
            decide_persona_request(&doc, &base_catalog()),
            PersonaVerdict::Admit
        );
    }

    // -- derive_behavior_id --

    #[test]
    fn derives_slugged_id() {
        let id = derive_behavior_id("did:key:agent", "Research Assistant!!", &BTreeMap::new());
        assert_eq!(id, "did:key:agent:research-assistant");
    }

    #[test]
    fn derives_collision_suffix() {
        let mut existing = BTreeMap::new();
        existing.insert(
            "did:key:agent:research-assistant".to_string(),
            BehaviorRef {
                enabled: true,
                protected: false,
                ..Default::default()
            },
        );
        let id = derive_behavior_id("did:key:agent", "Research Assistant", &existing);
        assert_eq!(id, "did:key:agent:research-assistant-2");

        existing.insert(
            "did:key:agent:research-assistant-2".to_string(),
            BehaviorRef {
                enabled: true,
                protected: false,
                ..Default::default()
            },
        );
        let id = derive_behavior_id("did:key:agent", "Research Assistant", &existing);
        assert_eq!(id, "did:key:agent:research-assistant-3");
    }

    #[test]
    fn canonical_presets_preserve_file_bash_and_background_capabilities() {
        let write =
            tools_from_preset("tools".into(), "did:key:agent", "Writer", "write", None).unwrap();
        let host = write.host.unwrap();
        assert_eq!(
            host.files.unwrap().mode,
            crate::tool_surface::FileToolMode::ReadWrite
        );
        let bash = host.bash.unwrap();
        assert_eq!(bash.mode, crate::tool_surface::BashMode::Unrestricted);
        assert!(bash.background_enabled);
        let readonly =
            tools_from_preset("tools".into(), "did:key:agent", "Reader", "readonly", None).unwrap();
        let host = readonly.host.unwrap();
        assert_eq!(
            host.files.unwrap().mode,
            crate::tool_surface::FileToolMode::ReadOnly
        );
        assert!(!host.bash.unwrap().background_enabled);
        assert!(readonly.remote.is_none() && readonly.self_config.is_none());
    }

    async fn read<T: serde::de::DeserializeOwned>(
        node: &EmbeddedNode,
        collection: Collection,
        owner: &str,
        id: &str,
    ) -> Result<T> {
        let value = ConfigAccess::transact_local(node, None, "test.persona.read", |txn| {
            Box::pin(async move {
                read_desired_state_document_in_txn(txn, collection, owner, id)
                    .await?
                    .context("fixture doc missing")
            })
        })
        .await?;
        Ok(serde_json::from_value(value)?)
    }

    #[tokio::test]
    async fn canonical_persona_create_clone_edit_disable_and_atomic_replay() -> Result<()> {
        let node = Arc::new(EmbeddedNode::builder().build().await?);
        crate::ensure_runtime_schemas(&node).await?;
        let owner = "did:key:agent";
        seed_persona_validation_references(&node, owner).await?;
        let mut doc = create_doc(PersonaOp::Create { clone_from: None });
        doc.root = Some("/workspace/root/original".into());
        let catalog = base_catalog();
        let created = apply_persona_request(&node, test_actor(), &doc, &catalog).await?;
        let replay = apply_persona_request(&node, test_actor(), &doc, &catalog).await?;
        assert_eq!(created.behavior_id, replay.behavior_id);
        assert!(replay.repaired);
        let original: AgentBehaviorDocument = read(
            &node,
            Collection::AgentBehavior,
            owner,
            &created.behavior_id,
        )
        .await?;
        assert_eq!(original.inference_profile_id, "profile-1");
        assert_eq!(
            original.description.as_deref(),
            Some("Researches a focused question")
        );
        let mut context: AgentContext = read(
            &node,
            Collection::AgentContext,
            owner,
            original.context_id.as_deref().unwrap(),
        )
        .await?;
        assert_eq!(
            context.system_prompt.as_deref(),
            Some("Research the question and cite evidence.")
        );
        assert_eq!(
            context.description.as_deref(),
            Some("Researches a focused question")
        );
        context.system_prompt = Some("Keep literal {{braces}}".into());
        context.description = Some("Shared context".into());
        ConfigAccess::Local(node.clone())
            .transact("test.persona.context", |txn| {
                let context = context.clone();
                Box::pin(async move {
                    apply_desired_state_plan(
                        txn,
                        &DesiredStateApplyPlan::new(vec![replacement(
                            Collection::AgentContext,
                            &context,
                        )?])?,
                    )
                    .await?;
                    Ok(())
                })
            })
            .await?;
        let source_tools: Tools = read(
            &node,
            Collection::Tools,
            owner,
            context.tools_id.as_deref().unwrap(),
        )
        .await?;
        doc.request_key = "clone".into();
        doc.op = Some(PersonaOp::Create {
            clone_from: Some(created.behavior_id.clone()),
        });
        doc.preset = None;
        doc.persona_name = Some("Cloned".into());
        doc.description = None;
        doc.system_prompt = None;
        doc.root = None;
        doc.profile_id = Some("profile-2".into());
        let cloned = apply_persona_request(&node, test_actor(), &doc, &catalog).await?;
        let behavior: AgentBehaviorDocument =
            read(&node, Collection::AgentBehavior, owner, &cloned.behavior_id).await?;
        assert_eq!(behavior.inference_profile_id, "profile-2");
        assert_eq!(behavior.description, original.description);
        let clone_context: AgentContext = read(
            &node,
            Collection::AgentContext,
            owner,
            behavior.context_id.as_deref().unwrap(),
        )
        .await?;
        assert_eq!(clone_context.system_prompt, context.system_prompt);
        assert_eq!(clone_context.description, context.description);
        let clone_tools: Tools = read(
            &node,
            Collection::Tools,
            owner,
            clone_context.tools_id.as_deref().unwrap(),
        )
        .await?;
        assert_ne!(clone_tools.tools_id, source_tools.tools_id);
        let mut copied_tools = clone_tools.clone();
        copied_tools.tools_id = source_tools.tools_id.clone();
        assert_eq!(copied_tools, source_tools);

        // A sparse rename carries no profile/root/prompt values. The signed
        // edit mask makes those omissions preserve the canonical chain.
        doc.request_key = "rename".into();
        doc.op = Some(PersonaOp::Edit);
        doc.behavior_id = Some(cloned.behavior_id.clone());
        doc.persona_name = Some("Renamed clone".into());
        doc.description = None;
        doc.system_prompt = None;
        doc.root = None;
        doc.profile_id = None;
        doc.edit_fields = vec!["display_name".into()];
        apply_persona_request(&node, test_actor(), &doc, &catalog).await?;
        let renamed: AgentBehaviorDocument =
            read(&node, Collection::AgentBehavior, owner, &cloned.behavior_id).await?;
        assert_eq!(renamed.display_name.as_deref(), Some("Renamed clone"));
        assert_eq!(renamed.inference_profile_id, "profile-2");
        assert_eq!(renamed.context_id, behavior.context_id);
        let renamed_context: AgentContext = read(
            &node,
            Collection::AgentContext,
            owner,
            renamed.context_id.as_deref().unwrap(),
        )
        .await?;
        assert_eq!(renamed_context.system_prompt, clone_context.system_prompt);
        let renamed_tools: Tools = read(
            &node,
            Collection::Tools,
            owner,
            renamed_context.tools_id.as_deref().unwrap(),
        )
        .await?;
        assert_eq!(
            renamed_tools
                .host
                .as_ref()
                .and_then(|host| host.root.as_deref()),
            Some("/workspace/root/original")
        );

        // Replacing only the permission preset preserves the existing root.
        doc.request_key = "preset-preserves-root".into();
        doc.preset = Some("readonly".into());
        doc.edit_fields = vec!["preset".into()];
        apply_persona_request(&node, test_actor(), &doc, &catalog).await?;
        let preset_behavior: AgentBehaviorDocument =
            read(&node, Collection::AgentBehavior, owner, &cloned.behavior_id).await?;
        let preset_context: AgentContext = read(
            &node,
            Collection::AgentContext,
            owner,
            preset_behavior.context_id.as_deref().unwrap(),
        )
        .await?;
        let preset_tools: Tools = read(
            &node,
            Collection::Tools,
            owner,
            preset_context.tools_id.as_deref().unwrap(),
        )
        .await?;
        assert_eq!(
            preset_tools
                .host
                .as_ref()
                .and_then(|host| host.root.as_deref()),
            Some("/workspace/root/original")
        );

        // Root clearing and preset replacement must not mutate a shared source.
        doc.request_key = "edit".into();
        doc.root = None;
        doc.description = Some("Edited behavior and context".into());
        doc.system_prompt = Some("Edited literal instructions".into());
        doc.edit_fields = vec!["description".into(), "system_prompt".into(), "root".into()];
        apply_persona_request(&node, test_actor(), &doc, &catalog).await?;
        let edited: AgentBehaviorDocument =
            read(&node, Collection::AgentBehavior, owner, &cloned.behavior_id).await?;
        let edited_context: AgentContext = read(
            &node,
            Collection::AgentContext,
            owner,
            edited.context_id.as_deref().unwrap(),
        )
        .await?;
        let edited_tools: Tools = read(
            &node,
            Collection::Tools,
            owner,
            edited_context.tools_id.as_deref().unwrap(),
        )
        .await?;
        assert_eq!(edited_tools.host.as_ref().unwrap().root, None);
        assert_eq!(
            edited_context.description.as_deref(),
            Some("Edited behavior and context")
        );
        assert_eq!(
            edited_context.system_prompt.as_deref(),
            Some("Edited literal instructions")
        );
        assert_eq!(
            read::<Tools>(&node, Collection::Tools, owner, &source_tools.tools_id).await?,
            source_tools
        );
        doc.request_key = "preset".into();
        doc.preset = Some("readonly".into());
        doc.edit_fields = vec!["preset".into()];
        apply_persona_request(&node, test_actor(), &doc, &catalog).await?;
        let preset: AgentBehaviorDocument =
            read(&node, Collection::AgentBehavior, owner, &cloned.behavior_id).await?;
        let preset_context: AgentContext = read(
            &node,
            Collection::AgentContext,
            owner,
            preset.context_id.as_deref().unwrap(),
        )
        .await?;
        assert_eq!(preset_context.system_prompt, edited_context.system_prompt);
        let preset_tools: Tools = read(
            &node,
            Collection::Tools,
            owner,
            preset_context.tools_id.as_deref().unwrap(),
        )
        .await?;
        assert_eq!(
            preset_tools.host.unwrap().files.unwrap().mode,
            crate::tool_surface::FileToolMode::ReadOnly
        );
        doc.op = Some(PersonaOp::Disable);
        doc.edit_fields.clear();
        apply_persona_request(&node, test_actor(), &doc, &catalog).await?;
        let mut disabled: AgentBehaviorDocument =
            read(&node, Collection::AgentBehavior, owner, &cloned.behavior_id).await?;
        assert!(!disabled.enabled);
        disabled.enabled = true;
        assert_eq!(disabled, preset);
        Ok(())
    }

    #[tokio::test]
    async fn failed_foreign_profile_rolls_back_persona_documents() -> Result<()> {
        let node = Arc::new(EmbeddedNode::builder().build().await?);
        crate::ensure_runtime_schemas(&node).await?;
        seed_persona_validation_references(&node, "did:key:foreign").await?;
        crate::document_config::ensure_agent_principal(&node, "did:key:agent").await?;
        let doc = create_doc(PersonaOp::Create { clone_from: None });
        assert!(
            apply_persona_request(&node, test_actor(), &doc, &base_catalog())
                .await
                .is_err()
        );
        let result = node
            .execute("{ AgentBehavior {behavior_id} AgentContext {context_id} Tools {tools_id} }")
            .await;
        assert!(!result.has_errors(), "{:?}", result.errors);
        for name in ["AgentBehavior", "AgentContext", "Tools"] {
            assert_eq!(result.data.as_ref().unwrap()[name], serde_json::json!([]));
        }
        Ok(())
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn apply_rechecks_root_after_admission_before_persistence() -> Result<()> {
        let node = Arc::new(EmbeddedNode::builder().build().await?);
        crate::ensure_runtime_schemas(&node).await?;
        let owner = "did:key:agent";
        seed_persona_validation_references(&node, owner).await?;
        let allowed = tempfile::tempdir()?;
        let inside = allowed.path().join("inside");
        std::fs::create_dir_all(&inside)?;
        let outside = tempfile::tempdir()?;
        let link = allowed.path().join("selection");
        std::os::unix::fs::symlink(&inside, &link)?;
        let catalog = catalog_with(
            &[allowed.path().to_str().context("utf-8 root")?],
            &["profile-1"],
            &[],
        );
        let mut doc = create_doc(PersonaOp::Create { clone_from: None });
        doc.root = Some(link.to_string_lossy().into_owned());
        assert_eq!(
            decide_persona_request(&doc, &catalog),
            PersonaVerdict::Admit
        );

        std::fs::remove_file(&link)?;
        std::os::unix::fs::symlink(outside.path(), &link)?;
        let error = apply_persona_request(&node, test_actor(), &doc, &catalog)
            .await
            .expect_err("changed symlink must fail the publication-boundary check");
        assert!(
            error
                .to_string()
                .contains("rejected immediately before persistence"),
            "{error:#}"
        );
        let response = node.execute("{ AgentBehavior {behavior_id} }").await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        assert_eq!(
            response.data.as_ref().expect("response data")["AgentBehavior"],
            serde_json::json!([])
        );
        Ok(())
    }

    #[tokio::test]
    async fn apply_reloads_workspace_root_policy_inside_transaction() -> Result<()> {
        let node = Arc::new(EmbeddedNode::builder().build().await?);
        crate::ensure_runtime_schemas(&node).await?;
        let owner = "did:key:agent";
        seed_persona_validation_references(&node, owner).await?;
        let ceiling = tempfile::tempdir()?;
        let selected = ceiling.path().join("selected");
        std::fs::create_dir_all(&selected)?;
        let selected_text = selected.to_string_lossy().into_owned();
        let catalog = catalog_with(&[&selected_text], &["profile-1"], &[]);
        let mut doc = create_doc(PersonaOp::Create { clone_from: None });
        doc.root = Some(selected_text.clone());
        assert_eq!(
            decide_persona_request(&doc, &catalog),
            PersonaVerdict::Admit,
            "preflight sees the stale enabled catalog"
        );

        let escaped = crate::graphql::escape_graphql_string(&selected_text);
        ConfigAccess::write_local(
            &node,
            "test.persona.revoked_root",
            &format!(
                r#"mutation {{ create_WorkspaceRoot(input: {{root_path:"{escaped}", enabled:false}}) {{_docID}} }}"#
            ),
        )
        .await?;

        let error = apply_persona_request(&node, test_actor(), &doc, &catalog)
            .await
            .expect_err("the transaction-local policy reload must observe revocation");
        assert!(
            error
                .to_string()
                .contains("rejected immediately before persistence"),
            "{error:#}"
        );
        let response = node.execute("{ AgentBehavior {behavior_id} }").await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        assert_eq!(
            response.data.as_ref().expect("response data")["AgentBehavior"],
            serde_json::json!([])
        );
        Ok(())
    }
}
