//! Pure `AgentRequest` admission projection shared by conformance and runtime.
//!
//! Cryptographic and durable-state checks produce these observations.  This
//! projector never treats a caller-authored tag or lineage field as evidence.

use crate::canonical::{
    parse_utc_seconds, require_enum, require_identifier, require_optional_identifier,
};
use crate::request_lifecycle::RequestLifecycleState;
use serde::{Deserialize, Serialize};

const REQUEST_SIGNATURE_DOMAIN: &str = "gents-agent-request-admission-v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentRequestAdmissionKind {
    Enrollment,
    LocalSelf,
    RuntimeInternal,
}

impl AgentRequestAdmissionKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Enrollment => "enrollment",
            Self::LocalSelf => "local-self",
            Self::RuntimeInternal => "runtime-internal",
        }
    }
}

impl TryFrom<&str> for AgentRequestAdmissionKind {
    type Error = &'static str;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "enrollment" => Ok(Self::Enrollment),
            "local-self" => Ok(Self::LocalSelf),
            "runtime-internal" => Ok(Self::RuntimeInternal),
            _ => Err("unknown AgentRequest admission kind"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RuntimeInternalSourceKind {
    LocalChild,
    CrossPrincipalChild,
    LocalControl,
    AutomatedTrigger,
}

impl RuntimeInternalSourceKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LocalChild => "local-child",
            Self::CrossPrincipalChild => "cross-principal-child",
            Self::LocalControl => "local-control",
            Self::AutomatedTrigger => "automated-trigger",
        }
    }
}

impl TryFrom<&str> for RuntimeInternalSourceKind {
    type Error = &'static str;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "local-child" => Ok(Self::LocalChild),
            "cross-principal-child" => Ok(Self::CrossPrincipalChild),
            "local-control" => Ok(Self::LocalControl),
            "automated-trigger" => Ok(Self::AutomatedTrigger),
            _ => Err("unknown runtime-internal source kind"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentRequestAdmissionObservation {
    pub kind: AgentRequestAdmissionKind,
    pub signature_valid: bool,
    pub signed_fields_match: bool,
    pub branch_fields_exact: bool,
    pub pending_deadline_absent: bool,
    pub signer_matches_requester: bool,
    pub requester_matches_target: bool,
    pub signer_matches_target: bool,
    pub signer_matches_issuer: bool,
    pub requester_matches_issuer: bool,
    pub current_approval: bool,
    pub exact_generation: bool,
    pub authorization_fresh: bool,
    pub runtime_evidence_present: bool,
    pub runtime_source_kind: RuntimeInternalSourceKind,
    pub target_runtime_attestation_valid: bool,
    pub source_binding_current: bool,
    pub trigger_config_document_binding_current: bool,
    pub source_document_binding_current: bool,
    pub source_tool_call_binding_current: bool,
    pub target_policy_allows: bool,
    pub bridge_author_binding_current: bool,
    pub bridge_author_authorization_fresh: bool,
    pub target_cross_principal_policy_allows: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentRequestAdmissionDisposition {
    Admit,
    Deny,
    Retry,
}

impl AgentRequestAdmissionDisposition {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Admit => "admit",
            Self::Deny => "deny",
            Self::Retry => "retry",
        }
    }
}

pub fn project_agent_request_admission(observation: AgentRequestAdmissionObservation) -> bool {
    if !observation.signature_valid
        || !observation.signed_fields_match
        || !observation.branch_fields_exact
        || !observation.pending_deadline_absent
    {
        return false;
    }
    match observation.kind {
        AgentRequestAdmissionKind::Enrollment => {
            observation.signer_matches_requester
                && observation.current_approval
                && observation.exact_generation
                && observation.authorization_fresh
        }
        AgentRequestAdmissionKind::LocalSelf => {
            observation.signer_matches_requester && observation.requester_matches_target
        }
        AgentRequestAdmissionKind::RuntimeInternal => {
            let common = observation.runtime_evidence_present
                && observation.signer_matches_issuer
                && observation.requester_matches_issuer
                && observation.signer_matches_target
                && observation.requester_matches_target
                && observation.target_runtime_attestation_valid
                && observation.source_binding_current;
            common
                && match observation.runtime_source_kind {
                    RuntimeInternalSourceKind::LocalChild => {
                        observation.source_document_binding_current
                            && observation.source_tool_call_binding_current
                            && observation.target_policy_allows
                    }
                    RuntimeInternalSourceKind::CrossPrincipalChild => {
                        observation.source_tool_call_binding_current
                            && observation.bridge_author_binding_current
                            && observation.bridge_author_authorization_fresh
                            && observation.target_cross_principal_policy_allows
                    }
                    RuntimeInternalSourceKind::LocalControl => {
                        observation.source_document_binding_current
                    }
                    RuntimeInternalSourceKind::AutomatedTrigger => {
                        observation.trigger_config_document_binding_current
                            && observation.target_policy_allows
                    }
                }
        }
    }
}

pub fn project_agent_request_admission_disposition(
    observation_available: bool,
    observation: AgentRequestAdmissionObservation,
) -> AgentRequestAdmissionDisposition {
    if !observation_available {
        AgentRequestAdmissionDisposition::Retry
    } else if project_agent_request_admission(observation) {
        AgentRequestAdmissionDisposition::Admit
    } else {
        AgentRequestAdmissionDisposition::Deny
    }
}

/// Immutable request semantics. Mutable lifecycle/claim/terminal fields are
/// intentionally excluded because they change after admission.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentRequestSigningFields<'a> {
    pub request_id: &'a str,
    pub agent_did: &'a str,
    pub requester_did: Option<&'a str>,
    pub behavior_id: &'a str,
    pub session_id: &'a str,
    pub retry_parent_request: Option<&'a str>,
    pub retry_parent_request_doc_id: Option<&'a str>,
    pub retry_root_request: Option<&'a str>,
    pub retry_key: Option<&'a str>,
    pub content: &'a str,
    pub input: &'a crate::request_input::RequestInput,
    pub execution_origin: Option<&'a str>,
    pub caused_by_trigger_id: Option<&'a str>,
    pub caused_by_trigger_doc_id: Option<&'a str>,
    pub caused_by_trigger_kind: Option<&'a str>,
    pub caused_by_correlation: Option<&'a str>,
    pub caused_by_trigger_context: Option<&'a str>,
    pub caused_by_source_doc_id: Option<&'a str>,
    pub created_at: &'a str,
    pub retry_count: Option<i64>,
    pub max_retries: Option<i64>,
    pub valid_until: Option<&'a str>,
    pub subagent_depth: u32,
    pub caused_by_parent_request_id: Option<&'a str>,
    pub caused_by_parent_request_doc_id: Option<&'a str>,
    pub caused_by_parent_tool_call_id: Option<&'a str>,
    pub caused_by_parent_tool_call_doc_id: Option<&'a str>,
    pub workspace_id: Option<&'a str>,
    /// Principal scope copied from the verified workspace/source, not a host identity.
    pub workspace_owner_agent_did: Option<&'a str>,
    pub workspace_authority: Option<&'a str>,
    pub workspace_seal_hash: Option<&'a str>,
}

pub fn validate_signing_fields(request: &AgentRequestSigningFields<'_>) -> anyhow::Result<()> {
    for (name, value) in [
        ("request_id", request.request_id),
        ("agent_did", request.agent_did),
        ("session_id", request.session_id),
    ] {
        require_identifier(name, value)?;
    }
    // Behavior selection is required even for new sessions.
    require_identifier("behavior_id", request.behavior_id)?;
    for (name, value) in [
        ("requester_did", request.requester_did),
        ("retry_parent_request", request.retry_parent_request),
        (
            "retry_parent_request_doc_id",
            request.retry_parent_request_doc_id,
        ),
        ("retry_root_request", request.retry_root_request),
        ("retry_key", request.retry_key),
        ("caused_by_trigger_id", request.caused_by_trigger_id),
        ("caused_by_trigger_doc_id", request.caused_by_trigger_doc_id),
        ("caused_by_source_doc_id", request.caused_by_source_doc_id),
        (
            "caused_by_parent_request_id",
            request.caused_by_parent_request_id,
        ),
        (
            "caused_by_parent_request_doc_id",
            request.caused_by_parent_request_doc_id,
        ),
        (
            "caused_by_parent_tool_call_id",
            request.caused_by_parent_tool_call_id,
        ),
        (
            "caused_by_parent_tool_call_doc_id",
            request.caused_by_parent_tool_call_doc_id,
        ),
        ("workspace_id", request.workspace_id),
    ] {
        require_optional_identifier(name, value)?;
    }
    let origin = request
        .execution_origin
        .ok_or_else(|| anyhow::anyhow!("execution_origin is required"))?;
    require_enum("execution_origin", origin, &["interactive", "scheduled"])?;
    if let Some(kind) = request.caused_by_trigger_kind {
        require_enum(
            "caused_by_trigger_kind",
            kind,
            &["manual", "event", "schedule", "subagent", "goal"],
        )?;
    }
    if let Some(authority) = request.workspace_authority {
        require_enum(
            "workspace_authority",
            authority,
            &["readOnly", "readWrite", "integrate"],
        )?;
    }
    parse_utc_seconds("created_at", request.created_at)?;
    if let Some(valid_until) = request.valid_until {
        parse_utc_seconds("valid_until", valid_until)?;
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRequestAdmissionRecord {
    pub kind: AgentRequestAdmissionKind,
    pub signer_did: String,
    pub signature: Vec<u8>,
    pub enrollment_request_id: Option<String>,
    pub enrollment_request_digest: Option<String>,
    pub enrollment_admin_did: Option<String>,
    pub enrollment_authorization_sequence: Option<u64>,
    pub enrollment_authorization_expires_at: Option<String>,
    pub runtime_issuer_did: Option<String>,
    pub runtime_source_request_id: Option<String>,
    pub runtime_source_kind: Option<RuntimeInternalSourceKind>,
    pub runtime_bridge_author_did: Option<String>,
}

impl AgentRequestAdmissionRecord {
    pub fn local_self(signer_did: impl Into<String>) -> Self {
        Self {
            kind: AgentRequestAdmissionKind::LocalSelf,
            signer_did: signer_did.into(),
            signature: Vec::new(),
            enrollment_request_id: None,
            enrollment_request_digest: None,
            enrollment_admin_did: None,
            enrollment_authorization_sequence: None,
            enrollment_authorization_expires_at: None,
            runtime_issuer_did: None,
            runtime_source_request_id: None,
            runtime_source_kind: None,
            runtime_bridge_author_did: None,
        }
    }

    fn runtime_internal(
        target_did: impl Into<String>,
        source_request_id: impl Into<String>,
        source_kind: RuntimeInternalSourceKind,
        bridge_author_did: Option<String>,
    ) -> Self {
        let target_did = target_did.into();
        Self {
            kind: AgentRequestAdmissionKind::RuntimeInternal,
            signer_did: target_did.clone(),
            signature: Vec::new(),
            enrollment_request_id: None,
            enrollment_request_digest: None,
            enrollment_admin_did: None,
            enrollment_authorization_sequence: None,
            enrollment_authorization_expires_at: None,
            runtime_issuer_did: Some(target_did),
            runtime_source_request_id: Some(source_request_id.into()),
            runtime_source_kind: Some(source_kind),
            runtime_bridge_author_did: bridge_author_did,
        }
    }

    pub fn runtime_local_child(
        target_did: impl Into<String>,
        source_request_id: impl Into<String>,
    ) -> Self {
        Self::runtime_internal(
            target_did,
            source_request_id,
            RuntimeInternalSourceKind::LocalChild,
            None,
        )
    }

    pub fn runtime_cross_principal_child(
        target_did: impl Into<String>,
        source_request_id: impl Into<String>,
        bridge_author_did: impl Into<String>,
    ) -> Self {
        Self::runtime_internal(
            target_did,
            source_request_id,
            RuntimeInternalSourceKind::CrossPrincipalChild,
            Some(bridge_author_did.into()),
        )
    }

    pub fn runtime_local_control(
        target_did: impl Into<String>,
        source_request_id: impl Into<String>,
    ) -> Self {
        Self::runtime_internal(
            target_did,
            source_request_id,
            RuntimeInternalSourceKind::LocalControl,
            None,
        )
    }

    pub fn runtime_automated_trigger(
        target_did: impl Into<String>,
        source_request_id: impl Into<String>,
    ) -> Self {
        Self::runtime_internal(
            target_did,
            source_request_id,
            RuntimeInternalSourceKind::AutomatedTrigger,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    pub fn enrollment(
        signer_did: impl Into<String>,
        request_id: impl Into<String>,
        request_digest: impl Into<String>,
        admin_did: impl Into<String>,
        authorization_sequence: u64,
        authorization_expires_at: impl Into<String>,
    ) -> Self {
        Self {
            kind: AgentRequestAdmissionKind::Enrollment,
            signer_did: signer_did.into(),
            signature: Vec::new(),
            enrollment_request_id: Some(request_id.into()),
            enrollment_request_digest: Some(request_digest.into()),
            enrollment_admin_did: Some(admin_did.into()),
            enrollment_authorization_sequence: Some(authorization_sequence),
            enrollment_authorization_expires_at: Some(authorization_expires_at.into()),
            runtime_issuer_did: None,
            runtime_source_request_id: None,
            runtime_source_kind: None,
            runtime_bridge_author_did: None,
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub fn from_wire_fields(
        kind: Option<&str>,
        signer_did: Option<&str>,
        signature: Option<&str>,
        enrollment_request_id: Option<&str>,
        enrollment_request_digest: Option<&str>,
        enrollment_admin_did: Option<&str>,
        enrollment_authorization_sequence: Option<i64>,
        enrollment_authorization_expires_at: Option<&str>,
        runtime_issuer_did: Option<&str>,
        runtime_source_request_id: Option<&str>,
        runtime_source_kind: Option<&str>,
        runtime_bridge_author_did: Option<&str>,
    ) -> Result<Self, &'static str> {
        let kind = kind
            .ok_or("request admission kind is missing")
            .and_then(AgentRequestAdmissionKind::try_from)?;
        let signature = signature
            .ok_or("request admission signature is missing")
            .and_then(|value| {
                bs58::decode(value)
                    .into_vec()
                    .map_err(|_| "request admission signature is not valid base58")
            })?;
        let sequence = enrollment_authorization_sequence
            .map(|value| {
                u64::try_from(value).map_err(|_| "negative enrollment authorization sequence")
            })
            .transpose()?;
        let signer_did = signer_did.unwrap_or_default();
        if signer_did.is_empty() || signer_did.trim() != signer_did {
            return Err("request admission signer DID is blank or non-canonical");
        }
        let record = Self {
            kind,
            signer_did: signer_did.to_string(),
            signature,
            enrollment_request_id: canonical_optional(enrollment_request_id)?,
            enrollment_request_digest: canonical_optional(enrollment_request_digest)?,
            enrollment_admin_did: canonical_optional(enrollment_admin_did)?,
            enrollment_authorization_sequence: sequence,
            enrollment_authorization_expires_at: canonical_optional(
                enrollment_authorization_expires_at,
            )?,
            runtime_issuer_did: canonical_optional(runtime_issuer_did)?,
            runtime_source_request_id: canonical_optional(runtime_source_request_id)?,
            runtime_source_kind: runtime_source_kind
                .map(RuntimeInternalSourceKind::try_from)
                .transpose()?,
            runtime_bridge_author_did: canonical_optional(runtime_bridge_author_did)?,
        };
        record.validate_branch_fields()?;
        Ok(record)
    }

    pub fn validate_branch_fields(&self) -> Result<(), &'static str> {
        if self.signer_did.trim().is_empty() || self.signature.len() != 64 {
            return Err("request admission signer/signature is missing or invalid");
        }
        let enrollment_present = [
            self.enrollment_request_id.as_deref(),
            self.enrollment_request_digest.as_deref(),
            self.enrollment_admin_did.as_deref(),
            self.enrollment_authorization_expires_at.as_deref(),
        ]
        .iter()
        .all(|value| value.is_some_and(|value| !value.trim().is_empty()))
            && self.enrollment_authorization_sequence.is_some();
        let enrollment_absent = self.enrollment_request_id.is_none()
            && self.enrollment_request_digest.is_none()
            && self.enrollment_admin_did.is_none()
            && self.enrollment_authorization_sequence.is_none()
            && self.enrollment_authorization_expires_at.is_none();
        let runtime_present = self
            .runtime_issuer_did
            .as_deref()
            .is_some_and(|value| !value.trim().is_empty())
            && self
                .runtime_source_request_id
                .as_deref()
                .is_some_and(|value| !value.trim().is_empty())
            && self.runtime_source_kind.is_some();
        let runtime_absent = self.runtime_issuer_did.is_none()
            && self.runtime_source_request_id.is_none()
            && self.runtime_source_kind.is_none()
            && self.runtime_bridge_author_did.is_none();
        match self.kind {
            AgentRequestAdmissionKind::Enrollment if enrollment_present && runtime_absent => Ok(()),
            AgentRequestAdmissionKind::LocalSelf if enrollment_absent && runtime_absent => Ok(()),
            AgentRequestAdmissionKind::RuntimeInternal
                if enrollment_absent
                    && runtime_present
                    && matches!(
                        (
                            self.runtime_source_kind,
                            self.runtime_bridge_author_did.as_deref()
                        ),
                        (
                            Some(RuntimeInternalSourceKind::CrossPrincipalChild),
                            Some(_)
                        ) | (Some(RuntimeInternalSourceKind::LocalChild), None)
                            | (Some(RuntimeInternalSourceKind::LocalControl), None)
                            | (Some(RuntimeInternalSourceKind::AutomatedTrigger), None)
                    ) =>
            {
                Ok(())
            }
            _ => Err("request admission branch fields are incomplete or mixed"),
        }
    }

    pub fn validate_canonical_fields(&self) -> anyhow::Result<()> {
        require_identifier("admission signer DID", &self.signer_did)?;
        for (name, value) in [
            (
                "enrollment_request_id",
                self.enrollment_request_id.as_deref(),
            ),
            (
                "enrollment_request_digest",
                self.enrollment_request_digest.as_deref(),
            ),
            ("enrollment_admin_did", self.enrollment_admin_did.as_deref()),
            ("runtime_issuer_did", self.runtime_issuer_did.as_deref()),
            (
                "runtime_source_request_id",
                self.runtime_source_request_id.as_deref(),
            ),
            (
                "runtime_bridge_author_did",
                self.runtime_bridge_author_did.as_deref(),
            ),
        ] {
            require_optional_identifier(name, value)?;
        }
        if let Some(expires) = self.enrollment_authorization_expires_at.as_deref() {
            parse_utc_seconds("enrollment_authorization_expires_at", expires)?;
        }
        Ok(())
    }

    pub fn signing_payload(&self, request: &AgentRequestSigningFields<'_>) -> Vec<u8> {
        let mut fields = Vec::new();
        push_text(&mut fields, REQUEST_SIGNATURE_DOMAIN);
        push_text(&mut fields, request.request_id);
        push_text(&mut fields, request.agent_did);
        push_option(&mut fields, request.requester_did);
        push_text(&mut fields, request.behavior_id);
        push_text(&mut fields, request.session_id);
        push_option(&mut fields, request.retry_parent_request);
        push_option(&mut fields, request.retry_parent_request_doc_id);
        push_option(&mut fields, request.retry_root_request);
        push_option(&mut fields, request.retry_key);
        push_text(&mut fields, request.content);
        // Typed invocation input, encoded by the canonical framing below; the
        // removed per-request sampling/metadata/deployment scalars are gone.
        push_request_input(&mut fields, request.input);
        push_option(&mut fields, request.execution_origin);
        push_option(&mut fields, request.caused_by_trigger_id);
        push_option(&mut fields, request.caused_by_trigger_doc_id);
        push_option(&mut fields, request.caused_by_trigger_kind);
        push_option(&mut fields, request.caused_by_correlation);
        push_option(&mut fields, request.caused_by_trigger_context);
        push_option(&mut fields, request.caused_by_source_doc_id);
        push_text(&mut fields, request.created_at);
        push_i64(&mut fields, request.retry_count);
        push_i64(&mut fields, request.max_retries);
        push_option(&mut fields, request.valid_until);
        push_text(&mut fields, &request.subagent_depth.to_string());
        push_option(&mut fields, request.caused_by_parent_request_id);
        push_option(&mut fields, request.caused_by_parent_request_doc_id);
        push_option(&mut fields, request.caused_by_parent_tool_call_id);
        push_option(&mut fields, request.caused_by_parent_tool_call_doc_id);
        push_option(&mut fields, request.workspace_id);
        push_option(&mut fields, request.workspace_authority);
        push_option(&mut fields, request.workspace_seal_hash);
        push_text(&mut fields, self.kind.as_str());
        push_text(&mut fields, &self.signer_did);
        push_option(&mut fields, self.enrollment_request_id.as_deref());
        push_option(&mut fields, self.enrollment_request_digest.as_deref());
        push_option(&mut fields, self.enrollment_admin_did.as_deref());
        push_u64(&mut fields, self.enrollment_authorization_sequence);
        push_option(
            &mut fields,
            self.enrollment_authorization_expires_at.as_deref(),
        );
        push_option(&mut fields, self.runtime_issuer_did.as_deref());
        push_option(&mut fields, self.runtime_source_request_id.as_deref());
        push_option(
            &mut fields,
            self.runtime_source_kind
                .map(RuntimeInternalSourceKind::as_str),
        );
        push_option(&mut fields, self.runtime_bridge_author_did.as_deref());
        serialize_fields(&fields)
    }
}

fn canonical_optional(value: Option<&str>) -> Result<Option<String>, &'static str> {
    value
        .map(|value| {
            if value.is_empty() || value.trim() != value {
                Err("request admission field is blank or non-canonical")
            } else {
                Ok(value.to_string())
            }
        })
        .transpose()
}

fn push_text(fields: &mut Vec<Vec<u8>>, value: &str) {
    fields.push(value.as_bytes().to_vec());
}

fn push_option(fields: &mut Vec<Vec<u8>>, value: Option<&str>) {
    match value {
        Some(value) => {
            fields.push(vec![1]);
            push_text(fields, value);
        }
        None => fields.push(vec![0]),
    }
}

fn push_i64(fields: &mut Vec<Vec<u8>>, value: Option<i64>) {
    push_option(
        fields,
        value.as_ref().map(|value| value.to_string()).as_deref(),
    );
}

fn push_u64(fields: &mut Vec<Vec<u8>>, value: Option<u64>) {
    push_option(
        fields,
        value.as_ref().map(|value| value.to_string()).as_deref(),
    );
}

/// Title provenance wire names shared by the canonical session title owner.
fn title_source_wire_name(source: crate::session::SessionTitleSource) -> &'static str {
    match source {
        crate::session::SessionTitleSource::Placeholder => "placeholder",
        crate::session::SessionTitleSource::Generated => "generated",
        crate::session::SessionTitleSource::Task => "task",
        crate::session::SessionTitleSource::User => "user",
    }
}

/// Queue enum wire names matching the canonical queue owners.
fn queue_source_wire_name(source: crate::request_input::QueueSource) -> &'static str {
    match source {
        crate::request_input::QueueSource::User => "user",
        crate::request_input::QueueSource::BackgroundCompletion => "background_completion",
        crate::request_input::QueueSource::Steering => "steering",
        crate::request_input::QueueSource::Goal => "goal",
    }
}

fn queue_policy_wire_name(policy: crate::request_input::QueuePolicy) -> &'static str {
    match policy {
        crate::request_input::QueuePolicy::Append => "append",
        crate::request_input::QueuePolicy::Coalesce => "coalesce",
    }
}

/// Canonical typed-input framing, mirroring the Lean
/// `Enrollment.requestInputFields` byte contract: explicit option tags,
/// collection lengths, and the fixed field order of RequestInput and
/// RequestQueue. This is the one canonical input encoder for signing and
/// verification; callers must never substitute unescaped JSON or metadata.
fn push_input_option(fields: &mut Vec<Vec<u8>>, value: Option<&str>) {
    match value {
        Some(value) => {
            push_text(fields, "some");
            push_text(fields, value);
        }
        None => push_text(fields, "none"),
    }
}

fn push_request_input(fields: &mut Vec<Vec<u8>>, input: &crate::request_input::RequestInput) {
    push_text(fields, &input.selected_skill_ids.len().to_string());
    for skill_id in &input.selected_skill_ids {
        push_text(fields, skill_id);
    }
    push_input_option(fields, input.cwd.as_deref());
    match &input.initial_title {
        Some(title) => {
            push_text(fields, "some");
            push_text(fields, &title.text);
            push_text(fields, title_source_wire_name(title.source));
        }
        None => push_text(fields, "none"),
    }
    match &input.queue {
        Some(queue) => {
            push_text(fields, "some");
            push_text(fields, queue_source_wire_name(queue.source));
            push_text(fields, queue_policy_wire_name(queue.policy));
            push_input_option(fields, queue.key.as_deref());
            push_input_option(fields, queue.queued_after_request_id.as_deref());
            push_input_option(fields, queue.interrupted_request_id.as_deref());
            push_input_option(
                fields,
                queue
                    .background_completion_wake_version
                    .map(|version| version.to_string())
                    .as_deref(),
            );
        }
        None => push_text(fields, "none"),
    }
    match &input.goal_continuation {
        Some(goal) => {
            push_text(fields, "some");
            push_text(fields, &goal.sequence.to_string());
            push_text(fields, if goal.wrapup { "true" } else { "false" });
        }
        None => push_text(fields, "none"),
    }
}

fn serialize_fields(fields: &[Vec<u8>]) -> Vec<u8> {
    let mut output = Vec::new();
    encode_length(fields.len(), &mut output);
    for field in fields {
        encode_length(field.len(), &mut output);
        output.extend_from_slice(field);
    }
    output
}

fn encode_length(length: usize, output: &mut Vec<u8>) {
    output.extend(std::iter::repeat_n(0, length));
    output.push(255);
}

/// Sole production input for authoring a new request. It owns the canonical
/// payload and GraphQL rendering so writers cannot sign one shape and persist
/// another.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentRequestCreate {
    pub request_id: String,
    pub agent_did: String,
    pub requester_did: String,
    pub behavior_id: String,
    pub session_id: String,
    pub retry_parent_request: Option<String>,
    pub retry_parent_request_doc_id: Option<String>,
    pub retry_root_request: Option<String>,
    pub retry_key: Option<String>,
    pub content: String,
    pub input: crate::request_input::RequestInput,
    pub execution_origin: String,
    pub caused_by_trigger_id: Option<String>,
    pub caused_by_trigger_doc_id: Option<String>,
    pub caused_by_trigger_kind: Option<String>,
    pub caused_by_correlation: Option<String>,
    pub caused_by_trigger_context: Option<String>,
    pub caused_by_source_doc_id: Option<String>,
    pub created_at: String,
    pub retry_count: i64,
    pub max_retries: i64,
    pub valid_until: Option<String>,
    pub subagent_depth: u32,
    pub caused_by_parent_request_id: Option<String>,
    pub caused_by_parent_request_doc_id: Option<String>,
    pub caused_by_parent_tool_call_id: Option<String>,
    pub caused_by_parent_tool_call_doc_id: Option<String>,
    pub workspace_id: Option<String>,
    /// Signed principal scope; present exactly when workspace_id is present.
    /// Issuance validates this against the existing workspace or authenticated source.
    pub workspace_owner_agent_did: Option<String>,
    pub workspace_authority: Option<String>,
    pub workspace_seal_hash: Option<String>,
    pub initial_lifecycle_state: RequestLifecycleState,
    pub admission: AgentRequestAdmissionRecord,
}

impl AgentRequestCreate {
    // This is the one required-field constructor for request creation
    // (#1336). Bundling these independent identity/admission fields into a
    // second parameter object would recreate another owner for the wire
    // contract just to satisfy an argument-count heuristic.
    #[allow(clippy::too_many_arguments)]
    pub fn base(
        request_id: impl Into<String>,
        agent_did: impl Into<String>,
        requester_did: impl Into<String>,
        behavior_id: impl Into<String>,
        session_id: impl Into<String>,
        content: impl Into<String>,
        execution_origin: impl Into<String>,
        created_at: impl Into<String>,
        admission: AgentRequestAdmissionRecord,
    ) -> Self {
        let request_id = request_id.into();
        Self {
            retry_root_request: Some(request_id.clone()),
            request_id,
            agent_did: agent_did.into(),
            requester_did: requester_did.into(),
            behavior_id: behavior_id.into(),
            session_id: session_id.into(),
            retry_parent_request: None,
            retry_parent_request_doc_id: None,
            retry_key: None,
            content: content.into(),
            input: crate::request_input::RequestInput::default(),
            execution_origin: execution_origin.into(),
            caused_by_trigger_id: None,
            caused_by_trigger_doc_id: None,
            caused_by_trigger_kind: None,
            caused_by_correlation: None,
            caused_by_trigger_context: None,
            caused_by_source_doc_id: None,
            created_at: created_at.into(),
            retry_count: 0,
            max_retries: 3,
            valid_until: None,
            subagent_depth: 0,
            caused_by_parent_request_id: None,
            caused_by_parent_request_doc_id: None,
            caused_by_parent_tool_call_id: None,
            caused_by_parent_tool_call_doc_id: None,
            workspace_id: None,
            workspace_authority: None,
            workspace_seal_hash: None,
            initial_lifecycle_state: RequestLifecycleState::Pending,
            admission,
        }
    }

    pub fn signing_fields(&self) -> AgentRequestSigningFields<'_> {
        AgentRequestSigningFields {
            request_id: &self.request_id,
            agent_did: &self.agent_did,
            requester_did: Some(&self.requester_did),
            behavior_id: &self.behavior_id,
            session_id: &self.session_id,
            retry_parent_request: self.retry_parent_request.as_deref(),
            retry_parent_request_doc_id: self.retry_parent_request_doc_id.as_deref(),
            retry_root_request: self.retry_root_request.as_deref(),
            retry_key: self.retry_key.as_deref(),
            content: &self.content,
            input: &self.input,
            execution_origin: Some(&self.execution_origin),
            caused_by_trigger_id: self.caused_by_trigger_id.as_deref(),
            caused_by_trigger_doc_id: self.caused_by_trigger_doc_id.as_deref(),
            caused_by_trigger_kind: self.caused_by_trigger_kind.as_deref(),
            caused_by_correlation: self.caused_by_correlation.as_deref(),
            caused_by_trigger_context: self.caused_by_trigger_context.as_deref(),
            caused_by_source_doc_id: self.caused_by_source_doc_id.as_deref(),
            created_at: &self.created_at,
            retry_count: Some(self.retry_count),
            max_retries: Some(self.max_retries),
            valid_until: self.valid_until.as_deref(),
            subagent_depth: self.subagent_depth,
            caused_by_parent_request_id: self.caused_by_parent_request_id.as_deref(),
            caused_by_parent_request_doc_id: self.caused_by_parent_request_doc_id.as_deref(),
            caused_by_parent_tool_call_id: self.caused_by_parent_tool_call_id.as_deref(),
            caused_by_parent_tool_call_doc_id: self.caused_by_parent_tool_call_doc_id.as_deref(),
            workspace_id: self.workspace_id.as_deref(),
            workspace_authority: self.workspace_authority.as_deref(),
            workspace_seal_hash: self.workspace_seal_hash.as_deref(),
        }
    }

    pub fn signing_payload(&self) -> Vec<u8> {
        self.admission.signing_payload(&self.signing_fields())
    }

    pub fn graphql_input_fields(&self) -> Result<String, &'static str> {
        validate_signing_fields(&self.signing_fields())
            .map_err(|_| "AgentRequest signed semantic field is non-canonical")?;
        self.admission.validate_branch_fields()?;
        self.admission
            .validate_canonical_fields()
            .map_err(|_| "AgentRequest admission field is non-canonical")?;
        if !matches!(
            self.initial_lifecycle_state,
            RequestLifecycleState::Pending | RequestLifecycleState::WorkspaceBindingPending
        ) {
            return Err("new AgentRequest must begin in a pre-claim pending state");
        }
        let mut fields = Vec::new();
        let text = |fields: &mut Vec<String>, name: &str, value: &str| {
            fields.push(format!(
                "{name}: \"{}\"",
                crate::graphql::escape_graphql_string(value)
            ));
        };
        text(&mut fields, "request_id", &self.request_id);
        text(&mut fields, "agent_did", &self.agent_did);
        text(&mut fields, "requester_did", &self.requester_did);
        text(&mut fields, "behavior_id", &self.behavior_id);
        text(&mut fields, "session_id", &self.session_id);
        optional_text(
            &mut fields,
            "retry_parent_request",
            self.retry_parent_request.as_deref(),
        );
        optional_text(
            &mut fields,
            "retry_parent_request_doc_id",
            self.retry_parent_request_doc_id.as_deref(),
        );
        optional_text(
            &mut fields,
            "retry_root_request",
            self.retry_root_request.as_deref(),
        );
        optional_text(&mut fields, "retry_key", self.retry_key.as_deref());
        text(&mut fields, "content", &self.content);
        if self.input != crate::request_input::RequestInput::default() {
            let input = serde_json::to_value(&self.input)
                .map_err(|_| "AgentRequest input serialization failed")?;
            let input = crate::graphql::graphql_input_literal(&input)
                .map_err(|_| "AgentRequest input is not a GraphQL value")?;
            fields.push(format!("input: {input}"));
        }
        text(&mut fields, "execution_origin", &self.execution_origin);
        optional_text(
            &mut fields,
            "caused_by_trigger_id",
            self.caused_by_trigger_id.as_deref(),
        );
        optional_text(
            &mut fields,
            "caused_by_trigger_doc_id",
            self.caused_by_trigger_doc_id.as_deref(),
        );
        optional_text(
            &mut fields,
            "caused_by_trigger_kind",
            self.caused_by_trigger_kind.as_deref(),
        );
        optional_text(
            &mut fields,
            "caused_by_correlation",
            self.caused_by_correlation.as_deref(),
        );
        optional_text(
            &mut fields,
            "caused_by_trigger_context",
            self.caused_by_trigger_context.as_deref(),
        );
        optional_text(
            &mut fields,
            "caused_by_source_doc_id",
            self.caused_by_source_doc_id.as_deref(),
        );
        text(&mut fields, "created_at", &self.created_at);
        fields.push(format!("retry_count: {}", self.retry_count));
        fields.push(format!("max_retries: {}", self.max_retries));
        optional_text(&mut fields, "valid_until", self.valid_until.as_deref());
        fields.push(format!("subagent_depth: {}", self.subagent_depth));
        optional_text(
            &mut fields,
            "caused_by_parent_request_id",
            self.caused_by_parent_request_id.as_deref(),
        );
        optional_text(
            &mut fields,
            "caused_by_parent_request_doc_id",
            self.caused_by_parent_request_doc_id.as_deref(),
        );
        optional_text(
            &mut fields,
            "caused_by_parent_tool_call_id",
            self.caused_by_parent_tool_call_id.as_deref(),
        );
        optional_text(
            &mut fields,
            "caused_by_parent_tool_call_doc_id",
            self.caused_by_parent_tool_call_doc_id.as_deref(),
        );
        optional_text(&mut fields, "workspace_id", self.workspace_id.as_deref());
        optional_text(
            &mut fields,
            "workspace_authority",
            self.workspace_authority.as_deref(),
        );
        optional_text(
            &mut fields,
            "workspace_seal_hash",
            self.workspace_seal_hash.as_deref(),
        );
        text(&mut fields, "admission_kind", self.admission.kind.as_str());
        text(
            &mut fields,
            "admission_signer_did",
            &self.admission.signer_did,
        );
        text(
            &mut fields,
            "admission_signature",
            &bs58::encode(&self.admission.signature).into_string(),
        );
        optional_text(
            &mut fields,
            "enrollment_request_id",
            self.admission.enrollment_request_id.as_deref(),
        );
        optional_text(
            &mut fields,
            "enrollment_request_digest",
            self.admission.enrollment_request_digest.as_deref(),
        );
        optional_text(
            &mut fields,
            "enrollment_admin_did",
            self.admission.enrollment_admin_did.as_deref(),
        );
        optional_scalar(
            &mut fields,
            "enrollment_authorization_sequence",
            self.admission.enrollment_authorization_sequence,
        );
        optional_text(
            &mut fields,
            "enrollment_authorization_expires_at",
            self.admission
                .enrollment_authorization_expires_at
                .as_deref(),
        );
        optional_text(
            &mut fields,
            "runtime_issuer_did",
            self.admission.runtime_issuer_did.as_deref(),
        );
        optional_text(
            &mut fields,
            "runtime_source_request_id",
            self.admission.runtime_source_request_id.as_deref(),
        );
        optional_text(
            &mut fields,
            "runtime_source_kind",
            self.admission
                .runtime_source_kind
                .map(RuntimeInternalSourceKind::as_str),
        );
        optional_text(
            &mut fields,
            "runtime_bridge_author_did",
            self.admission.runtime_bridge_author_did.as_deref(),
        );
        text(
            &mut fields,
            "lifecycle_state",
            self.initial_lifecycle_state.as_str(),
        );
        text(&mut fields, "failure_reason", "");
        Ok(fields.join(", "))
    }

    pub fn graphql_mutation(&self) -> Result<String, &'static str> {
        Ok(format!(
            "mutation {{ create_AgentRequest(input: {{ {} }}) {{ _docID }} }}",
            self.graphql_input_fields()?
        ))
    }
}

fn optional_text(fields: &mut Vec<String>, name: &str, value: Option<&str>) {
    if let Some(value) = value {
        fields.push(format!(
            "{name}: \"{}\"",
            crate::graphql::escape_graphql_string(value)
        ));
    }
}

fn optional_scalar<T: std::fmt::Display>(fields: &mut Vec<String>, name: &str, value: Option<T>) {
    if let Some(value) = value {
        fields.push(format!("{name}: {value}"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn local_create() -> AgentRequestCreate {
        let mut admission = AgentRequestAdmissionRecord::local_self("did:key:agent");
        admission.signature = vec![7; 64];
        AgentRequestCreate::base(
            "request-1",
            "did:key:agent",
            "did:key:agent",
            "default",
            "session-1",
            "hello",
            "interactive",
            "2026-08-30T00:00:00Z",
            admission,
        )
    }

    #[test]
    fn every_immutable_semantic_changes_the_signing_payload() {
        let base = local_create();
        let expected = base.signing_payload();
        let mut variants: Vec<(&str, AgentRequestCreate)> = Vec::new();
        macro_rules! changed {
            ($name:literal, $body:expr) => {{
                let mut value = base.clone();
                $body(&mut value);
                variants.push(($name, value));
            }};
        }
        changed!("request_id", |v: &mut AgentRequestCreate| v
            .request_id
            .push('x'));
        changed!("agent_did", |v: &mut AgentRequestCreate| v
            .agent_did
            .push('x'));
        changed!("requester_did", |v: &mut AgentRequestCreate| v
            .requester_did
            .push('x'));
        changed!("behavior_id", |v: &mut AgentRequestCreate| v
            .behavior_id
            .push('x'));
        changed!("session_id", |v: &mut AgentRequestCreate| v
            .session_id
            .push('x'));
        changed!("retry_parent_request", |v: &mut AgentRequestCreate| v
            .retry_parent_request =
            Some("parent".into()));
        changed!(
            "retry_parent_request_doc_id",
            |v: &mut AgentRequestCreate| v.retry_parent_request_doc_id = Some("parent-doc".into())
        );
        changed!("retry_root_request", |v: &mut AgentRequestCreate| v
            .retry_root_request =
            Some("other-root".into()));
        changed!("retry_key", |v: &mut AgentRequestCreate| v.retry_key =
            Some("retry-key".into()));
        changed!("content", |v: &mut AgentRequestCreate| v.content.push('!'));
        changed!("input.skills", |v: &mut AgentRequestCreate| v
            .input
            .selected_skill_ids =
            vec!["review".into()]);
        changed!("input.cwd", |v: &mut AgentRequestCreate| v.input.cwd =
            Some("/workspace".into()));
        changed!("input.title", |v: &mut AgentRequestCreate| v
            .input
            .initial_title =
            Some(crate::session::SessionTitle {
                text: "Review".into(),
                source: crate::session::SessionTitleSource::Task,
            }));
        changed!("input.queue", |v: &mut AgentRequestCreate| v.input.queue =
            Some(crate::request_input::RequestQueue {
                source: crate::request_input::QueueSource::Goal,
                policy: crate::request_input::QueuePolicy::Coalesce,
                key: Some("goal:one".into()),
                queued_after_request_id: Some("parent".into()),
                interrupted_request_id: None,
                background_completion_wake_version: None,
            }));
        changed!("input.goal_false", |v: &mut AgentRequestCreate| v
            .input
            .goal_continuation =
            Some(crate::request_input::GoalContinuationInput {
                sequence: 1,
                wrapup: false,
            }));
        changed!("execution_origin", |v: &mut AgentRequestCreate| v
            .execution_origin =
            "trigger".into());
        changed!("caused_by_trigger_id", |v: &mut AgentRequestCreate| v
            .caused_by_trigger_id =
            Some("trigger".into()));
        changed!("caused_by_trigger_doc_id", |v: &mut AgentRequestCreate| v
            .caused_by_trigger_doc_id =
            Some("trigger-doc".into()));
        changed!("caused_by_trigger_kind", |v: &mut AgentRequestCreate| v
            .caused_by_trigger_kind =
            Some("event".into()));
        changed!("caused_by_correlation", |v: &mut AgentRequestCreate| v
            .caused_by_correlation =
            Some("correlation".into()));
        changed!("caused_by_trigger_context", |v: &mut AgentRequestCreate| {
            v.caused_by_trigger_context = Some("context".into())
        });
        changed!("caused_by_source_doc_id", |v: &mut AgentRequestCreate| v
            .caused_by_source_doc_id =
            Some("source-doc".into()));
        changed!("created_at", |v: &mut AgentRequestCreate| v
            .created_at
            .push('x'));
        changed!("retry_count", |v: &mut AgentRequestCreate| v.retry_count =
            1);
        changed!("max_retries", |v: &mut AgentRequestCreate| v.max_retries =
            4);
        changed!("valid_until", |v: &mut AgentRequestCreate| v.valid_until =
            Some("2099-01-01T00:00:00Z".into()));
        changed!("subagent_depth", |v: &mut AgentRequestCreate| v
            .subagent_depth =
            1);
        changed!(
            "caused_by_parent_request_id",
            |v: &mut AgentRequestCreate| v.caused_by_parent_request_id = Some("parent".into())
        );
        changed!(
            "caused_by_parent_request_doc_id",
            |v: &mut AgentRequestCreate| v.caused_by_parent_request_doc_id =
                Some("parent-doc".into())
        );
        changed!(
            "caused_by_parent_tool_call_id",
            |v: &mut AgentRequestCreate| v.caused_by_parent_tool_call_id = Some("tool".into())
        );
        changed!(
            "caused_by_parent_tool_call_doc_id",
            |v: &mut AgentRequestCreate| v.caused_by_parent_tool_call_doc_id =
                Some("tool-doc".into())
        );
        changed!("workspace_id", |v: &mut AgentRequestCreate| v
            .workspace_id =
            Some("workspace".into()));
        changed!("workspace_authority", |v: &mut AgentRequestCreate| v
            .workspace_authority =
            Some("authority".into()));
        changed!("workspace_seal_hash", |v: &mut AgentRequestCreate| v
            .workspace_seal_hash =
            Some("seal".into()));
        changed!("admission_signer", |v: &mut AgentRequestCreate| v
            .admission
            .signer_did
            .push('x'));
        changed!("enrollment_request_id", |v: &mut AgentRequestCreate| v
            .admission
            .enrollment_request_id =
            Some("enrollment".into()));
        changed!("enrollment_request_digest", |v: &mut AgentRequestCreate| {
            v.admission.enrollment_request_digest = Some("digest".into())
        });
        changed!("enrollment_admin_did", |v: &mut AgentRequestCreate| v
            .admission
            .enrollment_admin_did =
            Some("admin".into()));
        changed!(
            "enrollment_authorization_sequence",
            |v: &mut AgentRequestCreate| v.admission.enrollment_authorization_sequence = Some(1)
        );
        changed!(
            "enrollment_authorization_expires_at",
            |v: &mut AgentRequestCreate| v.admission.enrollment_authorization_expires_at =
                Some("2099-01-01T00:00:00Z".into())
        );
        changed!("runtime_issuer_did", |v: &mut AgentRequestCreate| v
            .admission
            .runtime_issuer_did =
            Some("issuer".into()));
        changed!("runtime_source_request_id", |v: &mut AgentRequestCreate| {
            v.admission.runtime_source_request_id = Some("source".into())
        });
        changed!("runtime_source_kind", |v: &mut AgentRequestCreate| v
            .admission
            .runtime_source_kind =
            Some(RuntimeInternalSourceKind::LocalChild));
        changed!("runtime_bridge_author_did", |v: &mut AgentRequestCreate| {
            v.admission.runtime_bridge_author_did = Some("did:key:bridge".into())
        });
        for (name, variant) in variants {
            assert_ne!(
                variant.signing_payload(),
                expected,
                "field {name} was not signed"
            );
        }
    }

    #[test]
    fn typed_input_signing_distinguishes_subfields_and_skill_boundaries() {
        use crate::request_input::{GoalContinuationInput, QueuePolicy, QueueSource, RequestQueue};
        let mut base = local_create();
        base.input.selected_skill_ids = vec!["a|b".into(), "c".into()];
        base.input.cwd = Some("/workspace".into());
        base.input.initial_title = Some(crate::session::SessionTitle {
            text: "Review".into(),
            source: crate::session::SessionTitleSource::Task,
        });
        base.input.goal_continuation = Some(GoalContinuationInput {
            sequence: 1,
            wrapup: false,
        });
        base.input.queue = Some(RequestQueue {
            source: QueueSource::Goal,
            policy: QueuePolicy::Coalesce,
            key: Some("goal:one".into()),
            queued_after_request_id: Some("parent".into()),
            interrupted_request_id: Some("interrupted".into()),
            background_completion_wake_version: Some(1),
        });
        let expected = base.signing_payload();
        let mut variants = Vec::new();
        macro_rules! changed {
            ($body:expr) => {{
                let mut v = base.clone();
                $body(&mut v);
                variants.push(v);
            }};
        }
        changed!(|v: &mut AgentRequestCreate| v.input.selected_skill_ids =
            vec!["a".into(), "b|c".into()]);
        changed!(|v: &mut AgentRequestCreate| v.input.selected_skill_ids.reverse());
        changed!(|v: &mut AgentRequestCreate| v.input.cwd = None);
        changed!(|v: &mut AgentRequestCreate| v
            .input
            .initial_title
            .as_mut()
            .unwrap()
            .text
            .push('!'));
        changed!(
            |v: &mut AgentRequestCreate| v.input.initial_title.as_mut().unwrap().source =
                crate::session::SessionTitleSource::User
        );
        changed!(|v: &mut AgentRequestCreate| v.input.goal_continuation = None);
        changed!(|v: &mut AgentRequestCreate| v
            .input
            .goal_continuation
            .as_mut()
            .unwrap()
            .sequence = 2);
        changed!(
            |v: &mut AgentRequestCreate| v.input.goal_continuation.as_mut().unwrap().wrapup = true
        );
        changed!(
            |v: &mut AgentRequestCreate| v.input.queue.as_mut().unwrap().source =
                QueueSource::BackgroundCompletion
        );
        changed!(
            |v: &mut AgentRequestCreate| v.input.queue.as_mut().unwrap().policy =
                QueuePolicy::Append
        );
        changed!(|v: &mut AgentRequestCreate| v.input.queue.as_mut().unwrap().key = None);
        changed!(|v: &mut AgentRequestCreate| v
            .input
            .queue
            .as_mut()
            .unwrap()
            .queued_after_request_id = None);
        changed!(|v: &mut AgentRequestCreate| v
            .input
            .queue
            .as_mut()
            .unwrap()
            .interrupted_request_id = None);
        changed!(|v: &mut AgentRequestCreate| v
            .input
            .queue
            .as_mut()
            .unwrap()
            .background_completion_wake_version = None);
        for variant in variants {
            assert_ne!(
                variant.signing_payload(),
                expected,
                "typed input semantic was not signed: {:?}",
                variant.input
            );
        }
        let mut one_skill = local_create();
        one_skill.input.selected_skill_ids = vec!["a,b".into()];
        let mut two_skills = one_skill.clone();
        two_skills.input.selected_skill_ids = vec!["a".into(), "b".into()];
        assert_ne!(
            one_skill.signing_payload(),
            two_skills.signing_payload(),
            "skill list boundaries must be signed, not delimiter-joined"
        );
    }

    #[test]
    fn signed_semantic_identifiers_enums_and_timestamps_are_strict_but_content_is_opaque() {
        let mut value = local_create();
        value.content = "  preserve opaque content exactly  ".into();
        assert!(value.graphql_input_fields().is_ok());

        let fields = value.signing_fields();
        assert!(validate_signing_fields(&AgentRequestSigningFields {
            execution_origin: None,
            ..fields
        })
        .is_err());

        for behavior in ["", " ", " default", "default "] {
            let mut value = local_create();
            value.behavior_id = behavior.into();
            assert!(
                value.graphql_input_fields().is_err(),
                "explicit canonical behavior required"
            );
        }
        for hostile in [" request-1", "request-1 "] {
            let mut value = local_create();
            value.request_id = hostile.into();
            assert!(value.graphql_input_fields().is_err());
        }
        let mut value = local_create();
        value.execution_origin = " interactive".into();
        assert!(value.graphql_input_fields().is_err());
        let mut value = local_create();
        value.execution_origin = "unknown".into();
        assert!(value.graphql_input_fields().is_err());
        let mut value = local_create();
        value.created_at = "2026-08-30T00:00:00+00:00".into();
        assert!(value.graphql_input_fields().is_err());
        let mut value = local_create();
        value.created_at = "2026-08-30T00:00:00.000Z".into();
        assert!(value.graphql_input_fields().is_err());
    }

    #[test]
    fn branch_fields_are_all_present_or_all_absent() {
        let mut enrollment = AgentRequestAdmissionRecord::enrollment(
            "did:key:member",
            "request",
            "digest",
            "did:key:admin",
            1,
            "2099-01-01T00:00:00Z",
        );
        enrollment.signature = vec![1; 64];
        assert!(enrollment.validate_branch_fields().is_ok());
        enrollment.runtime_issuer_did = Some("did:key:member".into());
        assert!(enrollment.validate_branch_fields().is_err());

        let mut local = AgentRequestAdmissionRecord::local_self("did:key:agent");
        local.signature = vec![1; 64];
        assert!(local.validate_branch_fields().is_ok());
        local.enrollment_request_id = Some("request".into());
        assert!(local.validate_branch_fields().is_err());

        let mut internal =
            AgentRequestAdmissionRecord::runtime_local_control("did:key:agent", "source");
        internal.signature = vec![1; 64];
        assert!(internal.validate_branch_fields().is_ok());
        internal.runtime_source_request_id = None;
        assert!(internal.validate_branch_fields().is_err());

        let mut cross = AgentRequestAdmissionRecord::runtime_cross_principal_child(
            "did:key:agent",
            "source",
            "did:key:bridge",
        );
        cross.signature = vec![1; 64];
        assert!(cross.validate_branch_fields().is_ok());
        cross.runtime_source_kind = Some(RuntimeInternalSourceKind::LocalChild);
        assert!(
            cross.validate_branch_fields().is_err(),
            "cross bridge evidence cannot switch to the local-child branch"
        );

        let mut local = AgentRequestAdmissionRecord::runtime_local_child("did:key:agent", "source");
        local.signature = vec![1; 64];
        local.runtime_source_kind = Some(RuntimeInternalSourceKind::CrossPrincipalChild);
        assert!(
            local.validate_branch_fields().is_err(),
            "local evidence cannot switch to cross-principal without a bridge author"
        );
    }

    #[test]
    fn durable_branch_fields_must_be_exactly_canonical() {
        let signature = bs58::encode([7_u8; 64]).into_string();
        let parse = |request_id: Option<&str>, runtime_source: Option<&str>| {
            AgentRequestAdmissionRecord::from_wire_fields(
                Some(if runtime_source.is_some() {
                    "runtime-internal"
                } else {
                    "enrollment"
                }),
                Some("did:key:member"),
                Some(&signature),
                request_id,
                request_id.map(|_| "digest"),
                request_id.map(|_| "did:key:admin"),
                request_id.map(|_| 1),
                request_id.map(|_| "2099-01-01T00:00:00Z"),
                runtime_source.map(|_| "did:key:member"),
                runtime_source,
                runtime_source.map(|_| "local-control"),
                None,
            )
        };

        assert!(parse(Some("request"), None).is_ok());
        assert!(parse(Some(" request"), None).is_err());
        assert!(parse(Some("request "), None).is_err());
        assert!(parse(None, Some("source")).is_ok());
        assert!(parse(None, Some(" source")).is_err());
        assert!(parse(None, Some("source ")).is_err());
    }

    #[test]
    fn canonical_graphql_never_emits_an_empty_list_literal() {
        let mutation = local_create().graphql_mutation().unwrap();
        assert!(!mutation.contains("[]"));
        assert!(mutation.contains("admission_kind: \"local-self\""));
    }

    #[test]
    fn non_pending_initial_lifecycle_state_is_rejected() {
        let mut create = local_create();
        create.initial_lifecycle_state = RequestLifecycleState::Claimed;
        assert_eq!(
            create.graphql_input_fields(),
            Err("new AgentRequest must begin in a pre-claim pending state")
        );
    }

    #[test]
    fn workspace_binding_pending_emits_lifecycle_state_and_no_status_field() {
        let mut create = local_create();
        create.initial_lifecycle_state = RequestLifecycleState::WorkspaceBindingPending;
        let fields = create.graphql_input_fields().unwrap();
        assert!(fields.contains(r#"lifecycle_state: "workspaceBindingPending""#));
        assert!(!fields.contains("status:"));
    }

    #[test]
    fn workspace_authority_accepts_every_mode_modeled_by_the_runtime() {
        for authority in ["readOnly", "readWrite", "integrate"] {
            let mut request = local_create();
            request.workspace_authority = Some(authority.to_string());
            assert!(
                validate_signing_fields(&request.signing_fields()).is_ok(),
                "workspace authority {authority} must be admissible"
            );
        }

        let mut request = local_create();
        request.workspace_authority = Some("operator".to_string());
        assert!(validate_signing_fields(&request.signing_fields()).is_err());
    }
}
