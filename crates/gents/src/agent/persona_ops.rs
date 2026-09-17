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
use crate::behavior_scope::{
    behavior_slug_from_display_name, personal_behavior_key_candidates, reserved_behavior_slots,
};
use crate::config_client::{
    apply_desired_state_plan, materialize_behavior_closure_candidate_in_txn,
    read_desired_state_document_in_txn, read_desired_state_record_in_txn, ConfigAccess,
    ConfigApplyTxn, DesiredStateApplyDocument, DesiredStateApplyPlan,
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
    /// Published cwd choices within the authorized principal scope. An empty
    /// requested root is always fine regardless of this set (it means "no
    /// root restriction"), so this set only gates *non-empty* requests.
    pub allowed_roots: BTreeSet<String>,
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
        return None;
    }
    if !catalog.allowed_roots.contains(root) {
        return Some(format!(
            r#"root "{root}" is not allowed — pick from the published allowed_roots: {}"#,
            enumerate_bounded(&catalog.allowed_roots)
        ));
    }
    None
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
    behavior_slug_from_display_name(input)
}

/// Derive a preview personal behavior ID. Transactional publication performs
/// the stronger allocation across every reserved component slot.
pub fn derive_behavior_id(
    _agent_did: &str,
    persona_name: &str,
    existing: &BTreeMap<String, BehaviorRef>,
) -> String {
    let slug = slugify(persona_name);
    let base = format!("local:{slug}");
    if !existing.contains_key(&base) {
        return base;
    }
    let mut n = 2;
    loop {
        let candidate = format!("local:{slug}-{n}");
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
        scope_behavior_id: None,
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
    /// The request receipt was published atomically with this configuration.
    pub receipt_written: bool,
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
    doc: &PersonaRequestDoc,
    _catalog: &PersonaCatalogView,
) -> Result<PersonaApplyOutcome> {
    ConfigAccess::Local(node.clone()).transact("persona.apply", |txn| Box::pin(async move {
        let op = doc.op.as_ref().context("persona operation missing")?;
        let owner = &doc.agent_did;
        let mut receipt_row = false;
        if !doc.doc_id.trim().is_empty() {
            let response = txn.execute(&format!(
                "{{ PersonaConfigRequest(filter: {{_docID: {{_eq: \"{}\"}}}}, limit: 2) {{ _docID request_key agent_did status applied_behavior_id }} }}",
                crate::graphql::escape_graphql_string(&doc.doc_id)
            )).await?;
            let rows = gents_protocol::graphql::graphql_rows_from_response(&response, "PersonaConfigRequest");
            anyhow::ensure!(rows.len() <= 1, "ambiguous persona request receipt");
            if let Some(row) = rows.first() {
                anyhow::ensure!(row.get("request_key").and_then(serde_json::Value::as_str) == Some(doc.request_key.as_str()), "persona receipt request key changed");
                anyhow::ensure!(row.get("agent_did").and_then(serde_json::Value::as_str) == Some(owner.as_str()), "persona receipt principal changed");
                receipt_row = true;
                if let Some(behavior_id) = row.get("applied_behavior_id").and_then(serde_json::Value::as_str).filter(|id| !id.is_empty()) {
                    anyhow::ensure!(
                        row.get("status").and_then(serde_json::Value::as_str) == Some("applied"),
                        "persona receipt has an applied behavior without applied status"
                    );
                    anyhow::ensure!(
                        read_desired_state_record_in_txn(txn, Collection::AgentBehavior, owner, behavior_id).await?.is_some(),
                        "persona receipt references missing behavior {behavior_id:?} for principal {owner:?}"
                    );
                    return Ok(PersonaApplyOutcome { behavior_id: behavior_id.to_owned(), repaired: true, receipt_written: true });
                }
            }
        }
        // Use the same principal-scoped canonical codec as the desired-state loader.
        let (fields, _) = crate::config_client::config_projection(Collection::AgentBehavior, None)?;
        let response = txn.execute(&format!("{{ AgentBehavior(filter: {{agent_did: {{_eq: \"{}\"}}}}) {{ {} }} }}",
            crate::graphql::escape_graphql_string(owner), fields.join(" "))).await?;
        let mut current = BTreeMap::<String, AgentBehaviorDocument>::new();
        for value in gents_protocol::graphql::graphql_rows_from_response(&response, "AgentBehavior") {
            let behavior: AgentBehaviorDocument = serde_json::from_value(value)?;
            anyhow::ensure!(current.insert(behavior.behavior_id.clone(), behavior).is_none(), "ambiguous persona behavior within principal");
        }
        let source_id = match op {
            PersonaOp::Create {clone_from} => clone_from.as_deref(),
            PersonaOp::Edit | PersonaOp::Disable => Some(doc.behavior_id.as_deref().context("persona behavior missing")?),
        };
        let source = source_id.map(|id| current.get(id).cloned().with_context(|| format!("persona behavior {id} missing"))).transpose()?;
        if matches!(op, PersonaOp::Create {clone_from:Some(_)}) {
            anyhow::ensure!(source.as_ref().is_some_and(|source| source.enabled), "clone source is disabled");
        }
        if matches!(op, PersonaOp::Edit | PersonaOp::Disable) {
            anyhow::ensure!(
                source.as_ref().is_none_or(|source| {
                    source.behavior_id
                        != crate::behavior_scope::SETUP_CONFIGURATOR_BEHAVIOR_ID
                        && !source
                            .tags
                            .iter()
                            .any(|tag| tag == SETUP_STEWARD_BEHAVIOR_TAG)
                }),
                "protected configurator behavior cannot be modified"
            );
        }
        let create = matches!(op, PersonaOp::Create {..});
        let target_behavior_id = if create {
            let slug = slugify(doc.persona_name.as_deref().context("persona name missing")?);
            let mut selected = None;
            for candidate in personal_behavior_key_candidates(&slug)? {
                let mut occupied = false;
                for slot in reserved_behavior_slots(&candidate) {
                    if read_desired_state_record_in_txn(txn, slot.collection, owner, &slot.logical_id).await?.is_some() {
                        occupied = true;
                        break;
                    }
                }
                if !occupied {
                    selected = Some(candidate);
                    break;
                }
            }
            selected.context("personal behavior ID space exhausted")?
        } else {
            doc.behavior_id.clone().context("persona behavior missing")?
        };
        let mut behavior = if let Some(source) = source.clone() { source } else {
            serde_json::from_value(serde_json::json!({"behavior_id":target_behavior_id, "agent_did":owner,
                "inference_profile_id":doc.profile_id.as_deref().context("persona profile missing")?}))?
        };
        if matches!(op, PersonaOp::Disable) {
            behavior.enabled = false;
            apply_desired_state_plan(txn, &DesiredStateApplyPlan::new(vec![replacement(Collection::AgentBehavior, &behavior)?])?).await?;
            if doc.make_default {
                anyhow::bail!("disabled behavior cannot be the default");
            }
            write_persona_receipt(txn, doc, receipt_row, &behavior.behavior_id).await?;
            return Ok(PersonaApplyOutcome {behavior_id:behavior.behavior_id,repaired:false,receipt_written:receipt_row});
        }
        if create {
            behavior.behavior_id = target_behavior_id.clone();
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
        if source.is_none() || doc.edits("profile_id") || (create && doc.profile_id.is_some()) {
            let profile = doc.profile_id.as_deref().context("behavior profile_id cannot be cleared")?;
            anyhow::ensure!(!profile.trim().is_empty(), "behavior profile_id must not be blank");
            behavior.inference_profile_id = profile.into();
        }
        let source_context_id = source.as_ref().and_then(|b| b.context_id.clone());
        let need_context = source_context_id.is_some() || source.is_none() || doc.system_prompt.is_some()
            || doc.description.is_some() || doc.edits("system_prompt") || doc.edits("description")
            || doc.edits("root") || doc.edits("preset");
        let mut context: Option<AgentContext> = match source_context_id.as_deref() {
            Some(id) => Some(load_config(txn, Collection::AgentContext, owner, id).await?),
            None if need_context => Some(serde_json::from_value(serde_json::json!({"context_id":format!("persona-source:{}:context", doc.request_key),"agent_did":owner}))?),
            None => None,
        };
        let existing_tools: Option<Tools> = match context.as_ref().and_then(|context| context.tools_id.as_deref()) {
            Some(id) => Some(load_config(txn, Collection::Tools, owner, id).await?), None => None,
        };
        if create {
            if let Some(system_prompt) = &doc.system_prompt {
                if let Some(context) = &mut context { context.system_prompt = Some(system_prompt.clone()); }
            }
        } else if doc.edits("system_prompt") {
            if let Some(context) = &mut context { context.system_prompt = doc.system_prompt.clone(); }
        }
        // The command exposes one concise description because Behavior and
        // Context are materialized as one reusable interface. Keep both
        // canonical owners coherent instead of leaving the context opaque in
        // later inspect/edit flows.
        if source.is_none() || doc.edits("description") || (create && doc.description.is_some()) {
            if let Some(context) = &mut context { context.description = doc.description.clone(); }
        }
        let root = doc.root.as_ref().filter(|root| !root.trim().is_empty()).cloned();
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
                existing_tools.as_ref().map(|tools| tools.tools_id.clone()).unwrap_or_else(|| format!("persona-source:{}:tools", doc.request_key)),
                owner,
                effective_name,
                preset,
                preset_root,
            )?)
        } else { existing_tools.clone() };
        // Omitted edit fields preserve their canonical values. A present root
        // with a null payload explicitly clears only the root narrowing.
        if root.is_some() || (!create && doc.edits("root")) {
            if tools.is_none() && root.is_some() {
                tools = Some(Tools {
                    tools_id: format!("persona-source:{}:tools", doc.request_key),
                    agent_did: owner.clone(),
                    scope_behavior_id: None,
                    ..Default::default()
                });
            }
            if let Some(tools) = &mut tools {
                if let Some(host) = &mut tools.host { host.root = root.clone(); }
                else if root.is_some() { tools.host = Some(HostTools {root:root.clone(),..Default::default()}); }
            }
        }
        let mut overlays = Vec::new();
        if let Some(mut context) = context {
            if let Some(tools) = tools {
                context.tools_id = Some(tools.tools_id.clone());
                overlays.push((Collection::Tools, serde_json::to_value(tools)?));
            } else { context.tools_id = None; }
            behavior.context_id = Some(context.context_id.clone());
            overlays.push((Collection::AgentContext, serde_json::to_value(context)?));
        } else {
            behavior.context_id = None;
        }
        materialize_behavior_closure_candidate_in_txn(
            txn,
            &behavior,
            overlays,
            &target_behavior_id,
            behavior.display_name.as_deref(),
        ).await?;
        if doc.make_default {
            let mut principal: AgentPrincipal =
                load_config(txn, Collection::AgentPrincipal, owner, owner).await?;
            principal.default_behavior_id = Some(target_behavior_id.clone());
            apply_desired_state_plan(txn, &DesiredStateApplyPlan::new(vec![replacement(Collection::AgentPrincipal, &principal)?])?).await?;
        }
        write_persona_receipt(txn, doc, receipt_row, &target_behavior_id).await?;
        Ok(PersonaApplyOutcome {behavior_id:target_behavior_id,repaired:false,receipt_written:receipt_row})
    })).await
}

async fn write_persona_receipt(
    txn: &ConfigApplyTxn<'_>,
    doc: &PersonaRequestDoc,
    receipt_row: bool,
    behavior_id: &str,
) -> Result<()> {
    if !receipt_row {
        return Ok(());
    }
    let processed_at = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
    let response = txn.execute(&format!(
        "mutation {{ update_PersonaConfigRequest(filter: {{_docID: {{_eq: \"{}\"}}}}, input: {{status: \"applied\", status_detail: \"\", applied_behavior_id: \"{}\", processed_at: \"{}\"}}) {{ _docID }} }}",
        crate::graphql::escape_graphql_string(&doc.doc_id),
        crate::graphql::escape_graphql_string(behavior_id),
        crate::graphql::escape_graphql_string(&processed_at),
    )).await?;
    let rows = gents_protocol::graphql::graphql_rows_from_response(
        &response,
        "update_PersonaConfigRequest",
    );
    anyhow::ensure!(
        rows.len() == 1,
        "persona request receipt disappeared during apply"
    );
    Ok(())
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

    fn catalog_with(
        roots: &[&str],
        profiles: &[&str],
        behaviors: &[(&str, bool, &str)],
    ) -> PersonaCatalogView {
        PersonaCatalogView {
            allowed_roots: roots.iter().map(|s| s.to_string()).collect(),
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
            root: None,
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
        let outcome = apply_persona_request(&node, &doc, &base_catalog()).await?;
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
        let mut doc = create_doc(PersonaOp::Create { clone_from: None });
        doc.root = Some("/not/allowed".to_string());
        let verdict = decide_persona_request(&doc, &base_catalog());
        assert_eq!(
            verdict,
            PersonaVerdict::Reject(
                r#"root "/not/allowed" is not allowed — pick from the published allowed_roots: [/workspace/root]"#
                    .to_string()
            )
        );
    }

    #[test]
    fn empty_root_is_admitted() {
        let mut doc = create_doc(PersonaOp::Create { clone_from: None });
        doc.root = Some("".to_string());
        let verdict = decide_persona_request(&doc, &base_catalog());
        assert_eq!(verdict, PersonaVerdict::Admit);
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
        assert_eq!(id, "local:research-assistant");
    }

    #[test]
    fn derives_stable_nonempty_slug_for_unicode_only_name() {
        let first = derive_behavior_id("did:key:agent", "研究員", &BTreeMap::new());
        let second = derive_behavior_id("did:key:other", "研究員", &BTreeMap::new());
        assert_eq!(first, second);
        assert_eq!(first, "local:behavior");
    }

    #[test]
    fn derives_collision_suffix() {
        let mut existing = BTreeMap::new();
        existing.insert(
            "local:research-assistant".to_string(),
            BehaviorRef {
                enabled: true,
                protected: false,
            },
        );
        let id = derive_behavior_id("did:key:agent", "Research Assistant", &existing);
        assert_eq!(id, "local:research-assistant-2");

        existing.insert(
            "local:research-assistant-2".to_string(),
            BehaviorRef {
                enabled: true,
                protected: false,
            },
        );
        let id = derive_behavior_id("did:key:agent", "Research Assistant", &existing);
        assert_eq!(id, "local:research-assistant-3");
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
    async fn create_suffixes_when_a_reserved_component_slot_is_occupied() -> Result<()> {
        let node = Arc::new(EmbeddedNode::builder().build().await?);
        crate::ensure_runtime_schemas(&node).await?;
        let owner = "did:key:agent";
        seed_persona_validation_references(&node, owner).await?;
        let response = node
            .execute(&format!(
                r#"mutation {{ create_AgentContext(input: {{context_id:"local:research-assistant:context",agent_did:"{}"}}) {{_docID}} }}"#,
                crate::graphql::escape_graphql_string(owner)
            ))
            .await;
        crate::graphql::ensure_no_errors(&response, "seed reserved context slot")?;

        let outcome = apply_persona_request(
            &node,
            &create_doc(PersonaOp::Create { clone_from: None }),
            &base_catalog(),
        )
        .await?;
        assert_eq!(outcome.behavior_id, "local:research-assistant-2");
        Ok(())
    }

    #[tokio::test]
    async fn canonical_persona_create_clone_edit_disable_and_atomic_replay() -> Result<()> {
        let node = Arc::new(EmbeddedNode::builder().build().await?);
        crate::ensure_runtime_schemas(&node).await?;
        let owner = "did:key:agent";
        seed_persona_validation_references(&node, owner).await?;
        let mut doc = create_doc(PersonaOp::Create { clone_from: None });
        doc.root = Some("/original".into());
        let catalog = base_catalog();
        let created = apply_persona_request(&node, &doc, &catalog).await?;
        assert_eq!(created.behavior_id, "local:research-assistant");
        let original: AgentBehaviorDocument = read(
            &node,
            Collection::AgentBehavior,
            owner,
            &created.behavior_id,
        )
        .await?;
        assert_eq!(
            original.inference_profile_id,
            "local:research-assistant:inference"
        );
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
        let cloned = apply_persona_request(&node, &doc, &catalog).await?;
        let behavior: AgentBehaviorDocument =
            read(&node, Collection::AgentBehavior, owner, &cloned.behavior_id).await?;
        assert_eq!(behavior.inference_profile_id, "local:cloned:inference");
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
        copied_tools.scope_behavior_id = source_tools.scope_behavior_id.clone();
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
        apply_persona_request(&node, &doc, &catalog).await?;
        let renamed: AgentBehaviorDocument =
            read(&node, Collection::AgentBehavior, owner, &cloned.behavior_id).await?;
        assert_eq!(renamed.display_name.as_deref(), Some("Renamed clone"));
        assert_eq!(renamed.inference_profile_id, "local:cloned:inference");
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
            Some("/original")
        );

        // Replacing only the permission preset preserves the existing root.
        doc.request_key = "preset-preserves-root".into();
        doc.preset = Some("readonly".into());
        doc.edit_fields = vec!["preset".into()];
        apply_persona_request(&node, &doc, &catalog).await?;
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
            Some("/original")
        );

        // Root clearing and preset replacement must not mutate a shared source.
        doc.request_key = "edit".into();
        doc.root = None;
        doc.description = Some("Edited behavior and context".into());
        doc.system_prompt = Some("Edited literal instructions".into());
        doc.edit_fields = vec!["description".into(), "system_prompt".into(), "root".into()];
        apply_persona_request(&node, &doc, &catalog).await?;
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
        apply_persona_request(&node, &doc, &catalog).await?;
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
        apply_persona_request(&node, &doc, &catalog).await?;
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
        assert!(apply_persona_request(&node, &doc, &base_catalog())
            .await
            .is_err());
        let result = node
            .execute("{ AgentBehavior {behavior_id} AgentContext {context_id} Tools {tools_id} }")
            .await;
        assert!(!result.has_errors(), "{:?}", result.errors);
        for name in ["AgentBehavior", "AgentContext", "Tools"] {
            assert_eq!(result.data.as_ref().unwrap()[name], serde_json::json!([]));
        }
        Ok(())
    }
}
