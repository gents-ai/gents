use std::collections::BTreeMap;

use anyhow::{ensure, Result};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::ServeLifecycle;
use crate::enrollment::canonical_domain_payload;

pub const RUNTIME_CONTROL_VERSION: u8 = 1;
pub const RUNTIME_CONTROL_PATH: &str = "/runtime/operator/control";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase", deny_unknown_fields)]
pub enum RuntimeOperation {
    Observe,
    Retire { expected_build: String },
}

/// The challenge binds a fresh observation to its caller; it is not a durable
/// mutation nonce. Retirement requires the existing DefraDB HTTP operator
/// identity and remains available when the store cannot journal another write.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeControlCommand {
    pub version: u8,
    pub node_did: String,
    pub nonce: String,
    pub issued_at: String,
    pub operation: RuntimeOperation,
}

impl RuntimeControlCommand {
    pub fn digest(&self) -> Result<String> {
        Ok(format!(
            "{:x}",
            Sha256::digest(payload("gents-runtime-control-request-v1", self)?)
        ))
    }

    pub fn validate_at(&self, now: DateTime<Utc>) -> Result<()> {
        ensure!(
            self.version == RUNTIME_CONTROL_VERSION,
            "unsupported runtime control version"
        );
        ensure!(
            self.node_did.starts_with("did:") && !self.node_did.chars().any(char::is_whitespace),
            "runtime control requires a node DID"
        );
        ensure!(
            (16..=128).contains(&self.nonce.len())
                && self
                    .nonce
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-'),
            "invalid runtime control nonce"
        );
        validate_time(&self.issued_at, now)?;
        if let RuntimeOperation::Retire { expected_build } = &self.operation {
            ensure!(
                valid_build(expected_build),
                "invalid observed runtime build"
            );
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RuntimeControlDisposition {
    Observed,
    Retiring,
    Busy,
    BuildChanged,
    Starting,
    Stopping,
}

/// This receipt authenticates a build and one command outcome, never store
/// release. A replacement still requires the native claim-release and startup
/// reservation owners. Build identity does not establish version ordering.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeControlReceipt {
    pub version: u8,
    pub node_did: String,
    pub command_nonce: String,
    pub command_digest: String,
    pub build: String,
    pub capabilities: BTreeMap<String, u64>,
    pub replicated_schema: Option<crate::peer_schema::ReplicatedSchema>,
    pub lifecycle: ServeLifecycle,
    pub retiring: bool,
    pub disposition: RuntimeControlDisposition,
    pub observed_at: String,
    pub signature: Vec<u8>,
}

impl RuntimeControlReceipt {
    pub fn signing_payload(&self) -> Result<Vec<u8>> {
        let mut unsigned = self.clone();
        unsigned.signature.clear();
        payload("gents-runtime-operator-receipt-v1", &unsigned)
    }

    pub fn validate_for(&self, command: &RuntimeControlCommand, now: DateTime<Utc>) -> Result<()> {
        ensure!(
            self.version == RUNTIME_CONTROL_VERSION
                && self.node_did == command.node_did
                && self.command_nonce == command.nonce
                && self.command_digest == command.digest()?,
            "runtime receipt differs from the requested node or command"
        );
        ensure!(valid_build(&self.build), "invalid runtime receipt build");
        ensure!(
            self.signature.len() == 64,
            "invalid runtime receipt signature length"
        );
        validate_time(&self.observed_at, now)?;
        let matches_operation = match (&command.operation, self.disposition) {
            (RuntimeOperation::Observe, RuntimeControlDisposition::Observed) => true,
            (RuntimeOperation::Retire { expected_build }, RuntimeControlDisposition::Retiring) => {
                expected_build == &self.build && self.retiring
            }
            (
                RuntimeOperation::Retire { expected_build },
                RuntimeControlDisposition::BuildChanged,
            ) => expected_build != &self.build,
            (RuntimeOperation::Retire { .. }, RuntimeControlDisposition::Busy) => !self.retiring,
            (_, RuntimeControlDisposition::Starting) => self.lifecycle == ServeLifecycle::Starting,
            (_, RuntimeControlDisposition::Stopping) => self.retiring,
            _ => false,
        };
        ensure!(
            matches_operation,
            "runtime receipt outcome differs from its command"
        );
        Ok(())
    }
}

pub fn current_capabilities() -> BTreeMap<String, u64> {
    [
        ("runtime_control", u64::from(RUNTIME_CONTROL_VERSION)),
        (
            "mailbox_question",
            u64::from(crate::mailbox_question::MAILBOX_QUESTION_VERSION),
        ),
        (
            "request_input",
            u64::from(crate::request_input::REQUEST_INPUT_CONTRACT_VERSION),
        ),
        (
            "goal_stop",
            u64::from(crate::goal_control::GOAL_STOP_CONTRACT_VERSION),
        ),
        (
            "file_inventory",
            u64::from(crate::file_inventory::FILE_INVENTORY_RECEIPT_VERSION),
        ),
    ]
    .into_iter()
    .map(|(key, version)| (key.to_owned(), version))
    .collect()
}

fn valid_build(build: &str) -> bool {
    build.len() == 64
        && build
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn validate_time(value: &str, now: DateTime<Utc>) -> Result<()> {
    let issued = DateTime::parse_from_rfc3339(value)?.with_timezone(&Utc);
    ensure!(
        issued <= now + Duration::seconds(5),
        "runtime control timestamp is in the future"
    );
    ensure!(
        now - issued <= Duration::seconds(60),
        "runtime control timestamp expired"
    );
    Ok(())
}

fn payload<T>(domain: &str, value: &T) -> Result<Vec<u8>>
where
    T: Serialize,
{
    let encoded = serde_json::to_string(value)?;
    Ok(canonical_domain_payload(domain, [encoded.as_str()]))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command(operation: RuntimeOperation, now: DateTime<Utc>) -> RuntimeControlCommand {
        RuntimeControlCommand {
            version: RUNTIME_CONTROL_VERSION,
            node_did: "did:key:owner".into(),
            nonce: "test-runtime-control-challenge".into(),
            issued_at: now.to_rfc3339(),
            operation,
        }
    }

    #[test]
    fn runtime_control_challenges_are_bounded_and_do_not_authorize_another_node_or_operation() {
        let now = Utc::now();
        let request = command(RuntimeOperation::Observe, now);
        request.validate_at(now).unwrap();
        let mut receipt = RuntimeControlReceipt {
            version: RUNTIME_CONTROL_VERSION,
            node_did: request.node_did.clone(),
            command_nonce: request.nonce.clone(),
            command_digest: request.digest().unwrap(),
            build: "a".repeat(64),
            capabilities: current_capabilities(),
            replicated_schema: None,
            lifecycle: ServeLifecycle::Ready,
            retiring: false,
            disposition: RuntimeControlDisposition::Observed,
            observed_at: now.to_rfc3339(),
            signature: vec![0; 64],
        };
        receipt.validate_for(&request, now).unwrap();
        let mut other_request = request.clone();
        other_request.issued_at = (now - Duration::seconds(1)).to_rfc3339();
        assert!(receipt.validate_for(&other_request, now).is_err());
        let mut changed = request.clone();
        changed.nonce.push_str("-other");
        assert!(receipt.validate_for(&changed, now).is_err());
        let foreign = RuntimeControlCommand {
            node_did: "did:key:other".into(),
            ..request.clone()
        };
        assert!(receipt.validate_for(&foreign, now).is_err());
        let retire = command(
            RuntimeOperation::Retire {
                expected_build: receipt.build.clone(),
            },
            now,
        );
        assert!(receipt.validate_for(&retire, now).is_err());
        receipt.disposition = RuntimeControlDisposition::Retiring;
        receipt.retiring = true;
        receipt.command_digest = retire.digest().unwrap();
        receipt.validate_for(&retire, now).unwrap();
        receipt.lifecycle = ServeLifecycle::Starting;
        receipt.validate_for(&retire, now).unwrap();
        assert!(receipt.validate_for(&request, now).is_err());
        assert!(retire.validate_at(now + Duration::seconds(61)).is_err());
        assert!(retire.validate_at(now - Duration::seconds(6)).is_err());
        assert!(receipt
            .validate_for(&retire, now + Duration::seconds(61))
            .is_err());
    }
}
