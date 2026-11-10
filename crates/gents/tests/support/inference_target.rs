use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use gents::documents::{
    BackendAuth, ConfigReferences, InferenceBackend, InferenceProfile, InferenceSampling,
};
use gents::pack::{decode_pack_config, PackInstallOptions};
use gents::store::config_client::DesiredStateApplyPlan;
use gents::BackendProviderKind;

pub const EVAL_TARGET_VARIABLE: &str = "GENTS_EVAL_TARGET";

const TARGET_VALIDATION_OWNER: &str = "did:key:inference-target-validation";

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

pub fn targets_dir() -> PathBuf {
    workspace_root().join("scripts/evals/targets")
}

#[derive(Clone, Debug)]
pub struct InferenceTarget {
    pub name: String,
    backend: InferenceBackend,
    profile: InferenceProfile,
    sampling: Option<InferenceSampling>,
}

impl InferenceTarget {
    pub fn load(selector: &str) -> Result<Self> {
        let (name, path) = resolve_selector(selector)?;
        Self::load_path(name, &path)
    }

    fn load_path(name: String, path: &Path) -> Result<Self> {
        let bytes = std::fs::read(path)
            .with_context(|| format!("reading inference target {}", path.display()))?;
        let value = serde_json::from_slice(&bytes)
            .with_context(|| format!("parsing inference target {}", path.display()))?;
        Self::decode(name, value)
            .with_context(|| format!("invalid inference target {}", path.display()))
    }

    pub fn decode(name: String, value: serde_json::Value) -> Result<Self> {
        let config = decode_pack_config(
            value,
            Some(&PackInstallOptions {
                node_did: TARGET_VALIDATION_OWNER.to_owned(),
            }),
            // Targets are literal documents: interpolating the process
            // environment could copy secrets into endpoints, reports or the DB.
            &|_| None,
            &|_, _, reference| anyhow::bail!("inference targets have no sidecars: {reference}"),
        )?;
        let authored = serde_json::to_value(&config)?;
        let authored = authored.as_object().context("configuration bundle")?;
        for key in authored.keys() {
            anyhow::ensure!(
                matches!(
                    key.as_str(),
                    "node" | "inference_backends" | "inference_profiles" | "inference_sampling"
                ),
                "an inference target authors one backend, one profile and optional sampling, not {key}"
            );
        }
        anyhow::ensure!(
            authored["node"]
                .as_object()
                .is_some_and(|node| node.len() == 1),
            "an inference target's node must be empty; the test binds its node"
        );
        let plan = DesiredStateApplyPlan::from_pack_config(&config)?;
        ConfigReferences::from_documents(
            TARGET_VALIDATION_OWNER,
            plan.documents()
                .iter()
                .map(|document| (document.collection, document.add.clone())),
        )?
        .validate()?;
        let [backend] = <[InferenceBackend; 1]>::try_from(config.inference_backends)
            .map_err(|_| anyhow::anyhow!("an inference target has exactly one backend"))?;
        let [profile] = <[InferenceProfile; 1]>::try_from(config.inference_profiles)
            .map_err(|_| anyhow::anyhow!("an inference target has exactly one profile"))?;
        let sampling = match config.inference_sampling.as_slice() {
            [] => None,
            [sampling] if profile.sampling_id.as_deref() == Some(sampling.sampling_id.as_str()) => {
                sampling.validate()?;
                Some(sampling.clone())
            }
            [_] => {
                anyhow::bail!("an inference target's sampling must be referenced by its profile")
            }
            _ => anyhow::bail!("an inference target has at most one sampling document"),
        };
        backend.validate()?;
        profile.validate()?;
        anyhow::ensure!(
            !matches!(backend.auth, BackendAuth::NodeOAuth { .. }),
            "NodeOAuth targets are not supported for fresh-node evals: \
             each trial's new node has no OAuthCredential"
        );
        anyhow::ensure!(
            !matches!(backend.auth, BackendAuth::ApiKey { .. }),
            "inference targets select credentials by environment variable, never an inline API key"
        );
        anyhow::ensure!(backend.enabled, "inference target backend is disabled");
        Ok(Self {
            name,
            backend,
            profile,
            sampling,
        })
    }

    pub fn selected_all() -> Result<Vec<Self>> {
        let value = std::env::var(EVAL_TARGET_VARIABLE).map_err(|_| {
            anyhow::anyhow!(
                "set {EVAL_TARGET_VARIABLE} to one or more inference targets (names in {} or paths)",
                targets_dir().display()
            )
        })?;
        let targets = parse_selection(&value)?
            .into_iter()
            .map(|(name, path)| Self::load_path(name, &path))
            .collect::<Result<Vec<_>>>()?;
        for target in &targets {
            target.require_credential()?;
        }
        Ok(targets)
    }

    fn require_credential(&self) -> Result<()> {
        if let BackendAuth::Environment { variable } = &self.backend.auth {
            anyhow::ensure!(
                std::env::var(variable).is_ok_and(|key| !key.trim().is_empty()),
                "inference target {} requires {variable} to be set",
                self.name
            );
        }
        Ok(())
    }

    pub fn selected() -> Result<Self> {
        let mut targets = Self::selected_all()?;
        anyhow::ensure!(
            targets.len() == 1,
            "this live test runs against one inference target; {EVAL_TARGET_VARIABLE} names {}",
            targets.len()
        );
        Ok(targets.remove(0))
    }

    pub fn model(&self) -> &str {
        &self.profile.model_name
    }

    pub fn endpoint(&self) -> &str {
        &self.backend.endpoint
    }

    pub fn backend_id(&self) -> &str {
        &self.backend.backend_id
    }

    pub fn provider_kind(&self) -> BackendProviderKind {
        self.backend.provider_kind
    }

    pub fn auth(&self) -> &BackendAuth {
        &self.backend.auth
    }

    pub fn backend(&self, node_did: &str) -> InferenceBackend {
        InferenceBackend {
            physical_doc_id: None,
            node_did: node_did.to_owned(),
            ..self.backend.clone()
        }
    }

    pub fn profile(&self, node_did: &str) -> InferenceProfile {
        InferenceProfile {
            node_did: node_did.to_owned(),
            ..self.profile.clone()
        }
    }

    pub fn sampling(&self, node_did: &str) -> Option<InferenceSampling> {
        self.sampling.as_ref().map(|sampling| InferenceSampling {
            node_did: node_did.to_owned(),
            ..sampling.clone()
        })
    }

    /// Live high-thinking scenarios retain their historical sampling only when
    /// no sampling document was selected. An explicit document preserves unset knobs.
    pub fn live_sampling(&self, node_did: &str, sampling_id: &str) -> InferenceSampling {
        InferenceSampling {
            sampling_id: sampling_id.to_owned(),
            ..self
                .sampling(node_did)
                .unwrap_or_else(|| InferenceSampling {
                    node_did: node_did.to_owned(),
                    display_name: Some("live high-thinking sampling".to_owned()),
                    temperature: Some(1.0),
                    top_p: Some(0.95),
                    ..Default::default()
                })
        }
    }

    pub async fn assert_reachable(&self) {
        let url = format!("{}/models", self.endpoint().trim_end_matches('/'));
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(15))
            .build()
            .expect("reqwest client");
        let mut request = client.get(&url);
        if let Some(key) = self
            .backend
            .auth
            .resolve_api_key()
            .unwrap_or_else(|error| panic!("target {} credential: {error:#}", self.name))
        {
            request = request.bearer_auth(key);
        }
        let name = &self.name;
        match tokio::time::timeout(Duration::from_secs(20), request.send()).await {
            Ok(Ok(r)) if r.status().is_success() => {}
            Ok(Ok(r)) => panic!(
                "target {name} endpoint {url} returned status {}",
                r.status()
            ),
            Ok(Err(e)) => panic!("target {name} endpoint {url} unreachable: {e}"),
            Err(_) => panic!("target {name} endpoint {url} timed out (not reachable)"),
        }
    }
}

fn resolve_selector(selector: &str) -> Result<(String, PathBuf)> {
    let selector = selector.trim();
    anyhow::ensure!(!selector.is_empty(), "inference target selector is blank");
    let path = if selector.contains('/') || selector.ends_with(".json") {
        let path = PathBuf::from(selector);
        if path.is_absolute() {
            path
        } else {
            workspace_root().join(path)
        }
    } else {
        targets_dir().join(format!("{selector}.json"))
    };
    let path = std::fs::canonicalize(&path).unwrap_or(path);
    let name = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .with_context(|| format!("inference target path has no name: {}", path.display()))?
        .to_owned();
    Ok((name, path))
}

pub fn parse_selection(value: &str) -> Result<Vec<(String, PathBuf)>> {
    let mut selected: Vec<(String, PathBuf)> = Vec::new();
    for selector in value
        .split(',')
        .filter(|selector| !selector.trim().is_empty())
    {
        let (name, path) = resolve_selector(selector)?;
        match selected.iter().find(|(existing, _)| *existing == name) {
            Some((_, existing)) if *existing == path => {}
            Some((_, existing)) => anyhow::bail!(
                "inference targets {} and {} share the name {name}",
                existing.display(),
                path.display()
            ),
            None => selected.push((name, path)),
        }
    }
    anyhow::ensure!(
        !selected.is_empty(),
        "{EVAL_TARGET_VARIABLE} names no inference target"
    );
    Ok(selected)
}
