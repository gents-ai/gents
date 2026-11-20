use anyhow::{Context, Result};
use gents_inference::usage_observation::UsageTrigger;
use gents_protocol::enrollment::{signed_command_staleness, SignedCommandStaleness};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UsageReadCommand {
    pub trigger: UsageTrigger,
    pub provider: Option<String>,
    pub signer_did: String,
    pub issued_at: String,
    pub nonce: String,
    pub sig: Vec<u8>,
}

const USAGE_READ_SIGNATURE_DOMAIN: &str = "gents-account-usage-read-v1";

impl UsageReadCommand {
    pub async fn signed(
        identity: &dyn gents_store::identity::NodeIdentity,
        trigger: UsageTrigger,
        provider: Option<&str>,
    ) -> Result<Self> {
        let mut command = Self {
            trigger,
            provider: provider.map(str::to_owned),
            signer_did: identity.did().to_owned(),
            issued_at: chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            nonce: uuid::Uuid::new_v4().simple().to_string(),
            sig: Vec::new(),
        };
        command.sig = identity
            .sign(&command.signing_payload())
            .await
            .context("signing the usage read request")?;
        Ok(command)
    }

    pub fn signing_payload(&self) -> Vec<u8> {
        let trigger = match self.trigger {
            UsageTrigger::Cached => "cached",
            UsageTrigger::Open => "open",
            UsageTrigger::Refresh => "refresh",
        };
        gents_protocol::enrollment::canonical_domain_payload(
            USAGE_READ_SIGNATURE_DOMAIN,
            [
                trigger,
                self.provider.as_deref().unwrap_or_default(),
                &self.signer_did,
                &self.issued_at,
                &self.nonce,
            ],
        )
    }

    pub fn validate_at(&self, now: chrono::DateTime<chrono::Utc>) -> Result<()> {
        anyhow::ensure!(self.sig.len() == 64, "invalid usage read signature length");
        let issued = chrono::DateTime::parse_from_rfc3339(&self.issued_at)
            .context("usage read issued_at")?
            .with_timezone(&chrono::Utc);
        match signed_command_staleness(issued, now) {
            Some(SignedCommandStaleness::FromFuture) => {
                anyhow::bail!("usage read request is issued in the future")
            }
            Some(SignedCommandStaleness::Expired) => anyhow::bail!("usage read request expired"),
            None => Ok(()),
        }
    }
}
