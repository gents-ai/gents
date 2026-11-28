//! Canonical filesystem-root resolution and containment.
//!
//! All configuration-time root gates use this owner. Paths are resolved again
//! by the execution/tool construction boundary; this module does not provide a
//! filesystem handle and therefore does not claim TOCTOU safety.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

use gents_session::workspace::root_resolution::{
    resolve_admitted_tool_root, resolve_configured_tool_root, resolve_tool_root, RootAdmission,
    RootDenialReason,
};

/// Why a folder cannot be a tool root whoever asks: a root is a boundary only if it
/// leaves something out, and these leave out nothing of the person's.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TooBroadRoot {
    #[error("The filesystem root is too broad. Choose your home directory or a folder inside it.")]
    FilesystemRoot,
    #[error("{} is broader than your home directory. Choose your home directory or a more specific folder.", .0.display())]
    AboveUserHome(PathBuf),
}

/// Both paths must already be canonical.
pub fn refuse_too_broad_root(
    root: &Path,
    user_home: Option<&Path>,
) -> std::result::Result<(), TooBroadRoot> {
    if root.parent().is_none() {
        return Err(TooBroadRoot::FilesystemRoot);
    }
    if user_home.is_some_and(|home| home.starts_with(root) && home != root) {
        return Err(TooBroadRoot::AboveUserHome(root.to_path_buf()));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct WorkspaceRootDocument {
    pub root_path: Option<String>,
    pub enabled: Option<bool>,
}

/// Published filesystem policy after canonical resolution and the process
/// ceiling meet; `configured` also records an explicit ceiling that failed
/// resolution so explicit policy cannot fall back to self-admission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRootPolicy {
    pub configured: bool,
    pub published: Vec<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RootExecutionGuard {
    pub agent_did: String,
    pub selected_root: Option<PathBuf>,
    pub ceiling_root: Option<PathBuf>,
    /// The node home the runtime runs under, so a refusal names the `--home`
    /// its fix needs: several nodes run on one machine, and a grant applied to
    /// another node's home leaves this one unadmitted.
    pub node_home: Option<PathBuf>,
}

/// Why the execution boundary refused an Agent's tool root. Each cause is its
/// own variant with the facts its fix needs, so a caller (the tool-surface
/// builder, then the readiness rejection) names the step instead of erasing it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ToolRootRefusal {
    #[error(
        "workspace root requires the selected Agent identity, but the held identity is {held}"
    )]
    WrongAgentIdentity { held: String },
    #[error("agent {agent_did} has active host tools but no explicit root under current WorkspaceRoot policy; set Tools.host.root to an admitted absolute workspace path")]
    NoExplicitRoot { agent_did: String },
    #[error("agent {agent_did} persisted root {} re-resolved to {} at the execution boundary", .selected_root.display(), .resolved.display())]
    ReResolved {
        agent_did: String,
        selected_root: PathBuf,
        resolved: PathBuf,
    },
    /// The WorkspaceRoot its tools read is published and enabled, but this
    /// Agent holds no `reader` relationship on it — the one cause a User grants
    /// away. Whether that is the cause is not decided here: the variant carries
    /// the facts, and [`tool_surface_reason`] answers.
    #[error("{}", ReaderMissingText::new(agent_did, root.as_deref(), home.as_deref()))]
    ReaderMissing {
        agent_did: String,
        /// The published root the grant names, as stored: what
        /// `grant-reader --path` matches. `None` when the owner's view failed,
        /// and then the text names the listing call instead.
        root: Option<PathBuf>,
        /// The tool root policy denied.
        tool_root: PathBuf,
        /// The node home the grant must run against.
        home: Option<PathBuf>,
        /// How the node publishes the root over these tools.
        publishing: RootPublishing,
        /// What the policy said about the tools' root.
        denial: RootDenialKind,
    },
    #[error("agent {agent_did} cannot admit the root {} for its tools: {denial}", .root.display())]
    Denied {
        agent_did: String,
        root: PathBuf,
        denial: RootDenialReason,
        /// How the node publishes the root over these tools.
        publishing: RootPublishing,
        /// What the policy said about the tools' root.
        denial_kind: RootDenialKind,
    },
}

impl ToolRootRefusal {
    /// Whether the fix is the reader grant a User runs.
    pub fn is_reader_missing(&self) -> bool {
        matches!(self, Self::ReaderMissing { .. })
    }

    /// How the node publishes the root over these tools, as the refusal saw it.
    pub fn publishing(&self) -> RootPublishing {
        match self {
            // The two variants a denial produced, each carrying what it saw.
            Self::ReaderMissing { publishing, .. } | Self::Denied { publishing, .. } => *publishing,
            // A refusal reached before the root policy decided anything says
            // nothing about the roots; spelled out rather than caught by `_`, so
            // a variant added later has to choose a premise.
            Self::WrongAgentIdentity { .. }
            | Self::NoExplicitRoot { .. }
            | Self::ReResolved { .. } => RootPublishing::NoPolicy,
        }
    }

    /// What the policy said about the tools' root, as the refusal saw it.
    pub fn denial(&self) -> RootDenialKind {
        match self {
            Self::ReaderMissing { denial, .. } => *denial,
            Self::Denied { denial_kind, .. } => *denial_kind,
            // No policy decision was reached, so nothing was denied.
            Self::WrongAgentIdentity { .. }
            | Self::NoExplicitRoot { .. }
            | Self::ReResolved { .. } => RootDenialKind::Admitted,
        }
    }
}

/// How the node publishes the root an Agent's host tools read: the premise Lean
/// `Runtime.StartupDegraded.RootPublishing` names. Variants follow the model's
/// declaration order, which is how the replayed cases spell them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RootPublishing {
    NoPolicy,
    PublishedEnabled,
    PublishedDisabled,
}

/// What the policy said about the root the Agent's tools read, as Lean
/// `Runtime.StartupDegraded.RootDenial` names it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RootDenialKind {
    Admitted,
    NoAllowedRoots,
    Outside,
}

impl RootDenialKind {
    /// The guard's own denial, in the model's terms. `NoAllowedRoots` is its
    /// own case; **every other** denial means the path fell outside the roots
    /// this actor may use, and folds to `Outside`.
    pub fn of(denial: RootDenialReason) -> Self {
        match denial {
            RootDenialReason::NoAllowedRoots => Self::NoAllowedRoots,
            _ => Self::Outside,
        }
    }
}

/// Lean `Runtime.StartupDegraded.toolSurfaceReason`: the reason the runtime
/// reports for a tool surface it could not build, from how the node publishes
/// the root and what the policy said about it. A root the node publishes and
/// enables that the Agent cannot read is the missing reader; every other
/// denial is the unadmitted surface.
pub fn tool_surface_reason(
    publishing: RootPublishing,
    denial: RootDenialKind,
) -> gents_protocol::node_readiness::AgentReadinessUnavailableReason {
    use gents_protocol::node_readiness::AgentReadinessUnavailableReason as Reason;
    match (publishing, denial) {
        (
            RootPublishing::PublishedEnabled,
            RootDenialKind::NoAllowedRoots | RootDenialKind::Outside,
        ) => Reason::WorkspaceRootReaderMissing,
        _ => Reason::ToolSurfaceUnavailable,
    }
}

/// The reader-missing cause is the one a copy-pasteable grant fixes, and it has
/// four spellings: root and home known, root known, home known, neither. It is
/// its own value so the variant's `#[error]` stays a single reference and every
/// other variant states itself in its own.
pub struct ReaderMissingText<'a> {
    agent_did: &'a str,
    root: Option<&'a Path>,
    home: Option<&'a Path>,
}

impl<'a> ReaderMissingText<'a> {
    fn new(agent_did: &'a str, root: Option<&'a Path>, home: Option<&'a Path>) -> Self {
        Self {
            agent_did,
            root,
            home,
        }
    }
}

impl std::fmt::Display for ReaderMissingText<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use gents_protocol::node_readiness as readiness;
        let Self {
            agent_did,
            root,
            home,
        } = self;
        match (root, home) {
            (Some(root), Some(home)) => formatter.write_str(
                &readiness::workspace_root_reader_missing_message(
                    agent_did,
                    &root.to_string_lossy(),
                    &home.to_string_lossy(),
                ),
            ),
            (Some(root), None) => formatter.write_str(
                &readiness::workspace_root_reader_missing_message_without_home(
                    agent_did,
                    &root.to_string_lossy(),
                ),
            ),
            (None, Some(home)) => write!(
                formatter,
                "{} has host tools under a workspace root, which it is not a reader of; find it with `{}` and grant the reader it names, then start the agent again",
                agent_did,
                readiness::workspace_root_list_call(home),
            ),
            (None, None) => formatter.write_str(readiness::WORKSPACE_ROOT_READER_MISSING_MESSAGE),
        }
    }
}

/// The [`ToolRootRefusal`] inside a tool-surface build failure, found through
/// its source chain. This is the one classification the readiness demotion
/// makes: a missing reader is a cause with its own fix, so the Agent is not
/// reported with the generic `tool_surface_unavailable`.
pub fn tool_root_refusal<'a>(
    error: &'a (dyn std::error::Error + 'static),
) -> Option<&'a ToolRootRefusal> {
    gents_protocol::error_chain::find_source::<ToolRootRefusal>(error)
}

impl RootExecutionGuard {
    pub async fn validate(
        &self,
        node: &std::sync::Arc<defra_node::Node>,
        agent: &dyn gents_store::identity::NodeIdentity,
    ) -> Result<()> {
        if agent.did() != self.agent_did {
            return Err(ToolRootRefusal::WrongAgentIdentity {
                held: agent.did().to_owned(),
            }
            .into());
        }
        let access = gents_store::config_client::ConfigAccess::local_managed_agent(
            node.clone(),
            identity::Did::new(&self.agent_did)?,
            agent,
        )?;
        let node_did = node.node_identity_did().unwrap_or_default();
        let policy = access
            .transact_readonly("workspace_root.execution", |txn| {
                Box::pin(async move {
                    load_workspace_root_policy_in_txn(txn, self.ceiling_root.as_deref(), node_did)
                        .await
                })
            })
            .await?;
        let Some(selected_root) = self.selected_root.as_deref() else {
            if policy.configured {
                return Err(ToolRootRefusal::NoExplicitRoot {
                    agent_did: self.agent_did.clone(),
                }
                .into());
            }
            return Ok(());
        };
        match policy.admit(selected_root)? {
            RootAdmission::Admitted(root) if root == selected_root => Ok(()),
            RootAdmission::Admitted(root) => Err(ToolRootRefusal::ReResolved {
                agent_did: self.agent_did.clone(),
                selected_root: selected_root.to_path_buf(),
                resolved: root,
            }
            .into()),
            RootAdmission::Denied {
                reason: denial,
                resolved_candidate,
            } => Err(self
                .refusal_for_denial(node, resolved_candidate, denial, policy.configured)
                .await),
        }
    }

    /// The typed refusal for a denied root. A configured publishing policy
    /// denies for one of two reasons: nothing this Agent may use is published
    /// (the Agent holds no reader at all), or the path falls outside every
    /// published root (possibly one it reads, while its tools point at another
    /// it does not). Both are a missing `reader` on a root the node publishes
    /// and enables — so both name that grant, on the covering root; a denial
    /// that covers no published root keeps the plain [`ToolRootRefusal::Denied`].
    async fn refusal_for_denial(
        &self,
        node: &std::sync::Arc<defra_node::Node>,
        denied: PathBuf,
        denial: RootDenialReason,
        policy_configured: bool,
    ) -> anyhow::Error {
        let covering = self.published_roots_covering(node, &denied).await;
        // The facts the refusal carries, so Lean `toolSurfaceReason` decides the
        // cause rather than this match: a root the node publishes and enables
        // over the tools, plus a denial, is the missing reader.
        match covering.into_iter().next() {
            // `PublishedEnabled` holds by construction: `published_roots_covering`
            // keeps only enabled roots, so a covering root the Agent cannot read
            // is one the node publishes and enables over its tools.
            Some(root) => ToolRootRefusal::ReaderMissing {
                agent_did: self.agent_did.clone(),
                root: Some(root),
                tool_root: denied,
                home: self.node_home.clone(),
                publishing: RootPublishing::PublishedEnabled,
                denial: RootDenialKind::of(denial),
            }
            .into(),
            // No enabled root covers the tools, so the denial is about the path.
            // The facts say which of the two ways: a policy with nothing enabled
            // over them, or no policy at all. The model keeps the generic surface
            // for both, whatever the denial.
            None => ToolRootRefusal::Denied {
                agent_did: self.agent_did.clone(),
                root: denied,
                denial,
                publishing: if policy_configured {
                    RootPublishing::PublishedDisabled
                } else {
                    RootPublishing::NoPolicy
                },
                denial_kind: RootDenialKind::of(denial),
            }
            .into(),
        }
    }

    /// The published, enabled roots this node keeps that contain `path` in their
    /// resolution, read as the node's owner rather than the Agent: an Agent
    /// without a reader sees no roots at all, which is the very cause a refusal
    /// has to name. The answer is each root's stored path, which is what
    /// `grant-reader --path` matches.
    async fn published_roots_covering(
        &self,
        node: &std::sync::Arc<defra_node::Node>,
        path: &Path,
    ) -> Vec<PathBuf> {
        let Some(node_did) = node.node_identity_did().map(str::to_owned) else {
            return Vec::new();
        };
        let Ok(actor) = identity::Did::new(&node_did) else {
            return Vec::new();
        };
        let owner = gents_store::config_client::ConfigAccess::local_as(node.clone(), actor);
        // The owner's view serves the refusal's wording alone: if it fails, the
        // cause stays a plain denial rather than an error about a call the Agent
        // cannot make.
        owner
            .transact_readonly("workspace_root.owner_covering", |txn| {
                let path = path.to_path_buf();
                let node_did = node_did.clone();
                Box::pin(
                    async move { published_roots_covering_in_txn(txn, &node_did, &path).await },
                )
            })
            .await
            .unwrap_or_default()
    }
}

impl WorkspaceRootPolicy {
    pub(crate) fn admit(&self, candidate: &Path) -> Result<RootAdmission> {
        anyhow::ensure!(
            candidate.is_absolute(),
            "configured tool root must be absolute, got {}",
            candidate.display()
        );
        // With no operator policy and no process ceiling, preserve the legacy
        // embedder contract: an authored absolute root narrows to its own
        // canonical resolution; configured/revoked policy stays fail-closed.
        if !self.configured && self.published.is_empty() {
            return resolve_configured_tool_root(candidate).map(RootAdmission::Admitted);
        }
        resolve_admitted_tool_root(candidate, self.published.iter().map(PathBuf::as_path))
    }
}

/// The caller supplies all configured roots; actor-filtered database results
/// must instead use the completeness-aware loader below.
pub fn project_workspace_root_policy(
    documents: Vec<WorkspaceRootDocument>,
    ceiling_root: Option<&Path>,
) -> WorkspaceRootPolicy {
    project_workspace_root_policy_with_completeness(documents, ceiling_root, true)
}

fn project_workspace_root_policy_with_completeness(
    documents: Vec<WorkspaceRootDocument>,
    ceiling_root: Option<&Path>,
    complete: bool,
) -> WorkspaceRootPolicy {
    let resolved_ceiling = ceiling_root.and_then(|ceiling| resolve_tool_root(ceiling).ok());
    let configured = !complete
        || !documents.is_empty()
        || (ceiling_root.is_some() && resolved_ceiling.is_none());

    let candidates = if configured {
        documents
            .into_iter()
            .filter(|document| document.enabled.unwrap_or(false))
            .filter_map(|document| document.root_path)
            .filter_map(|root| {
                let root = root.trim();
                (!root.is_empty() && Path::new(root).is_absolute()).then(|| root.to_owned())
            })
            .collect::<Vec<_>>()
    } else {
        resolved_ceiling
            .as_ref()
            .map(|ceiling| ceiling.path.to_string_lossy().into_owned())
            .into_iter()
            .collect()
    };

    let mut published = std::collections::BTreeSet::new();
    for candidate in candidates {
        let admitted = match &resolved_ceiling {
            Some(ceiling) => {
                resolve_admitted_tool_root(&PathBuf::from(&candidate), [ceiling.path.as_path()])
                    .ok()
                    .and_then(RootAdmission::admitted)
            }
            None if ceiling_root.is_some() => None,
            None => resolve_configured_tool_root(Path::new(&candidate)).ok(),
        };
        if let Some(root) = admitted {
            published.insert(root);
        }
    }

    WorkspaceRootPolicy {
        configured,
        published: published.into_iter().collect(),
    }
}

fn decode_workspace_root_rows(value: &serde_json::Value) -> Result<Vec<WorkspaceRootDocument>> {
    let rows = value
        .get("data")
        .and_then(|data| data.get("WorkspaceRoot"))
        .cloned()
        .context("WorkspaceRoot policy query omitted rows")?;
    serde_json::from_value(rows).context("decoding WorkspaceRoot policy rows")
}

fn workspace_root_query(complete: bool, node_did: &str) -> String {
    if complete {
        "{ WorkspaceRoot { root_path enabled } }".to_owned()
    } else {
        format!(
            "{{ WorkspaceRoot(filter: {{node_did: {{_eq: \"{}\"}}}}) {{ root_path enabled }} }}",
            gents_store::graphql::escape_graphql_string(node_did)
        )
    }
}

pub(crate) async fn load_workspace_root_policy_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    ceiling_root: Option<&Path>,
    node_did: &str,
) -> Result<WorkspaceRootPolicy> {
    let complete = txn.collection_has_no_document_policy("WorkspaceRoot");
    let query = workspace_root_query(complete, node_did);
    let response = txn
        .execute(&query)
        .await
        .context("querying WorkspaceRoot policy in configuration transaction")?;
    Ok(project_workspace_root_policy_with_completeness(
        decode_workspace_root_rows(&response)?,
        ceiling_root,
        complete,
    ))
}

/// The stored path of the enabled WorkspaceRoot, visible to the transaction's
/// actor, whose resolution contains `root`. Reader grants name a root by this
/// exact stored path, which can differ from its canonical resolution.
pub(crate) async fn published_root_covering_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    root: &Path,
) -> Result<Option<String>> {
    let complete = txn.collection_has_no_document_policy("WorkspaceRoot");
    let response = txn
        .execute(&workspace_root_query(complete, node_did))
        .await
        .context("querying WorkspaceRoot paths in configuration transaction")?;
    for document in decode_workspace_root_rows(&response)? {
        let Some(path) = document
            .root_path
            .filter(|_| document.enabled.unwrap_or(false))
        else {
            continue;
        };
        let Ok(published) = resolve_configured_tool_root(Path::new(path.trim())) else {
            continue;
        };
        if matches!(
            resolve_admitted_tool_root(root, [published.as_path()]),
            Ok(RootAdmission::Admitted(_))
        ) {
            return Ok(Some(path));
        }
    }
    Ok(None)
}

/// The stored paths of every enabled WorkspaceRoot, visible to the
/// transaction's actor, whose resolution contains `path` — or that `path`
/// contains. An Agent that reads root A while its tools point at published root
/// B is denied for B, so the covering set is what names the missing grant. The
/// transaction's actor is the node's owner: an Agent with no reader sees no
/// roots at all, which is the cause itself.
pub(crate) async fn published_roots_covering_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    path: &Path,
) -> Result<Vec<PathBuf>> {
    let complete = txn.collection_has_no_document_policy("WorkspaceRoot");
    let response = txn
        .execute(&workspace_root_query(complete, node_did))
        .await
        .context("querying WorkspaceRoot paths in configuration transaction")?;
    let mut covering = Vec::new();
    for document in decode_workspace_root_rows(&response)? {
        let Some(stored) = document
            .root_path
            .filter(|_| document.enabled.unwrap_or(false))
        else {
            continue;
        };
        let Ok(published) = resolve_configured_tool_root(Path::new(stored.trim())) else {
            continue;
        };
        // Admission requires the tool root to sit inside an allowed root, so a
        // published root that merely sits under a broader tool root does not
        // cover it: the grant it named would not admit the tools.
        if path.starts_with(&published) {
            covering.push(PathBuf::from(stored.trim()));
        }
    }
    covering.sort();
    covering.dedup();
    Ok(covering)
}

/// The WorkspaceRoot a User grants an Agent reader on so it can run the host
/// tools its Tools select. Agent creation names the same root in its receipt
/// ([`reader_grant_step`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolRootReader {
    /// The Tools select no filesystem root, or this node publishes no roots.
    NotNeeded,
    /// The stored path of the enabled published root covering the tool root.
    Published(String),
    /// No published root visible to the reader covers this tool root.
    Unpublished(String),
}

pub(crate) async fn tools_root_reader_in_txn(
    txn: &gents_store::config_client::ConfigApplyTxn<'_>,
    node_did: &str,
    policy: &WorkspaceRootPolicy,
    tools: &gents_documents::Tools,
) -> Result<ToolRootReader> {
    let root = tools.host.as_ref().and_then(|host| host.root.clone());
    match (
        policy.configured
            && crate::surface::ResolvedToolSelection::from_document(tools)?
                .requires_filesystem_root(),
        root,
    ) {
        (true, Some(root)) => Ok(
            match published_root_covering_in_txn(txn, node_did, Path::new(&root)).await? {
                Some(published) => ToolRootReader::Published(published),
                None => ToolRootReader::Unpublished(root),
            },
        ),
        _ => Ok(ToolRootReader::NotNeeded),
    }
}

/// [`ToolRootReader`] for the Tools `agent_did` selects, read as `access`'s actor.
pub async fn agent_tool_root_reader(
    access: &gents_store::config_client::ConfigAccess,
    node_did: &str,
    agent_did: &str,
) -> Result<ToolRootReader> {
    access
        .transact_readonly("workspace_root.agent_tool_root", |txn| {
            Box::pin(async move {
                let Some(tools) =
                    gents_documents::load_agent_tools_in_txn(txn, node_did, agent_did).await?
                else {
                    return Ok(ToolRootReader::NotNeeded);
                };
                let policy = load_workspace_root_policy_in_txn(txn, None, node_did).await?;
                tools_root_reader_in_txn(txn, node_did, &policy, &tools).await
            })
        })
        .await
}

/// [`ToolRootReader`] for host tools run under `root`, read as `access`'s
/// actor: what a pack's preview shows before any agent of it exists.
pub async fn tool_root_reader(
    access: &gents_store::config_client::ConfigAccess,
    node_did: &str,
    root: &str,
) -> Result<ToolRootReader> {
    access
        .transact_readonly("workspace_root.tool_root", |txn| {
            Box::pin(async move {
                let policy = load_workspace_root_policy_in_txn(txn, None, node_did).await?;
                if !policy.configured {
                    return Ok(ToolRootReader::NotNeeded);
                }
                Ok(
                    match published_root_covering_in_txn(txn, node_did, Path::new(root)).await? {
                        Some(published) => ToolRootReader::Published(published),
                        None => ToolRootReader::Unpublished(root.to_owned()),
                    },
                )
            })
        })
        .await
}

/// Those of `agents` whose Tools select host tools under a published root,
/// each with its Tools id, its tool root and the root it needs reader on, as `access`'s
/// actor sees them: what the agents a pack ships need before they run. A User
/// grants each with
/// [`grant_workspace_root_reader`](super::grant_workspace_root_reader); an
/// agent that installed the pack reports [`reader_grant_step`] instead.
pub async fn agents_tool_root_readers(
    access: &gents_store::config_client::ConfigAccess,
    node_did: &str,
    agents: &[String],
) -> Result<Vec<(String, String, String, ToolRootReader)>> {
    access
        .transact_readonly("workspace_root.agents_tool_roots", |txn| {
            Box::pin(async move {
                let policy = load_workspace_root_policy_in_txn(txn, None, node_did).await?;
                let mut readers = Vec::new();
                for agent_did in agents {
                    let Some(tools) =
                        gents_documents::load_agent_tools_in_txn(txn, node_did, agent_did).await?
                    else {
                        continue;
                    };
                    let Some(root) = tools.host.as_ref().and_then(|host| host.root.clone()) else {
                        continue;
                    };
                    match tools_root_reader_in_txn(txn, node_did, &policy, &tools).await? {
                        ToolRootReader::NotNeeded => {}
                        reader => {
                            readers.push((agent_did.clone(), tools.tools_id.clone(), root, reader));
                        }
                    }
                }
                Ok(readers)
            })
        })
        .await
}

/// The step a Node administrator takes so `agent_did` may run host tools under
/// a published root. WorkspaceRoot `reader` is managed only by the Node's owner
/// and administrators (`workspace_roots.reader_manager`), so an Agent creating
/// another Agent never grants it, even with relationship delegation.
/// `known_missing` is true for a newly created Agent, which holds no
/// relationship yet; otherwise the caller cannot see the Agent's relationships.
pub(crate) fn reader_grant_step(
    agent_did: &str,
    published_root: Option<&str>,
    home: Option<&std::path::Path>,
    known_missing: bool,
) -> serde_json::Value {
    let (answer, user_command) = match (published_root, home) {
        (Some(root), Some(home)) => (
            if known_missing {
                format!("Saved, but {agent_did} cannot use its host tools yet: a User must grant it reader on the published WorkspaceRoot {root}. Until then the runtime cannot load its tools and reports the agent unavailable.")
            } else {
                format!("{agent_did} uses host tools only if a User granted it reader on the published WorkspaceRoot {root}; without that grant the runtime cannot load its tools and the agent is unavailable (agent_new to it is not offered).")
            },
            gents_protocol::node_readiness::workspace_root_reader_grant(
                root,
                agent_did,
                &home.to_string_lossy(),
            ),
        ),
        (Some(root), None) => (
            if known_missing {
                format!("Saved, but {agent_did} cannot use its host tools yet: a User must grant it reader on the published WorkspaceRoot {root}. Until then the runtime cannot load its tools and reports the agent unavailable.")
            } else {
                format!("{agent_did} uses host tools only if a User granted it reader on the published WorkspaceRoot {root}; without that grant the runtime cannot load its tools and the agent is unavailable (agent_new to it is not offered).")
            },
            format!(
                "gents config workspace-root grant-reader --path {root} --agent-did {agent_did}"
            ),
        ),
        (None, home) => (
            format!("{agent_did} uses host tools read through a WorkspaceRoot this node has not published. Until a User publishes one and grants {agent_did} reader on it, the runtime cannot load its tools and the agent is unavailable."),
            match home {
                Some(home) => gents_protocol::node_readiness::workspace_root_list_call(home),
                None => "gents config workspace-root list".to_owned(),
            },
        ),
    };
    let target_guidance = home
        .is_none()
        .then_some(gents_protocol::node_readiness::TARGET_GUIDANCE);
    serde_json::json!({
        "target_guidance": target_guidance,
        "state": if known_missing { "waiting_for_user" } else { "requires_user_grant" },
        "published_root": published_root,
        "answer": answer,
        "user_command": user_command,
        "how_to_proceed": gents_protocol::node_readiness::reader_grant_how_to_proceed(
            target_guidance.is_some()
        ),
    })
}

pub async fn load_workspace_root_policy(
    node: &defra_node::Node,
    ceiling_root: Option<&Path>,
) -> Result<WorkspaceRootPolicy> {
    let complete = node
        .get_collection("WorkspaceRoot")
        .ok()
        .flatten()
        .is_some_and(|collection| collection.policy.is_none());
    let node_did = if complete {
        ""
    } else {
        node.node_identity_did()
            .context("WorkspaceRoot policy requires the selected Node identity")?
    };
    let query = workspace_root_query(complete, node_did);
    let response = gents_store::graphql::graphql_with_transaction_retry(
        node,
        &query,
        "querying WorkspaceRoot policy",
    )
    .await?;
    let value = serde_json::json!({"data": response.data.unwrap_or(serde_json::Value::Null)});
    Ok(project_workspace_root_policy_with_completeness(
        decode_workspace_root_rows(&value)?,
        ceiling_root,
        complete,
    ))
}

/// Read-only startup check that the configured host tool root (the process
/// ceiling) is covered by a WorkspaceRoot published for this Node. A mismatch
/// is reported with the operator's fix; nothing is written here.
pub async fn ensure_configured_root_published(
    node: &defra_node::Node,
    configured_root: &Path,
) -> Result<()> {
    let policy = load_workspace_root_policy(node, Some(configured_root)).await?;
    configured_root_coverage(&policy, configured_root)
}

/// The folder a command Task runs in, checked at each launch: the operator's
/// tool root while a WorkspaceRoot published for this Node still covers it,
/// or the runtime cwd while no WorkspaceRoot policy is configured, as a task
/// hook of an Agent without a root runs.
pub async fn node_command_root(
    node: &defra_node::Node,
    ceiling_root: Option<&Path>,
) -> Result<PathBuf> {
    let policy = load_workspace_root_policy(node, ceiling_root).await?;
    match ceiling_root {
        Some(root) => {
            configured_root_coverage(&policy, root)?;
            resolve_configured_tool_root(root)
        }
        None => {
            anyhow::ensure!(
                !policy.configured,
                "WorkspaceRoot policy is configured but this node has no operator tool root, so a command Task has nowhere admitted to run; start the node with a tool root"
            );
            std::env::current_dir().context("resolving the runtime cwd for a command Task")
        }
    }
}

/// [`ensure_configured_root_published`] through a running Node's
/// configuration access, as `node_did`'s principal, for a caller that must
/// refuse a root before it replaces that runtime.
pub async fn ensure_root_published_for_node(
    access: &gents_store::config_client::ConfigAccess,
    node_did: &str,
    configured_root: &Path,
) -> Result<()> {
    let policy = access
        .transact_readonly("workspace_root.configured", |txn| {
            Box::pin(async move {
                load_workspace_root_policy_in_txn(txn, Some(configured_root), node_did).await
            })
        })
        .await?;
    configured_root_coverage(&policy, configured_root)
}

pub(crate) fn configured_root_coverage(
    policy: &WorkspaceRootPolicy,
    configured_root: &Path,
) -> Result<()> {
    match policy.admit(configured_root)? {
        RootAdmission::Admitted(_) => Ok(()),
        denied @ RootAdmission::Denied { .. } => anyhow::bail!(
            "configured host tool root {} is not published as a WorkspaceRoot for this Node ({}). Choose this folder in the desktop app while the Node is running, or publish it with `gents config workspace-root set --graphql <url> --node-did <node-did> --path <folder> --reader-agent <engineer-agent-did>`, then start the server again.",
            configured_root.display(),
            denied.denial_reason().expect("denied outcome has a reason")
        ),
    }
}

pub fn admit_authored_root(
    policy: &WorkspaceRootPolicy,
    authored_root: &str,
    stored_grant: bool,
) -> Result<Option<PathBuf>> {
    let root = authored_root.trim();
    if root.is_empty() {
        anyhow::ensure!(
            !policy.configured,
            "configured WorkspaceRoot policy requires an explicit root"
        );
        return Ok(None);
    }
    match policy.admit(Path::new(root))? {
        RootAdmission::Admitted(root) => Ok(Some(root)),
        denied @ RootAdmission::Denied { .. } => {
            // A stored grant containing a published root stays admitted
            // unclamped; a grant authored by this write must be strictly
            // admitted so it cannot pre-position a wide grant that activates
            // when the operator later widens authority. Only a disjoint grant
            // is refused.
            if stored_grant {
                let overlaps = policy.published.iter().any(|published| {
                    matches!(
                        resolve_admitted_tool_root(published, [Path::new(root)]),
                        Ok(RootAdmission::Admitted(_))
                    )
                });
                if overlaps {
                    return resolve_configured_tool_root(Path::new(root)).map(Some);
                }
            }
            anyhow::bail!(
                "Tools.host.root {:?} is not admitted by published WorkspaceRoot policy: {}",
                root,
                denied.denial_reason().expect("denied outcome has a reason")
            )
        }
    }
}

#[derive(Debug, thiserror::Error)]
#[error("active host tools require an explicit root in Tools.host.root while WorkspaceRoot policy is configured. Set host.root to an admitted absolute workspace path (a sibling of host.files and host.bash). In config, read tools get first, then tools update with set.host preserving its other fields; do not put root inside host.files. For agent create, omit --preset (options.preset) for database/plugin-only agents or pass --root <path> (options.root)")]
pub struct ExplicitHostRootRequired;

/// Validate and canonicalize the existing `Tools.host.root` field against a
/// published policy. `stored_root` is the `host.root` value the document held
/// before this write; the containing-grant tolerance applies only when the
/// authored root is that stored value. This mutates only the canonical Tools
/// document shape; it does not introduce a parallel root field.
pub fn canonicalize_tools_root(
    tools: &mut gents_documents::Tools,
    policy: &WorkspaceRootPolicy,
    stored_root: Option<&str>,
) -> Result<()> {
    let selection = crate::surface::ResolvedToolSelection::from_document(tools)?;
    let requires_root = selection.requires_filesystem_root();
    let authored_root = tools
        .host
        .as_ref()
        .and_then(|host| host.root.as_deref())
        .map(str::trim)
        .filter(|root| !root.is_empty());

    let Some(authored_root) = authored_root else {
        if policy.configured && requires_root {
            return Err(ExplicitHostRootRequired.into());
        }
        if let Some(host) = tools.host.as_mut() {
            host.root = None;
        }
        return Ok(());
    };

    let stored_grant = stored_root.map(str::trim) == Some(authored_root);
    let canonical = admit_authored_root(policy, authored_root, stored_grant)?
        .expect("nonblank authored root produces an admitted path");
    tools
        .host
        .as_mut()
        .expect("authored root came from host")
        .root = Some(canonical.to_string_lossy().into_owned());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gents_session::workspace::root_resolution::RootDenialReason;

    #[test]
    fn reader_step_keeps_commands_runnable_and_home_guidance_separate() {
        for root in [None, Some("/workspace")] {
            for home in [None, Some(Path::new("/node/home"))] {
                let step = reader_grant_step("did:key:helper", root, home, true);
                let command = step["user_command"].as_str().unwrap();
                assert!(!command.contains('('));
                assert!(!command.contains("Add --home"));
                assert_eq!(step["target_guidance"].is_null(), home.is_some());
                assert_eq!(
                    step["how_to_proceed"]
                        .as_str()
                        .unwrap()
                        .contains("target_guidance"),
                    home.is_none(),
                    "the step names target_guidance only when it carries one"
                );
                if home.is_some() {
                    assert!(command.contains("--home /node/home"));
                }
            }
        }
    }

    #[test]
    fn workspace_root_fallback_requires_complete_visibility() {
        gents_lean_vocab::families::workspace_root_completeness::replay(|i| {
            let documents = if i.visible_configured {
                vec![WorkspaceRootDocument {
                    root_path: None,
                    enabled: Some(false),
                }]
            } else {
                Vec::new()
            };
            let policy =
                project_workspace_root_policy_with_completeness(documents, None, i.complete);
            !policy.configured && policy.published.is_empty()
        });
    }

    #[tokio::test]
    async fn filtered_native_workspace_roots_do_not_enable_fallback() -> anyhow::Result<()> {
        use gents_store::identity::NodeIdentity;
        let directory = tempfile::tempdir()?;
        let owner = gents_store::identity::KeyIdentity::load_or_create(
            directory.path().join("owner.key"),
            None,
        )?;
        let binding = gents_store::identity::provision_agent_identity(&owner)?;
        let agent = gents_store::identity::resolve_agent_identity(&owner, &binding)?;
        let node = std::sync::Arc::new(
            defra_node::Node::builder()
                .enable_signing()
                .with_node_identity_did(owner.did())
                .with_node_acp_enabled()
                .build()
                .await?,
        );
        let context =
            defra_core::RequestContext::default().with_identity(Some(owner.did().to_owned()));
        let policy_id = node.add_dac_policy_as(&context, "name: WorkspaceRootCompleteness\nresources:\n  - name: roots\n    permissions:\n      - name: read\n      - name: update\n      - name: delete\n").await?;
        node.add_collection_as(&context, &gents_protocol::schemas::HomePolicyIds::node_store().render(&format!(
            "type WorkspaceRoot @policy(id: \"{policy_id}\", resource: \"roots\") {{ node_did: String root_path: String enabled: Boolean }}"
        ))).await?;
        let owner_access =
            gents_store::config_client::ConfigAccess::local_as(node.clone(), owner.did().parse()?);
        for permission in [
            gents_store::config_client::NodePermission::DocumentRead,
            gents_store::config_client::NodePermission::CollectionGet,
        ] {
            owner_access
                .grant_node_permission(&agent.did().parse()?, permission)
                .await?;
        }
        owner_access.write("test.root.hidden", &format!(
            "mutation {{ create_WorkspaceRoot(input: {{root_path: \"{}\", enabled: true}}) {{_docID}} }}",
            gents_store::graphql::escape_graphql_string(&directory.path().to_string_lossy())
        )).await?;
        let access = gents_store::config_client::ConfigAccess::local_managed_agent(
            node.clone(),
            agent.did().parse()?,
            agent.as_ref(),
        )?;
        let rows = access.execute("{ WorkspaceRoot { _docID } }").await?;
        assert_eq!(rows["data"]["WorkspaceRoot"].as_array().unwrap().len(), 0);
        let owner_did = owner.did().to_owned();
        let projected = access
            .transact_readonly("test.root.filtered", |txn| {
                Box::pin(async { load_workspace_root_policy_in_txn(txn, None, &owner_did).await })
            })
            .await?;
        assert!(projected.configured);
        assert!(projected.published.is_empty());
        assert!(matches!(
            projected.admit(directory.path())?,
            RootAdmission::Denied { .. }
        ));
        node.shutdown().await?;
        Ok(())
    }

    #[test]
    fn disabled_explicit_policy_does_not_fall_back_to_ceiling() {
        let ceiling = tempfile::tempdir().expect("ceiling");
        let policy = project_workspace_root_policy(
            vec![WorkspaceRootDocument {
                root_path: Some(ceiling.path().to_string_lossy().into_owned()),
                enabled: Some(false),
            }],
            Some(ceiling.path()),
        );
        assert!(policy.configured);
        assert!(policy.published.is_empty());
    }

    #[test]
    fn blank_and_relative_explicit_rows_fail_closed_without_cwd_projection() {
        let ceiling = tempfile::tempdir().expect("ceiling");
        let policy = project_workspace_root_policy(
            vec![
                WorkspaceRootDocument {
                    root_path: Some("   ".to_string()),
                    enabled: Some(true),
                },
                WorkspaceRootDocument {
                    root_path: Some("relative/workspace".to_string()),
                    enabled: Some(true),
                },
            ],
            Some(ceiling.path()),
        );
        assert!(policy.configured);
        assert!(policy.published.is_empty());
    }

    #[test]
    fn explicit_rows_trim_once_before_publication() {
        let ceiling = tempfile::tempdir().expect("ceiling");
        let allowed = ceiling.path().join("allowed");
        std::fs::create_dir_all(&allowed).expect("allowed root");
        let policy = project_workspace_root_policy(
            vec![WorkspaceRootDocument {
                root_path: Some(format!("  {}  ", allowed.display())),
                enabled: Some(true),
            }],
            Some(ceiling.path()),
        );
        assert_eq!(
            policy.published,
            vec![std::fs::canonicalize(allowed).expect("canonical")]
        );
    }

    #[test]
    fn cli_only_tools_need_a_root_under_explicit_policy() {
        let allowed = tempfile::tempdir().expect("allowed root");
        let policy = project_workspace_root_policy(
            vec![WorkspaceRootDocument {
                root_path: Some(allowed.path().to_string_lossy().into_owned()),
                enabled: Some(true),
            }],
            None,
        );
        let mut tools = gents_documents::Tools {
            host: Some(gents_documents::HostTools {
                cli: vec![gents_documents::CliTool {
                    name: "git".to_string(),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            ..Default::default()
        };
        let error = canonicalize_tools_root(&mut tools, &policy, None)
            .expect_err("CLI uses the shared root and cannot inherit cwd under explicit policy");
        let message = error.to_string();
        assert!(message.contains("require an explicit root"), "{message}");
        assert!(
            message.contains("omit --preset") && message.contains("pass --root"),
            "{message}"
        );
    }

    #[test]
    fn stored_grant_containing_the_operator_root_canonicalizes_unclamped_under_default_policy() {
        let workspace = tempfile::tempdir().expect("workspace");
        let project = workspace.path().join("project");
        std::fs::create_dir_all(&project).expect("narrowed operator root");
        let policy = project_workspace_root_policy(Vec::new(), Some(&project));

        assert_eq!(
            admit_authored_root(
                &policy,
                workspace.path().to_str().expect("utf-8 root"),
                true
            )
            .expect("a stored grant containing the operator root stays admitted"),
            Some(std::fs::canonicalize(workspace.path()).expect("canonical workspace")),
            "the grant is canonicalized unclamped; the selection/ceiling meet narrows it before use"
        );
    }

    #[test]
    fn newly_authored_grant_containing_the_operator_root_refuses_under_default_policy() {
        let workspace = tempfile::tempdir().expect("workspace");
        let project = workspace.path().join("project");
        std::fs::create_dir_all(&project).expect("narrowed operator root");
        let policy = project_workspace_root_policy(Vec::new(), Some(&project));

        let error = admit_authored_root(
            &policy,
            workspace.path().to_str().expect("utf-8 root"),
            false,
        )
        .expect_err("a grant authored by this write must be strictly admitted");
        assert!(
            error
                .to_string()
                .contains("is not admitted by published WorkspaceRoot policy"),
            "{error:#}"
        );
    }

    #[test]
    fn canonicalize_keeps_a_stored_grant_but_refuses_a_widening_set() {
        let workspace = tempfile::tempdir().expect("workspace");
        let project = workspace.path().join("project");
        std::fs::create_dir_all(&project).expect("narrowed operator root");
        let policy = project_workspace_root_policy(Vec::new(), Some(&project));
        let workspace_root = workspace.path().to_string_lossy().into_owned();

        let mut tools = gents_documents::Tools {
            host: Some(gents_documents::HostTools {
                root: Some(workspace_root.clone()),
                files: Some(gents_documents::FileTools {
                    mode: gents_documents::FileToolMode::ReadOnly,
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        canonicalize_tools_root(&mut tools, &policy, Some(&workspace_root))
            .expect("an edit that leaves the stored wide grant alone re-admits it");
        assert_eq!(
            tools.host.as_ref().and_then(|host| host.root.clone()),
            Some(
                std::fs::canonicalize(workspace.path())
                    .expect("canonical workspace")
                    .to_string_lossy()
                    .into_owned()
            )
        );

        let error = canonicalize_tools_root(&mut tools, &policy, Some("unrelated/old/root"))
            .expect_err("a set that changes host.root must be strictly admitted");
        assert!(
            error
                .to_string()
                .contains("is not admitted by published WorkspaceRoot policy"),
            "{error:#}"
        );
    }

    #[test]
    fn disjoint_authored_root_still_refuses_under_configured_policy() {
        let allowed = tempfile::tempdir().expect("allowed root");
        let elsewhere = tempfile::tempdir().expect("disjoint root");
        let policy = project_workspace_root_policy(
            vec![WorkspaceRootDocument {
                root_path: Some(allowed.path().to_string_lossy().into_owned()),
                enabled: Some(true),
            }],
            None,
        );

        let error = admit_authored_root(
            &policy,
            elsewhere.path().to_str().expect("utf-8 root"),
            true,
        )
        .expect_err("a grant disjoint from every published root must be refused");
        assert!(
            error
                .to_string()
                .contains("is not admitted by published WorkspaceRoot policy"),
            "{error:#}"
        );
    }

    #[test]
    fn no_documents_publish_the_resolved_ceiling_default() {
        let ceiling = tempfile::tempdir().expect("ceiling");
        let policy = project_workspace_root_policy(Vec::new(), Some(ceiling.path()));
        assert!(!policy.configured);
        assert_eq!(
            policy.published,
            vec![std::fs::canonicalize(ceiling.path()).expect("canonical")]
        );
    }

    #[cfg(unix)]
    #[test]
    fn invalid_configured_ceiling_fails_closed_without_legacy_self_admission() {
        let sandbox = tempfile::tempdir().expect("sandbox");
        let broken_ceiling = sandbox.path().join("broken-ceiling");
        std::os::unix::fs::symlink(sandbox.path().join("absent-target"), &broken_ceiling)
            .expect("broken ceiling symlink");
        let candidate = tempfile::tempdir().expect("candidate");

        let policy = project_workspace_root_policy(Vec::new(), Some(&broken_ceiling));
        assert!(policy.configured);
        assert!(policy.published.is_empty());
        assert_eq!(
            policy
                .admit(candidate.path())
                .expect("candidate resolution succeeds")
                .denial_reason(),
            Some(RootDenialReason::NoAllowedRoots)
        );
    }

    #[test]
    fn no_documents_and_no_ceiling_preserve_legacy_rootless_default() {
        let policy = project_workspace_root_policy(Vec::new(), None);
        assert!(!policy.configured);
        assert!(policy.published.is_empty());

        let mut tools = gents_documents::Tools {
            host: Some(gents_documents::HostTools {
                files: Some(gents_documents::FileTools {
                    mode: gents_documents::FileToolMode::ReadOnly,
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        canonicalize_tools_root(&mut tools, &policy, None)
            .expect("absence of operator root policy retains the legacy runtime default");

        let authored = tempfile::tempdir().expect("authored root");
        tools.host.as_mut().expect("host tools").root =
            Some(authored.path().to_string_lossy().into_owned());
        canonicalize_tools_root(&mut tools, &policy, None)
            .expect("an authored absolute root remains valid without policy or ceiling");
        assert_eq!(
            tools.host.and_then(|host| host.root),
            Some(
                std::fs::canonicalize(authored.path())
                    .expect("canonical")
                    .to_string_lossy()
                    .into_owned()
            )
        );
        assert_eq!(
            policy
                .admit(authored.path())
                .expect("request guard uses the same unconfigured policy")
                .admitted(),
            Some(std::fs::canonicalize(authored.path()).expect("canonical"))
        );
    }

    #[cfg(windows)]
    #[test]
    fn component_containment_distinguishes_windows_volume_prefixes() {
        let c_volume = PathBuf::from(r"C:\workspace\root");
        let d_volume = PathBuf::from(r"D:\workspace\root");
        assert!(!d_volume.starts_with(&c_volume));
        assert!(!c_volume.starts_with(&d_volume));
    }

    #[test]
    fn configured_tool_root_must_be_covered_by_a_published_root() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let published = directory.path().join("published");
        let other = directory.path().join("other");
        std::fs::create_dir_all(published.join("nested"))?;
        std::fs::create_dir_all(&other)?;
        let policy_for = |root: &Path, ceiling: &Path| {
            project_workspace_root_policy(
                vec![WorkspaceRootDocument {
                    root_path: Some(root.to_string_lossy().into_owned()),
                    enabled: Some(true),
                }],
                Some(ceiling),
            )
        };
        let nested = published.join("nested");

        configured_root_coverage(&policy_for(&published, &published), &published)?;
        configured_root_coverage(&policy_for(&nested, &nested), &nested)?;
        // A broader grant does not admit a narrower ceiling at execution, so
        // the startup check refuses the same configuration.
        assert!(configured_root_coverage(&policy_for(&published, &nested), &nested).is_err());

        let mismatch = configured_root_coverage(&policy_for(&published, &other), &other)
            .expect_err("a configured root outside every published root must be refused");
        let message = mismatch.to_string();
        assert!(message.contains(&other.display().to_string()), "{message}");
        assert!(message.contains("workspace-root set"), "{message}");
        assert!(message.contains("start the server again"), "{message}");

        let unpublished = WorkspaceRootPolicy {
            configured: true,
            published: Vec::new(),
        };
        assert!(configured_root_coverage(&unpublished, &published).is_err());
        Ok(())
    }
}

#[cfg(test)]
mod pushdown_tests;
