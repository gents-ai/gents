use anyhow::{ensure, Context, Result};
use chrono::{DateTime, FixedOffset};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::document_config::{BackendModelCatalog, InferenceBackendObservation};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    node_did: Option<String>,
    content_at: String,
    observed_at: String,
    payload_digest: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Freshness {
    version: u8,
    entries: Vec<Entry>,
}

fn payload_digest(catalog: &BackendModelCatalog) -> Result<String> {
    let mut hash = Sha256::new();
    hash.update(b"gents-backend-catalog-payload-v1\0");
    hash.update(serde_json::to_vec(&catalog.models)?);
    Ok(format!("sha256:{:x}", hash.finalize()))
}

impl Freshness {
    pub(super) fn load(row: &Value, catalogs: &[BackendModelCatalog]) -> Result<Self> {
        let value = row
            .get("catalog_freshness")
            .filter(|value| !value.is_null());
        let result = match value {
            None => Self {
                version: 1,
                entries: Vec::new(),
            },
            Some(value) => serde_json::from_value::<Self>(value.clone())
                .context("decoding catalog freshness")?,
        };
        ensure!(result.version == 1, "unsupported catalog freshness version");
        let mut scopes = std::collections::BTreeSet::new();
        for entry in &result.entries {
            ensure!(
                scopes.insert(entry.node_did.as_deref()),
                "ambiguous catalog freshness credential scope"
            );
            let mut matching = catalogs
                .iter()
                .filter(|catalog| catalog.node_did == entry.node_did);
            let catalog = matching
                .next()
                .context("catalog freshness has no retained payload")?;
            ensure!(
                matching.next().is_none(),
                "ambiguous catalog freshness payload"
            );
            ensure!(
                entry.payload_digest == payload_digest(catalog)?
                    && DateTime::parse_from_rfc3339(&entry.content_at)?
                        == DateTime::parse_from_rfc3339(&catalog.observed_at)?,
                "catalog freshness does not match its retained payload"
            );
            ensure!(
                DateTime::parse_from_rfc3339(&entry.content_at)?
                    <= DateTime::parse_from_rfc3339(&entry.observed_at)?,
                "catalog freshness precedes its retained payload"
            );
        }
        Ok(result)
    }

    pub(super) fn observed_at(
        &self,
        catalog: &BackendModelCatalog,
    ) -> Result<DateTime<FixedOffset>> {
        let at = self
            .entries
            .iter()
            .find(|entry| entry.node_did == catalog.node_did)
            .map_or(catalog.observed_at.as_str(), |entry| {
                entry.observed_at.as_str()
            });
        DateTime::parse_from_rfc3339(at).context("invalid successful catalog observation timestamp")
    }

    pub(super) fn contains_scope(&self, scope: &Option<String>) -> bool {
        self.entries.iter().any(|entry| &entry.node_did == scope)
    }

    pub(super) fn record(
        &mut self,
        catalog: &BackendModelCatalog,
        observed_at: String,
    ) -> Result<()> {
        let entry = Entry {
            node_did: catalog.node_did.clone(),
            content_at: catalog.observed_at.clone(),
            observed_at,
            payload_digest: payload_digest(catalog)?,
        };
        if let Some(previous) = self
            .entries
            .iter_mut()
            .find(|previous| previous.node_did == entry.node_did)
        {
            *previous = entry;
        } else {
            self.entries.push(entry);
        }
        Ok(())
    }
}

/// The content fingerprint binds a small freshness watermark to the retained
/// payload. A mismatched binding is unavailable evidence, never permission to
/// regress freshness or silently accept another scope's catalog.
pub fn decode_backend_observation(row: Value) -> Result<InferenceBackendObservation> {
    let mut observation: InferenceBackendObservation = serde_json::from_value(row.clone())?;
    let freshness = Freshness::load(&row, &observation.catalogs)?;
    for catalog in &mut observation.catalogs {
        if let Some(entry) = freshness
            .entries
            .iter()
            .find(|entry| entry.node_did == catalog.node_did)
        {
            catalog.observed_at = entry.observed_at.clone();
        }
    }
    Ok(observation)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog(scope: Option<&str>, name: &str) -> BackendModelCatalog {
        BackendModelCatalog {
            node_did: scope.map(str::to_owned),
            observed_at: "2026-10-28T01:00:00Z".into(),
            models: vec![crate::document_config::AdvertisedModel {
                model_name: name.into(),
                display_name: None,
                context_window: None,
                max_context_window: None,
                max_output_tokens: None,
                reasoning_efforts: None,
            }],
        }
    }

    #[test]
    fn replaced_catalog_cannot_reuse_its_previous_freshness_watermark() -> Result<()> {
        let original = catalog(None, "first");
        let replacement = catalog(None, "replacement");
        let mut freshness = Freshness::load(&Value::Null, std::slice::from_ref(&original))?;
        freshness.record(&original, original.observed_at.clone())?;
        let mut row = serde_json::json!({
            "backend_id": "backend", "catalogs": {"entries": [replacement.clone()]},
            "catalog_freshness": freshness,
        });
        let error = decode_backend_observation(row.clone()).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("freshness does not match its retained payload"),
            "{error:#}"
        );
        row["catalog_freshness"] = Value::Null;
        assert_eq!(decode_backend_observation(row)?.catalogs, [replacement]);
        Ok(())
    }

    #[test]
    fn freshness_projection_is_scoped_and_requires_matching_retained_content() -> Result<()> {
        let shared = catalog(None, "shared");
        let private = catalog(Some("did:key:private"), "private");
        let catalogs = vec![shared.clone(), private.clone()];
        let mut freshness = Freshness::load(&Value::Null, &catalogs)?;
        freshness.record(&private, "2026-10-28T02:00:00Z".into())?;
        let row = serde_json::json!({
            "backend_id":"backend", "catalogs":{"entries":catalogs},
            "catalog_freshness":freshness
        });
        let decoded = decode_backend_observation(row.clone())?;
        assert_eq!(decoded.catalogs[0], shared);
        assert_eq!(decoded.catalogs[1].models, private.models);
        assert_eq!(decoded.catalogs[1].observed_at, "2026-10-28T02:00:00Z");
        for kind in [
            "scope",
            "payload",
            "content-time",
            "older",
            "duplicate",
            "version",
        ] {
            let mut invalid = row.clone();
            match kind {
                "scope" => {
                    invalid["catalog_freshness"]["entries"][0]["node_did"] = "did:key:other".into()
                }
                "payload" => {
                    invalid["catalogs"]["entries"][1]["models"][0]["model_name"] =
                        "substituted".into()
                }
                "content-time" => {
                    invalid["catalog_freshness"]["entries"][0]["content_at"] =
                        "2026-10-28T00:00:00Z".into()
                }
                "older" => {
                    invalid["catalog_freshness"]["entries"][0]["observed_at"] =
                        "2026-10-28T00:00:00Z".into()
                }
                "duplicate" => {
                    let entry = invalid["catalog_freshness"]["entries"][0].clone();
                    invalid["catalog_freshness"]["entries"]
                        .as_array_mut()
                        .unwrap()
                        .push(entry);
                }
                "version" => invalid["catalog_freshness"]["version"] = 2.into(),
                _ => unreachable!(),
            }
            assert!(decode_backend_observation(invalid).is_err(), "{kind}");
        }
        Ok(())
    }
}
