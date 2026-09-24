//! Inference targets, backend binding, runtime boot and durable observation
//! for live tests and evals.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use gents::config_client::DesiredStateApplyPlan;
use gents::defra_node::EmbeddedNode;
use gents::document_config::{
    AgentBehavior, AgentPrincipal, BackendAuth, ConfigReferences, InferenceBackend,
    InferenceProfile,
};
use gents::graphql::escape_graphql_string;
use gents::pack::{decode_pack_config, PackInstallOptions};
use gents::{
    default_behavior_id_for_agent, default_inference_profile_id_for_behavior,
    ensure_agent_principal, AgentIdentity, BackendProviderKind, Collection, DocumentRuntimeOptions,
    Gents, ToolCeiling,
};
use gents_protocol::output::reconstruction::{reconstruct_message, ObservedSegment};
use gents_protocol::output::{OutputSegment, TranscriptMessage};
use gents_protocol::request_lifecycle::RequestLifecycleState;
use serde::Deserialize;

use crate::support::interrupt::{wait_for_runtime_ready, BootedAgent};
use crate::support::{first_optional_row, TestDb};

/// Names the inference targets for live tests and evals: a comma-separated
/// list of target names (files under `scripts/evals/targets/`) or paths to
/// target files. Relative paths resolve against the workspace root.
pub const EVAL_TARGET_VARIABLE: &str = "GENTS_EVAL_TARGET";

/// Owner bound while validating a target; binding replaces it with the
/// principal under test. Target files must not author `agent_did`.
const TARGET_VALIDATION_OWNER: &str = "did:key:inference-target-validation";

fn workspace_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

pub fn targets_dir() -> PathBuf {
    workspace_root().join("scripts/evals/targets")
}

/// One inference target: a canonical configuration bundle (the
/// `pack_config.json` shape) that authors exactly one `InferenceBackend` and
/// one `InferenceProfile` selecting its model. Decoding and validation use the
/// runtime's own configuration owners, so a target is exactly what an operator
/// could apply. Credentials are selected through `BackendAuth` (for example an
/// environment variable name), never stored in the file.
#[derive(Clone, Debug)]
pub struct InferenceTarget {
    pub name: String,
    backend: InferenceBackend,
    profile: InferenceProfile,
}

impl InferenceTarget {
    /// Resolve a target name or path.
    pub fn load(selector: &str) -> Result<Self> {
        let (name, path) = resolve_selector(selector)?;
        Self::load_path(name, &path)
    }

    fn load_path(name: String, path: &Path) -> Result<Self> {
        let bytes = std::fs::read(&path)
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
                agent_did: TARGET_VALIDATION_OWNER.to_owned(),
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
                    "agent_principal" | "inference_backends" | "inference_profiles"
                ),
                "an inference target authors only one backend and one profile, not {key}"
            );
        }
        anyhow::ensure!(
            authored["agent_principal"]
                .as_object()
                .is_some_and(|principal| principal.len() == 1),
            "an inference target's agent_principal must be empty; the test binds its principal"
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
        backend.validate()?;
        profile.validate()?;
        anyhow::ensure!(
            !matches!(backend.auth, BackendAuth::PrincipalOAuth),
            "PrincipalOAuth targets are not supported for fresh-principal evals: \
             each trial's new principal has no OAuthCredential"
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
        })
    }

    /// Every target named by `GENTS_EVAL_TARGET`, in order. Each target's
    /// environment credential must be present before any trial starts.
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

    /// The single target a live test runs against.
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

    /// The target backend owned by `agent_did`.
    pub fn backend(&self, agent_did: &str) -> InferenceBackend {
        InferenceBackend {
            agent_did: agent_did.to_owned(),
            ..self.backend.clone()
        }
    }

    /// The target's model selection owned by `agent_did`. Callers that install
    /// several profiles replace `profile_id` and their own eval settings.
    pub fn profile(&self, agent_did: &str) -> InferenceProfile {
        InferenceProfile {
            agent_did: agent_did.to_owned(),
            ..self.profile.clone()
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

/// Resolve one selector to its target name and file. Names are files under
/// `scripts/evals/targets/`; relative paths resolve against the workspace root.
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

/// Parse a comma-separated selection. A repeated file is selected once; two
/// different files with one name would make report rows ambiguous.
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

/// The one target selected for a live test; panics with the selection rule.
pub fn live_target() -> InferenceTarget {
    InferenceTarget::selected().unwrap_or_else(|error| panic!("{error:#}"))
}

/// Bind one isolated live-test principal to a target: its backend, its model
/// selection as the default behavior's profile, and that behavior.
pub async fn bind_target(
    node: &EmbeddedNode,
    identity: &dyn AgentIdentity,
    target: &InferenceTarget,
) -> (String, String) {
    let agent_did = identity.did().to_string();
    let mut principal = ensure_agent_principal(node, &agent_did)
        .await
        .expect("ensure principal");
    let behavior_id = default_behavior_id_for_agent(&agent_did);
    let profile_id = default_inference_profile_id_for_behavior(&behavior_id);
    principal.default_behavior_id = Some(behavior_id.clone());
    let backend = target.backend(&agent_did);
    let profile = InferenceProfile {
        profile_id: profile_id.clone(),
        ..target.profile(&agent_did)
    };
    let behavior = AgentBehavior {
        behavior_id: behavior_id.clone(),
        agent_did: agent_did.clone(),
        display_name: Some("Live default behavior".to_string()),
        description: None,
        context_id: None,
        inference_profile_id: profile_id,
        enabled: true,
        tags: Vec::new(),
        created_at: Some(chrono::Utc::now().to_rfc3339()),
    };

    apply_live_backend_documents(node, principal, backend, profile, behavior).await;
    (agent_did, behavior_id)
}

async fn apply_live_backend_documents(
    node: &EmbeddedNode,
    principal: AgentPrincipal,
    backend: InferenceBackend,
    profile: InferenceProfile,
    behavior: AgentBehavior,
) {
    use gents::config_client::{apply_desired_state_plan, DesiredStateApplyDocument};
    let plan = DesiredStateApplyPlan::new(
        [
            (Collection::AgentPrincipal, serde_json::to_value(principal)),
            (Collection::InferenceBackend, serde_json::to_value(backend)),
            (Collection::InferenceProfile, serde_json::to_value(profile)),
            (Collection::AgentBehavior, serde_json::to_value(behavior)),
        ]
        .into_iter()
        .map(|(collection, value)| {
            let value = value.expect("serialize live configuration document");
            DesiredStateApplyDocument {
                collection,
                add: value.clone(),
                update: value,
            }
        })
        .collect(),
    )
    .expect("build live backend plan");
    gents::ConfigAccess::transact_local(node, None, "test.bind_live_backend", |txn| {
        let plan = &plan;
        Box::pin(async move { apply_desired_state_plan(txn, plan).await.map(|_| ()) })
    })
    .await
    .expect("upsert live backend");
}

pub async fn boot_live_agent(db: &TestDb, identity: Arc<dyn AgentIdentity>) -> Result<BootedAgent> {
    boot_live_agent_with_ceiling(db, identity, ToolCeiling::meta_only()).await
}

pub async fn boot_live_agent_with_ceiling(
    db: &TestDb,
    identity: Arc<dyn AgentIdentity>,
    tool_ceiling: ToolCeiling,
) -> Result<BootedAgent> {
    Ok(boot_live_agent_with_options(
        db,
        identity,
        DocumentRuntimeOptions {
            tool_ceiling,
            ..Default::default()
        },
    )
    .await?
    .0)
}

pub async fn boot_live_agent_with_options(
    db: &TestDb,
    identity: Arc<dyn AgentIdentity>,
    options: DocumentRuntimeOptions,
) -> Result<(BootedAgent, Gents)> {
    let agent = Gents::from_default_behavior_documents(db.node.clone(), identity, options).await?;
    let agent_did = agent.agent_did().to_string();
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    let handle = tokio::spawn(agent.clone().run(shutdown_rx));
    wait_for_runtime_ready(db.node.as_ref(), &agent_did).await;
    Ok((BootedAgent::new(shutdown_tx, handle, agent_did), agent))
}

fn is_terminal(state: &str) -> bool {
    RequestLifecycleState::is_terminal_str(Some(state))
}

async fn fetch_request_lifecycle(node: &EmbeddedNode, request_id: &str) -> Option<String> {
    let escaped = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentRequest(filter: {{ request_id: {{ _eq: "{escaped}" }} }}, limit: 2) {{
                lifecycle_state
            }}
        }}"#
    );
    #[derive(Deserialize)]
    struct Row {
        lifecycle_state: Option<String>,
    }
    let resp = node.execute(&query).await;
    assert!(
        !resp.has_errors(),
        "request observation failed: {:?}",
        resp.errors
    );
    let rows = resp.data.as_ref().unwrap()["AgentRequest"]
        .as_array()
        .unwrap();
    assert!(
        rows.len() <= 1,
        "live test request label is ambiguous: {request_id}"
    );
    first_optional_row::<Row>(&resp, "AgentRequest").and_then(|r| r.lifecycle_state)
}

pub async fn wait_for_request_terminal(
    node: &EmbeddedNode,
    request_id: &str,
    timeout: Duration,
) -> String {
    let deadline = tokio::time::Instant::now() + timeout;
    let mut last = String::from("<none>");
    loop {
        if let Some(state) = fetch_request_lifecycle(node, request_id).await {
            last = state.clone();
            if is_terminal(&state) {
                return state;
            }
        }
        if tokio::time::Instant::now() >= deadline {
            let escaped = escape_graphql_string(request_id);
            // Bounded lifecycle evidence only: never dump provider payloads,
            // prompts, tool arguments, or credentials into failure logs.
            let evidence = node
                .execute(&format!(
                    r#"{{
                AgentRequest(filter: {{ request_id: {{ _eq: "{escaped}" }} }}, limit: 2) {{
                    _docID lifecycle_state execution_generation execution_lease_expires_at
                    terminal_output failure_reason
                }}
                InferenceCall(filter: {{ request_id: {{ _eq: "{escaped}" }} }}, limit: 10) {{
                    call_id call_state failure_reason started_at ended_at
                }}
                AgentToolCall(filter: {{ request_id: {{ _eq: "{escaped}" }} }}, limit: 10) {{
                    _docID tool_name lifecycle_state started_at completed_at
                }}
            }}"#
                ))
                .await;
            panic!("timed out waiting for request {request_id} to terminalize; last={last}; evidence={:?}; errors={:?}", evidence.data, evidence.errors);
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

/// Resolve the one assistant message selected by a terminal request's physical
/// canonical output reference.  This deliberately has no latest-message or
/// legacy response-content fallback: callers polling a still-running request
/// observe an empty value, while a malformed terminal projection fails loudly.
pub async fn terminal_assistant_answer(node: &EmbeddedNode, request_id: &str) -> String {
    let escaped = escape_graphql_string(request_id);
    let query = format!(
        r#"{{
            AgentRequest(filter: {{ request_id: {{ _eq: "{escaped}" }} }}, limit: 2) {{
                _docID agent_did requester_did session_id terminal_output
            }}
        }}"#
    );
    #[derive(Deserialize)]
    struct RequestRow {
        #[serde(rename = "_docID")]
        doc_id: String,
        agent_did: String,
        requester_did: Option<String>,
        session_id: Option<String>,
        terminal_output: Option<gents_protocol::output::TerminalOutput>,
    }
    let resp = node.execute(&query).await;
    assert!(
        !resp.has_errors(),
        "canonical live answer request lookup failed: {:?}",
        resp.errors
    );
    let row = first_optional_row::<RequestRow>(&resp, "AgentRequest");
    let Some(row) = row else { return String::new() };
    let session_id = match row.session_id {
        Some(s) if !s.is_empty() => s,
        _ => return String::new(),
    };
    let escaped_session = escape_graphql_string(&session_id);
    let requester_filter = row
        .requester_did
        .as_deref()
        .map(|did| {
            format!(
                r#", requester_did: {{ _eq: "{}" }}"#,
                escape_graphql_string(did)
            )
        })
        .unwrap_or_else(|| ", requester_did: { _eq: null }".to_owned());
    let Some(gents_protocol::output::TerminalOutput::Message { message_doc_id }) =
        row.terminal_output
    else {
        return String::new();
    };
    let query = format!(
        r#"{{
            AgentMessage(
                filter: {{ _docID: {{ _eq: "{}" }}, request_doc_id: {{ _eq: "{}" }}, session_id: {{ _eq: "{escaped_session}" }}, agent_did: {{ _eq: "{}" }}, role: {{ _eq: "assistant" }}{requester_filter} }}, limit: 2
            ) {{ message_key session_id agent_did requester_did request_doc_id publication outcome sequence role native_id blocks created_at }}
            AgentOutputSegment(filter: {{ request_doc_id: {{ _eq: "{}" }}, agent_did: {{ _eq: "{}" }}{requester_filter} }}) {{ _docID agent_did requester_did session_id request_doc_id source ordinal writer runs payload close created_at }}
        }}"#,
        escape_graphql_string(&message_doc_id),
        escape_graphql_string(&row.doc_id),
        escape_graphql_string(&row.agent_did),
        escape_graphql_string(&row.doc_id),
        escape_graphql_string(&row.agent_did)
    );
    let resp = node.execute(&query).await;
    assert!(
        !resp.has_errors(),
        "canonical live answer header lookup failed: {:?}",
        resp.errors
    );
    let headers = resp
        .data
        .as_ref()
        .and_then(|data| data["AgentMessage"].as_array())
        .expect("canonical live answer query omitted headers");
    assert_eq!(
        headers.len(),
        1,
        "terminal output did not select one physical assistant header"
    );
    let header = serde_json::from_value::<TranscriptMessage>(headers[0].clone())
        .expect("decode selected canonical terminal header");
    let observed = resp
        .data
        .as_ref()
        .and_then(|data| data["AgentOutputSegment"].as_array())
        .expect("canonical live answer query omitted segments")
        .iter()
        .map(|value| {
            let doc_id = value["_docID"]
                .as_str()
                .expect("canonical segment omitted physical ID")
                .to_owned();
            let mut segment = value.clone();
            segment
                .as_object_mut()
                .expect("canonical segment row object")
                .remove("_docID");
            (
                doc_id,
                serde_json::from_value::<OutputSegment>(segment)
                    .expect("decode canonical output segment"),
            )
        })
        .collect::<Vec<_>>();
    let observations = observed
        .iter()
        .map(|(doc_id, segment)| ObservedSegment { doc_id, segment })
        .collect::<Vec<_>>();
    let message = reconstruct_message(&observations, &[], &[], &header)
        .expect("reconstruct selected canonical terminal message");
    gents_protocol::transcript::present_message(&message).body_markdown
}

pub async fn wait_for_assistant_answer(
    node: &EmbeddedNode,
    request_id: &str,
    timeout: Duration,
) -> String {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let answer = terminal_assistant_answer(node, request_id).await;
        if !answer.trim().is_empty() {
            return answer;
        }
        if tokio::time::Instant::now() >= deadline {
            return answer;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}
