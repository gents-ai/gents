use crate::canonical::{
    parse_utc_seconds, require_enum, require_identifier, require_optional_identifier,
};
use crate::request_lifecycle::RequestLifecycleState;
use serde::{Deserialize, Serialize};

const REQUEST_SIGNATURE_DOMAIN: &str = "gents-agent-request-admission-v1";

/// Signed admission purpose. Missing or unknown values must not acquire normal
/// request authority; auxiliary work owns an independent request lease.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RequestPurpose {
    Normal,
    TitleAudit,
    AccessReview,
}

impl RequestPurpose {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::TitleAudit => "title-audit",
            Self::AccessReview => "access-review",
        }
    }

    pub const PUBLIC: Self = Self::Normal;

    pub const fn is_auxiliary(self) -> bool {
        matches!(self, Self::TitleAudit | Self::AccessReview)
    }

    pub fn is_public(purpose: Option<Self>) -> bool {
        purpose == Some(Self::PUBLIC)
    }
}

impl TryFrom<&str> for RequestPurpose {
    type Error = &'static str;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "normal" => Ok(Self::Normal),
            "title-audit" => Ok(Self::TitleAudit),
            "access-review" => Ok(Self::AccessReview),
            _ => Err("unknown AgentRequest purpose"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AgentRequestAdmissionKind {
    Enrollment,
    LocalSelf,
    RuntimeInternal,
    /// A request authored by another node's agent. The requester signs, the
    /// target differs from the requester, and the target's
    /// `PeerAdmissionAuthority` ACP authorizes the requester DID. Replies
    /// route back to `requester_did` best-effort.
    Peer,
}

impl AgentRequestAdmissionKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Enrollment => "enrollment",
            Self::LocalSelf => "local-self",
            Self::RuntimeInternal => "runtime-internal",
            Self::Peer => "peer",
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
            "peer" => Ok(Self::Peer),
            _ => Err("unknown AgentRequest admission kind"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RuntimeInternalSourceKind {
    LocalControl,
    AutomatedTrigger,
}

impl RuntimeInternalSourceKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LocalControl => "local-control",
            Self::AutomatedTrigger => "automated-trigger",
        }
    }
}

impl TryFrom<&str> for RuntimeInternalSourceKind {
    type Error = &'static str;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
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
    /// The requester owns the target session or is the target for a new
    /// session or title audit (Lean `Enrollment.runtimeRequesterScope`).
    pub requester_matches_session_scope: bool,
    pub current_approval: bool,
    pub exact_generation: bool,
    pub authorization_fresh: bool,
    pub runtime_evidence_present: bool,
    pub runtime_source_kind: RuntimeInternalSourceKind,
    pub target_runtime_attestation_valid: bool,
    pub source_binding_current: bool,
    pub trigger_config_document_binding_current: bool,
    pub source_document_binding_current: bool,
    pub target_policy_allows: bool,
    /// The target's `PeerAdmissionAuthority` ACP authorizes the requester DID.
    pub peer_authority_allows: bool,
    /// The target node's configured causal-hop cap admits this request, if set.
    pub hop_within_bound: bool,
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
        || !observation.hop_within_bound
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
        AgentRequestAdmissionKind::Peer => {
            observation.signer_matches_requester
                && !observation.requester_matches_target
                && observation.peer_authority_allows
        }
        AgentRequestAdmissionKind::RuntimeInternal => {
            let common = observation.runtime_evidence_present
                && observation.signer_matches_issuer
                && observation.signer_matches_target
                && observation.target_runtime_attestation_valid
                && observation.source_binding_current;
            common
                && match observation.runtime_source_kind {
                    RuntimeInternalSourceKind::LocalControl => {
                        observation.requester_matches_session_scope
                            && observation.source_document_binding_current
                    }
                    RuntimeInternalSourceKind::AutomatedTrigger => {
                        observation.requester_matches_session_scope
                            && observation.trigger_config_document_binding_current
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

#[derive(Debug, Clone, PartialEq)]
pub struct AgentRequestSigningFields<'a> {
    pub request_id: &'a str,
    pub purpose: RequestPurpose,
    pub node_did: &'a str,
    pub requester_did: Option<&'a str>,
    pub agent_id: &'a str,
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
    pub request_hop: u32,
    pub caused_by_parent_request_id: Option<&'a str>,
    pub caused_by_parent_request_doc_id: Option<&'a str>,
    pub caused_by_parent_tool_call_id: Option<&'a str>,
    pub caused_by_parent_tool_call_doc_id: Option<&'a str>,
    pub workspace_id: Option<&'a str>,
    /// Node scope copied from the verified workspace/source, not a host identity.
    pub workspace_owner_node_did: Option<&'a str>,
    pub workspace_authority: Option<&'a str>,
    pub workspace_seal_hash: Option<&'a str>,
}

pub fn validate_signing_fields(request: &AgentRequestSigningFields<'_>) -> anyhow::Result<()> {
    for (name, value) in [
        ("request_id", request.request_id),
        ("node_did", request.node_did),
        ("session_id", request.session_id),
    ] {
        require_identifier(name, value)?;
    }
    require_identifier("agent_id", request.agent_id)?;
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
        ("workspace_owner_node_did", request.workspace_owner_node_did),
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
            &["manual", "event", "schedule", "goal"],
        )?;
    }
    validate_workspace_reference(
        request.workspace_id,
        request.workspace_owner_node_did,
        request.workspace_authority,
        request.workspace_seal_hash,
    )?;
    parse_utc_seconds("created_at", request.created_at)?;
    if let Some(valid_until) = request.valid_until {
        parse_utc_seconds("valid_until", valid_until)?;
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
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
        }
    }

    pub fn peer(signer_did: impl Into<String>) -> Self {
        Self {
            kind: AgentRequestAdmissionKind::Peer,
            ..Self::local_self(signer_did)
        }
    }

    fn runtime_internal(
        target_did: impl Into<String>,
        source_request_id: impl Into<String>,
        source_kind: RuntimeInternalSourceKind,
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
        }
    }

    pub fn runtime_local_control(
        target_did: impl Into<String>,
        source_request_id: impl Into<String>,
    ) -> Self {
        Self::runtime_internal(
            target_did,
            source_request_id,
            RuntimeInternalSourceKind::LocalControl,
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
            && self.runtime_source_kind.is_none();
        match self.kind {
            AgentRequestAdmissionKind::Enrollment if enrollment_present && runtime_absent => Ok(()),
            AgentRequestAdmissionKind::LocalSelf | AgentRequestAdmissionKind::Peer
                if enrollment_absent && runtime_absent =>
            {
                Ok(())
            }
            AgentRequestAdmissionKind::RuntimeInternal if enrollment_absent && runtime_present => {
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
        push_text(&mut fields, request.purpose.as_str());
        push_text(&mut fields, request.node_did);
        push_option(&mut fields, request.requester_did);
        push_text(&mut fields, request.agent_id);
        push_text(&mut fields, request.session_id);
        push_option(&mut fields, request.retry_parent_request);
        push_option(&mut fields, request.retry_parent_request_doc_id);
        push_option(&mut fields, request.retry_root_request);
        push_option(&mut fields, request.retry_key);
        push_text(&mut fields, request.content);
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
        push_text(&mut fields, &request.request_hop.to_string());
        push_option(&mut fields, request.caused_by_parent_request_id);
        push_option(&mut fields, request.caused_by_parent_request_doc_id);
        push_option(&mut fields, request.caused_by_parent_tool_call_id);
        push_option(&mut fields, request.caused_by_parent_tool_call_doc_id);
        push_option(&mut fields, request.workspace_id);
        push_option(&mut fields, request.workspace_owner_node_did);
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
        serialize_fields(&fields)
    }
}

/// Structural validation only. Admission authenticates the source; workspace
/// owners still validate ACP, principal grants, lifecycle, placement and seal.
pub fn validate_workspace_reference(
    workspace_id: Option<&str>,
    owner_node_did: Option<&str>,
    authority: Option<&str>,
    seal_hash: Option<&str>,
) -> anyhow::Result<()> {
    require_optional_identifier("workspace_id", workspace_id)?;
    require_optional_identifier("workspace_owner_node_did", owner_node_did)?;
    match (workspace_id, owner_node_did, authority) {
        (None, None, None) if seal_hash.is_none() => Ok(()),
        (Some(_), Some(_), Some(authority)) => require_enum(
            "workspace_authority",
            authority,
            &["readOnly", "readWrite", "integrate"],
        ),
        _ => {
            anyhow::bail!("workspace reference requires id, owner node and authority together")
        }
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

fn title_source_wire_name(source: crate::session::SessionTitleSource) -> &'static str {
    match source {
        crate::session::SessionTitleSource::Placeholder => "placeholder",
        crate::session::SessionTitleSource::Generated => "generated",
        crate::session::SessionTitleSource::Task => "task",
        crate::session::SessionTitleSource::User => "user",
    }
}

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

/// Canonical typed-input framing mirroring the Lean
/// `Enrollment.requestInputFields` byte contract. The one canonical input
/// encoder for signing and verification.
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
            if !queue.delivery.is_queue() || queue.position.is_some() {
                push_text(fields, "queue-v2");
                push_text(
                    fields,
                    match queue.delivery {
                        crate::request_input::QueueDelivery::Queue => "queue",
                        crate::request_input::QueueDelivery::Steer => "steer",
                    },
                );
                match &queue.position {
                    Some(position) => {
                        push_text(fields, "some");
                        push_text(fields, &position.slot_request_doc_id);
                        push_text(fields, &position.replaces_request_doc_id);
                    }
                    None => push_text(fields, "none"),
                }
            }
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
    if !input.captured_context.is_empty() {
        push_text(fields, "captured-context");
        push_text(fields, &input.captured_context.len().to_string());
        for context in &input.captured_context {
            push_captured_context(fields, context);
        }
    }
    if let Some(reference) = &input.goal_assignment {
        push_text(fields, "goal-assignment");
        push_text(fields, &reference.goal_id);
        push_input_option(fields, reference.root_request_doc_id.as_deref());
    }
}

fn push_captured_context(
    fields: &mut Vec<Vec<u8>>,
    context: &crate::request_input::CapturedContext,
) {
    push_text(fields, context.kind.as_str());
    push_text(fields, context.source.as_str());
    push_text(fields, &context.captured_at);
    push_input_option(fields, context.uri.as_deref());
    push_input_option(fields, context.terminal_name.as_deref());
    push_input_option(fields, context.workspace_uri.as_deref());
    push_input_option(
        fields,
        context
            .version
            .map(|version| version.to_string())
            .as_deref(),
    );
    push_text(fields, if context.unsaved { "true" } else { "false" });
    match context.range {
        Some(range) => {
            push_text(fields, "some");
            for coordinate in [
                range.start.line,
                range.start.character,
                range.end.line,
                range.end.character,
            ] {
                push_text(fields, &coordinate.to_string());
            }
        }
        None => push_text(fields, "none"),
    }
    push_text(fields, &context.content);
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
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AgentRequestCreate {
    pub request_id: String,
    pub purpose: RequestPurpose,
    pub node_did: String,
    pub requester_did: String,
    pub agent_id: String,
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
    pub request_hop: u32,
    pub caused_by_parent_request_id: Option<String>,
    pub caused_by_parent_request_doc_id: Option<String>,
    pub caused_by_parent_tool_call_id: Option<String>,
    pub caused_by_parent_tool_call_doc_id: Option<String>,
    pub workspace_id: Option<String>,
    /// Signed node scope; present exactly when workspace_id is present.
    pub workspace_owner_node_did: Option<String>,
    pub workspace_authority: Option<String>,
    pub workspace_seal_hash: Option<String>,
    pub initial_lifecycle_state: RequestLifecycleState,
    pub admission: AgentRequestAdmissionRecord,
}

impl AgentRequestCreate {
    #[allow(clippy::too_many_arguments)]
    pub fn base(
        purpose: RequestPurpose,
        request_id: impl Into<String>,
        node_did: impl Into<String>,
        requester_did: impl Into<String>,
        agent_id: impl Into<String>,
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
            purpose,
            node_did: node_did.into(),
            requester_did: requester_did.into(),
            agent_id: agent_id.into(),
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
            request_hop: 0,
            caused_by_parent_request_id: None,
            caused_by_parent_request_doc_id: None,
            caused_by_parent_tool_call_id: None,
            caused_by_parent_tool_call_doc_id: None,
            workspace_id: None,
            workspace_owner_node_did: None,
            workspace_authority: None,
            workspace_seal_hash: None,
            initial_lifecycle_state: RequestLifecycleState::Pending,
            admission,
        }
    }

    pub fn signing_fields(&self) -> AgentRequestSigningFields<'_> {
        AgentRequestSigningFields {
            request_id: &self.request_id,
            purpose: self.purpose,
            node_did: &self.node_did,
            requester_did: Some(&self.requester_did),
            agent_id: &self.agent_id,
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
            request_hop: self.request_hop,
            caused_by_parent_request_id: self.caused_by_parent_request_id.as_deref(),
            caused_by_parent_request_doc_id: self.caused_by_parent_request_doc_id.as_deref(),
            caused_by_parent_tool_call_id: self.caused_by_parent_tool_call_id.as_deref(),
            caused_by_parent_tool_call_doc_id: self.caused_by_parent_tool_call_doc_id.as_deref(),
            workspace_id: self.workspace_id.as_deref(),
            workspace_owner_node_did: self.workspace_owner_node_did.as_deref(),
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
        text(&mut fields, "purpose", self.purpose.as_str());
        text(&mut fields, "node_did", &self.node_did);
        text(&mut fields, "requester_did", &self.requester_did);
        text(&mut fields, "agent_id", &self.agent_id);
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
        fields.push(format!("request_hop: {}", self.request_hop));
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
            "workspace_owner_node_did",
            self.workspace_owner_node_did.as_deref(),
        );
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
        text(
            &mut fields,
            "lifecycle_state",
            self.initial_lifecycle_state.as_str(),
        );
        text(&mut fields, "failure_reason", "");
        Ok(fields.join(", "))
    }

    pub fn graphql_mutation(&self) -> Result<String, &'static str> {
        self.graphql_mutation_selecting("_docID")
    }

    /// Keep signed create input canonical while an owner requests additional
    /// returned fields in the same mutation. `selection` must be trusted static
    /// GraphQL field syntax, never user input.
    pub fn graphql_mutation_selecting(
        &self,
        selection: &'static str,
    ) -> Result<String, &'static str> {
        Ok(format!(
            "mutation {{ create_AgentRequest(input: {{ {} }}) {{ {selection} }} }}",
            self.graphql_input_fields()?,
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

fn optional_scalar<T>(fields: &mut Vec<String>, name: &str, value: Option<T>)
where
    T: std::fmt::Display,
{
    if let Some(value) = value {
        fields.push(format!("{name}: {value}"));
    }
}
