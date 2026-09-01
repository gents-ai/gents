//! Canonical cryptographic envelope for local/self persona requests.

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::canonical::{
    parse_utc_seconds, require_enum, require_identifier, require_optional_identifier,
};
use crate::enrollment::canonical_domain_payload;

pub const PERSONA_AUTHORITY_ENROLLMENT: &str = "enrollment";
pub const PERSONA_AUTHORITY_LOCAL_SELF: &str = "local-self";
const LOCAL_PERSONA_SIGNATURE_DOMAIN: &str = "gents-persona-local-self-signature-v1";
const MAX_PERSONA_FIELD_BYTES: usize = 16 * 1024;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocalPersonaRequestRecord {
    pub request_key: String,
    pub requester_did: String,
    pub agent_did: String,
    pub authority_kind: String,
    pub local_signer_did: String,
    pub op: String,
    pub behavior_id: Option<String>,
    pub clone_from: Option<String>,
    pub persona_name: Option<String>,
    pub backend_model: Option<String>,
    pub root: Option<String>,
    pub preset: Option<String>,
    pub profile_id: Option<String>,
    pub created_at: String,
    pub local_signature: Vec<u8>,
}

impl LocalPersonaRequestRecord {
    pub fn validate_shape(&self) -> Result<()> {
        anyhow::ensure!(
            self.authority_kind == PERSONA_AUTHORITY_LOCAL_SELF,
            "local persona request authority_kind must be local-self"
        );
        anyhow::ensure!(
            self.local_signer_did == self.requester_did && self.requester_did == self.agent_did,
            "local persona request signer, requester, and target agent must match"
        );
        for (name, value) in [
            ("request_key", self.request_key.as_str()),
            ("requester_did", self.requester_did.as_str()),
            ("agent_did", self.agent_did.as_str()),
            ("local_signer_did", self.local_signer_did.as_str()),
            ("op", self.op.as_str()),
            ("created_at", self.created_at.as_str()),
        ] {
            require_identifier(name, value)?;
            anyhow::ensure!(
                value.len() <= MAX_PERSONA_FIELD_BYTES,
                "{name} exceeds maximum length"
            );
        }
        for (name, value) in [
            ("behavior_id", self.behavior_id.as_deref()),
            ("clone_from", self.clone_from.as_deref()),
            ("persona_name", self.persona_name.as_deref()),
            ("backend_model", self.backend_model.as_deref()),
            ("root", self.root.as_deref()),
            ("preset", self.preset.as_deref()),
            ("profile_id", self.profile_id.as_deref()),
        ] {
            anyhow::ensure!(
                value.is_none_or(|value| value.len() <= MAX_PERSONA_FIELD_BYTES),
                "{name} exceeds maximum length"
            );
            if name != "persona_name" {
                require_optional_identifier(name, value)?;
            }
        }
        require_enum("persona op", &self.op, &["create", "edit", "disable"])?;
        parse_utc_seconds("persona created_at", &self.created_at)?;
        anyhow::ensure!(
            self.local_signature.len() == 64,
            "invalid local persona signature length"
        );
        Ok(())
    }

    pub fn signing_payload(&self) -> Vec<u8> {
        fn option(value: Option<&str>) -> String {
            match value {
                Some(value) => format!("some:{value}"),
                None => "none:".to_string(),
            }
        }
        let fields = [
            self.request_key.clone(),
            self.requester_did.clone(),
            self.agent_did.clone(),
            self.authority_kind.clone(),
            self.local_signer_did.clone(),
            self.op.clone(),
            option(self.behavior_id.as_deref()),
            option(self.clone_from.as_deref()),
            option(self.persona_name.as_deref()),
            option(self.backend_model.as_deref()),
            option(self.root.as_deref()),
            option(self.preset.as_deref()),
            option(self.profile_id.as_deref()),
            self.created_at.clone(),
        ];
        canonical_domain_payload(
            LOCAL_PERSONA_SIGNATURE_DOMAIN,
            fields.iter().map(String::as_str),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> LocalPersonaRequestRecord {
        LocalPersonaRequestRecord {
            request_key: "persona-1".into(),
            requester_did: "did:key:local".into(),
            agent_did: "did:key:local".into(),
            authority_kind: PERSONA_AUTHORITY_LOCAL_SELF.into(),
            local_signer_did: "did:key:local".into(),
            op: "create".into(),
            behavior_id: None,
            clone_from: None,
            persona_name: Some("Research".into()),
            backend_model: Some("openai|gpt-5".into()),
            root: None,
            preset: Some("write".into()),
            profile_id: Some("profile-1".into()),
            created_at: "2026-08-29T00:00:00Z".into(),
            local_signature: vec![0; 64],
        }
    }

    #[test]
    fn payload_is_stable_and_covers_each_semantic_field() {
        let base = record();
        let payload = base.signing_payload();
        assert_eq!(payload, base.signing_payload());
        let mut changed = base.clone();
        changed.preset = Some("readonly".into());
        assert_ne!(payload, changed.signing_payload());
        changed = base.clone();
        changed.agent_did = "did:key:other".into();
        assert_ne!(payload, changed.signing_payload());
    }

    #[test]
    fn shape_rejects_cross_principal_and_bad_signature_length() {
        let mut value = record();
        assert!(value.validate_shape().is_ok());
        value.agent_did = "did:key:other".into();
        assert!(value.validate_shape().is_err());
        value.agent_did = value.requester_did.clone();
        value.local_signature.clear();
        assert!(value.validate_shape().is_err());
    }

    #[test]
    fn semantic_identifiers_and_timestamp_are_canonical_while_name_is_opaque() {
        let mut value = record();
        value.persona_name = Some("  Display Name  ".into());
        assert!(value.validate_shape().is_ok());
        value.behavior_id = Some(" behavior".into());
        assert!(value.validate_shape().is_err());
        value.behavior_id = None;
        value.created_at = "2026-08-29T00:00:00+00:00".into();
        assert!(value.validate_shape().is_err());
        value.created_at = "2026-08-29T00:00:00Z".into();
        value.op = " create".into();
        assert!(value.validate_shape().is_err());
    }
}
