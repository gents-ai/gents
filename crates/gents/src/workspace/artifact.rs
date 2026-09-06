//! Request-owned compiler output, separate from the sealed source capability.
//! The workspace executor owns the containing Git worktree directory and cleanup.
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use chrono::{DateTime, Utc};
use defra_node::EmbeddedNode;
use serde_json::Value;

use crate::graphql::{escape_graphql_string, graphql_with_transaction_retry};
use crate::watcher::AgentRequest;

#[derive(Clone)]
pub struct ArtifactGrant(Arc<ArtifactOwner>);

struct ArtifactOwner {
    node: Arc<EmbeddedNode>,
    source_root: PathBuf,
    git_dir: PathBuf,
    root: PathBuf,
    request_doc_id: String,
    request_id: String,
    execution_generation: String,
    workspace_id: String,
    deployment_id: String,
    seal_hash: String,
    identities: Vec<(PathBuf, DirectoryIdentity)>,
}

impl std::fmt::Debug for ArtifactGrant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArtifactGrant")
            .field("source_root", &self.0.source_root)
            .field("root", &self.0.root)
            .field("request_doc_id", &self.0.request_doc_id)
            .field("execution_generation", &self.0.execution_generation)
            .finish()
    }
}

#[derive(Debug, PartialEq, Eq)]
struct DirectoryIdentity {
    #[cfg(unix)]
    device: u64,
    #[cfg(unix)]
    inode: u64,
}

fn checked_directory(path: &Path) -> Result<DirectoryIdentity> {
    if !path.is_absolute() {
        bail!("artifact directory must be absolute");
    }
    let mut prefix = PathBuf::new();
    for component in path.components() {
        if matches!(
            component,
            std::path::Component::ParentDir | std::path::Component::CurDir
        ) {
            bail!("artifact directory has noncanonical components");
        }
        prefix.push(component.as_os_str());
        let metadata = std::fs::symlink_metadata(&prefix)
            .with_context(|| format!("inspecting artifact directory {}", prefix.display()))?;
        if metadata.file_type().is_symlink() || !metadata.is_dir() {
            bail!(
                "artifact directory component is not a real directory: {}",
                prefix.display()
            );
        }
    }
    let metadata = std::fs::metadata(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Ok(DirectoryIdentity {
            device: metadata.dev(),
            inode: metadata.ino(),
        })
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        bail!("artifact directory identity requires a supported host");
    }
}

fn create_private_directory(path: &Path) -> Result<()> {
    let mut builder = std::fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
        .create(path)
        .with_context(|| format!("creating private artifacts {}", path.display()))
}

fn checked_worktree_git_dir(source: &Path, workspace_id: &str) -> Result<PathBuf> {
    checked_directory(source)?;
    let pointer = source.join(".git");
    let metadata = std::fs::symlink_metadata(&pointer)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() {
        bail!("artifact grant requires an actual linked Git worktree");
    }
    let contents = std::fs::read_to_string(&pointer)?;
    let raw = contents
        .trim()
        .strip_prefix("gitdir: ")
        .ok_or_else(|| anyhow!("invalid worktree Git directory pointer"))?;
    let path = Path::new(raw);
    let git_dir = if path.is_absolute() {
        path.to_owned()
    } else {
        source.join(path)
    };
    // Git-created relative pointers may contain '..'; resolve them, then inspect
    // every original path component for symlinks before trusting the resolved root.
    let mut prefix = PathBuf::new();
    for component in git_dir.components() {
        prefix.push(component.as_os_str());
        if std::fs::symlink_metadata(&prefix)?.file_type().is_symlink() {
            bail!("worktree Git directory traverses a symlink");
        }
    }
    let git_dir = std::fs::canonicalize(git_dir)?;
    checked_directory(&git_dir)?;
    if git_dir.starts_with(source) {
        bail!("artifact root cannot be inside source");
    }
    let backlink = git_dir.join("gitdir");
    if std::fs::symlink_metadata(&backlink)?
        .file_type()
        .is_symlink()
    {
        bail!("worktree Git backlink is a symlink");
    }
    let backlink = std::fs::read_to_string(backlink)?;
    if std::fs::canonicalize(backlink.trim())? != pointer {
        bail!("worktree Git directory does not point back to the bound source");
    }
    let marker = git_dir.join("gents-workspace-identity.json");
    if std::fs::symlink_metadata(&marker)?.file_type().is_symlink() {
        bail!("workspace identity marker is a symlink");
    }
    let identity: Value = serde_json::from_slice(&std::fs::read(marker)?)?;
    if identity["workspace_id"].as_str() != Some(workspace_id) {
        bail!("artifact placement workspace identity mismatch");
    }
    Ok(git_dir)
}

impl ArtifactGrant {
    pub fn source_root(&self) -> &Path {
        &self.0.source_root
    }
    pub fn root(&self) -> &Path {
        &self.0.root
    }
    pub fn request_doc_id(&self) -> &str {
        &self.0.request_doc_id
    }
    pub fn execution_generation(&self) -> &str {
        &self.0.execution_generation
    }

    pub(super) async fn create(
        node: Arc<EmbeddedNode>,
        request: &AgentRequest,
        execution_generation: &str,
        source_root: &Path,
        deployment_id: &str,
        seal_hash: &str,
    ) -> Result<Self> {
        let workspace_id = request
            .workspace_id
            .as_deref()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| anyhow!("artifact grant requires workspace identity"))?;
        if execution_generation.is_empty() {
            bail!("artifact grant requires execution generation");
        }
        let git_dir = checked_worktree_git_dir(source_root, workspace_id)?;
        let parent = git_dir.join("gents-artifacts");
        match create_private_directory(&parent) {
            Ok(()) => (),
            Err(error) if parent.exists() => {
                checked_directory(&parent).context(error)?;
            }
            Err(error) => return Err(error),
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            let metadata = std::fs::metadata(&parent)?;
            if metadata.uid() != std::fs::metadata(&git_dir)?.uid() || metadata.mode() & 0o077 != 0
            {
                bail!("artifact parent is not private to the Git worktree owner");
            }
        }
        // Never reopen a prior request's artifact directory or derive authority
        // from a caller-provided directory name.
        let root = parent.join(uuid::Uuid::new_v4().to_string());
        create_private_directory(&root)?;
        for child in ["target", "tmp"] {
            create_private_directory(&root.join(child))?;
        }
        let identities = [
            source_root.to_path_buf(),
            git_dir.clone(),
            parent,
            root.clone(),
            root.join("target"),
            root.join("tmp"),
        ]
        .into_iter()
        .map(|p| checked_directory(&p).map(|identity| (p, identity)))
        .collect::<Result<Vec<_>>>()?;
        let grant = Self(Arc::new(ArtifactOwner {
            node,
            source_root: source_root.to_path_buf(),
            git_dir,
            root,
            request_doc_id: request.doc_id.clone(),
            request_id: request.request_id.clone(),
            execution_generation: execution_generation.to_owned(),
            workspace_id: workspace_id.to_owned(),
            deployment_id: deployment_id.to_owned(),
            seal_hash: seal_hash.to_owned(),
            identities,
        }));
        grant.validate_for_launch().await?;
        Ok(grant)
    }

    /// Revalidates this incarnation immediately before shared command preparation.
    /// Existing request cancellation remains responsible for process termination
    /// if cancellation races the subsequent spawn; this is not a second owner.
    pub async fn validate_for_launch(&self) -> Result<()> {
        let owner = &self.0;
        for (path, expected) in &owner.identities {
            if &checked_directory(path)? != expected {
                bail!("artifact directory identity changed");
            }
        }
        if checked_worktree_git_dir(&owner.source_root, &owner.workspace_id)? != owner.git_dir {
            bail!("artifact Git placement changed");
        }
        // Hash before observing the execution lease: on a large tree this may
        // take hundreds of milliseconds. Keep the generation/expiry observation
        // as close to launch as possible. Fresh hashing deliberately avoids a
        // cache whose invalidation would become another source-integrity premise.
        if super::adapter::working_tree_hash(&owner.source_root)? != owner.seal_hash {
            bail!("artifact source no longer matches its seal");
        }
        let doc = escape_graphql_string(&owner.request_doc_id);
        let workspace = escape_graphql_string(&owner.workspace_id);
        let deployment = escape_graphql_string(&owner.deployment_id);
        let query = format!(
            r#"{{
          AgentRequest(filter: {{ _docID: {{ _eq: "{doc}" }} }}) {{
            _docID request_id lifecycle_state execution_generation execution_lease_expires_at
            workspace_id workspace_authority workspace_owner_deployment_id workspace_seal_hash
          }}
          WorkspaceBinding(filter: {{ request_doc_id: {{ _eq: "{doc}" }} }}) {{
            workspace_id request_id request_doc_id authority deployment_id seal_hash lifecycle_state
          }}
          IsolatedWorkspace(filter: {{ workspace_id: {{ _eq: "{workspace}" }} }}) {{
            workspace_id owner_deployment_id lifecycle_state seal_hash
          }}
          WorkspacePlacement(filter: {{ workspace_id: {{ _eq: "{workspace}" }}, deployment_id: {{ _eq: "{deployment}" }} }}) {{
            host_path observed_tree_hash
          }}
        }}"#
        );
        let value = graphql_with_transaction_retry(
            &owner.node,
            &query,
            "validate artifact execution grant",
        )
        .await?;
        validate_launch_rows(
            owner,
            value
                .data
                .as_ref()
                .ok_or_else(|| anyhow!("missing artifact query data"))?,
            Utc::now(),
        )?;
        Ok(())
    }
}

fn validate_launch_rows(owner: &ArtifactOwner, response: &Value, now: DateTime<Utc>) -> Result<()> {
    let data = response.get("data").unwrap_or(response);
    let one = |name: &str| -> Result<&Value> {
        let rows = data[name]
            .as_array()
            .ok_or_else(|| anyhow!("missing artifact {name} rows"))?;
        if rows.len() != 1 {
            bail!("artifact {name} identity missing or ambiguous");
        }
        Ok(&rows[0])
    };
    let request = one("AgentRequest")?;
    let state = request["lifecycle_state"]
        .as_str()
        .and_then(|s| gents_protocol::request_lifecycle::RequestLifecycleState::parse(s).ok());
    if !matches!(
        state,
        Some(
            gents_protocol::request_lifecycle::RequestLifecycleState::Claimed
                | gents_protocol::request_lifecycle::RequestLifecycleState::Processing
        )
    ) || request["execution_generation"].as_str() != Some(&owner.execution_generation)
        || request["request_id"].as_str() != Some(&owner.request_id)
        || request["workspace_id"].as_str() != Some(&owner.workspace_id)
        || request["workspace_owner_deployment_id"].as_str() != Some(&owner.deployment_id)
        || request["workspace_seal_hash"].as_str() != Some(&owner.seal_hash)
        || request["workspace_authority"]
            .as_str()
            .and_then(|s| crate::toolset::WorkspaceAuthority::parse(s).ok())
            != Some(crate::toolset::WorkspaceAuthority::ReadOnly)
    {
        bail!("artifact execution owner is no longer current and live");
    }
    let expiry = request["execution_lease_expires_at"]
        .as_str()
        .ok_or_else(|| anyhow!("artifact owner has no lease deadline"))?;
    // Match the existing execution owner: the deadline millisecond is live;
    // expiry occurs strictly after it, with the same millisecond precision.
    if DateTime::parse_from_rfc3339(expiry)?.timestamp_millis() < now.timestamp_millis() {
        bail!("artifact execution lease has expired");
    }
    let bindings = data["WorkspaceBinding"]
        .as_array()
        .ok_or_else(|| anyhow!("missing artifact bindings"))?;
    let valid = bindings
        .iter()
        .filter(|row| {
            row["workspace_id"].as_str() == Some(&owner.workspace_id)
                && row["request_id"].as_str() == Some(&owner.request_id)
                && row["request_doc_id"].as_str() == Some(&owner.request_doc_id)
                && row["deployment_id"].as_str() == Some(&owner.deployment_id)
                && row["seal_hash"].as_str() == Some(&owner.seal_hash)
                && row["lifecycle_state"].as_str() == Some(super::documents::BINDING_ACTIVE)
                && row["authority"]
                    .as_str()
                    .and_then(|s| crate::toolset::WorkspaceAuthority::parse(s).ok())
                    == Some(crate::toolset::WorkspaceAuthority::ReadOnly)
        })
        .count();
    if valid != 1 {
        bail!("artifact requires one current active ReadOnly binding");
    }
    let workspace = one("IsolatedWorkspace")?;
    if workspace["owner_deployment_id"].as_str() != Some(&owner.deployment_id)
        || workspace["seal_hash"].as_str() != Some(&owner.seal_hash)
        || crate::toolset::normalize_workspace_lifecycle_state(
            workspace["lifecycle_state"].as_str().unwrap_or(""),
        ) != Some("sealed")
    {
        bail!("artifact workspace is no longer sealed under its admitted owner");
    }
    let placement = one("WorkspacePlacement")?;
    if placement["host_path"].as_str().map(Path::new) != Some(owner.source_root.as_path())
        || placement["observed_tree_hash"].as_str() != Some(&owner.seal_hash)
    {
        bail!("artifact workspace placement or seal changed");
    }
    Ok(())
}
