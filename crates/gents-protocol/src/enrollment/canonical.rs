use chrono::{DateTime, FixedOffset, SecondsFormat, Utc};
use sha2::{Digest as _, Sha256};

use super::error::{EnrollmentError, Result};
use super::{
    DECISION_SIGNATURE_DOMAIN, ENROLLMENT_DIGEST_DOMAIN, ENROLLMENT_DIGEST_PREFIX,
    ENROLLMENT_SCHEMA_DOMAIN, MAX_ENROLLMENT_FIELD_BYTES, OFFER_SIGNATURE_DOMAIN,
    REQUEST_SIGNATURE_DOMAIN, REVISION_SIGNATURE_DOMAIN, ROUTE_RECEIPT_SIGNATURE_DOMAIN,
};

pub fn canonical_enrollment_payload<'a>(fields: impl IntoIterator<Item = &'a str>) -> Vec<u8> {
    canonical_domain_payload(ENROLLMENT_DIGEST_DOMAIN, fields)
}

pub fn canonical_domain_payload<'a>(
    domain: &'a str,
    fields: impl IntoIterator<Item = &'a str>,
) -> Vec<u8> {
    let fields = std::iter::once(domain).chain(fields).collect::<Vec<_>>();
    let mut encoded = encode_wire_length(fields.len());
    for field in fields {
        encoded.extend(frame_enrollment_field(field));
    }
    encoded
}

pub fn frame_enrollment_field(value: &str) -> Vec<u8> {
    let mut encoded = encode_wire_length(value.len());
    encoded.extend_from_slice(value.as_bytes());
    encoded
}

pub fn canonical_enrollment_digest<'a>(fields: impl IntoIterator<Item = &'a str>) -> String {
    format!(
        "{ENROLLMENT_DIGEST_PREFIX}{}",
        lower_hex(&canonical_enrollment_payload(fields))
    )
}

pub fn derive_enrollment_id(domain: &str, fields: &[&str]) -> String {
    let mut hash = Sha256::new();
    hash.update(canonical_domain_payload(domain, fields.iter().copied()));
    bs58::encode(&hash.finalize()[..20]).into_string()
}

/// Shared observation-time interpretation of a signed authorization lease.
/// Invalid or non-canonical timestamps fail closed.
pub fn authorization_lease_is_fresh_at(expires_at: &str, now: DateTime<Utc>) -> bool {
    parse_canonical_timestamp("authorization_expires_at", expires_at)
        .ok()
        .is_some_and(|expires_at| now < expires_at.with_timezone(&Utc))
}

/// Fingerprint of the exact durable enrollment wire schemas: carried in signed
/// offers so a client cannot write a request whose immutable fields the server
/// interprets under a different schema.
pub fn enrollment_schema_fingerprint() -> String {
    let mut hash = Sha256::new();
    hash.update(canonical_domain_payload(
        ENROLLMENT_SCHEMA_DOMAIN,
        [
            gents_schemas::NETWORK_ADMIN_PIN,
            gents_schemas::NETWORK_ENROLLMENT_REQUEST,
            gents_schemas::NETWORK_ENROLLMENT_DECISION,
            gents_schemas::NETWORK_AUTHORIZATION_REVISION,
            gents_schemas::NETWORK_ENROLLMENT_ROUTE_RECEIPT,
            OFFER_SIGNATURE_DOMAIN,
            REQUEST_SIGNATURE_DOMAIN,
            DECISION_SIGNATURE_DOMAIN,
            REVISION_SIGNATURE_DOMAIN,
            ROUTE_RECEIPT_SIGNATURE_DOMAIN,
        ],
    ));
    format!("sha256:{}", lower_hex(&hash.finalize()))
}

pub(super) fn validate_exact_field(name: &'static str, value: &str) -> Result<()> {
    if value.is_empty() {
        return Err(EnrollmentError::OfferFieldEmpty { name });
    }
    if value != value.trim() {
        return Err(EnrollmentError::OfferFieldWhitespace { name });
    }
    if value.len() > MAX_ENROLLMENT_FIELD_BYTES {
        return Err(EnrollmentError::OfferFieldTooLarge { name });
    }
    Ok(())
}

pub(super) fn parse_canonical_timestamp(
    name: &'static str,
    value: &str,
) -> Result<DateTime<FixedOffset>> {
    if !value.ends_with('Z') {
        return Err(EnrollmentError::TimestampNotUtc { name });
    }
    let parsed = DateTime::parse_from_rfc3339(value)
        .map_err(|source| EnrollmentError::TimestampParse { name, source })?;
    if parsed.to_rfc3339_opts(SecondsFormat::Secs, true) != value {
        return Err(EnrollmentError::TimestampNotCanonical { name });
    }
    Ok(parsed)
}

fn encode_wire_length(length: usize) -> Vec<u8> {
    let mut encoded = vec![0; length];
    encoded.push(0xff);
    encoded
}

fn lower_hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut rendered = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        rendered.push(DIGITS[(byte >> 4) as usize] as char);
        rendered.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    rendered
}
