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
}

impl RootExecutionGuard {
    pub async fn validate(
        &self,
        node: &std::sync::Arc<defra_node::Node>,
        agent: &dyn gents_store::identity::NodeIdentity,
    ) -> Result<()> {
        anyhow::ensure!(
            agent.did() == self.agent_did,
            "workspace root requires the selected Agent identity"
        );
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
            anyhow::ensure!(
                !policy.configured,
                "agent {} has active host tools but no explicit root under current WorkspaceRoot policy",
                self.agent_did
            );
            return Ok(());
        };
        match policy.admit(selected_root)? {
            RootAdmission::Admitted(root) if root == selected_root => Ok(()),
            RootAdmission::Admitted(root) => anyhow::bail!(
                "agent {} persisted root {} re-resolved to {} at the execution boundary",
                self.agent_did,
                selected_root.display(),
                root.display()
            ),
            denied @ RootAdmission::Denied { .. } => anyhow::bail!(
                "agent {} persisted root {} is not admitted by current WorkspaceRoot policy at the execution boundary: {}",
                self.agent_did,
                selected_root.display(),
                denied.denial_reason().expect("denied outcome has a reason")
            ),
        }
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

/// Those of `agents` whose Tools select host tools under a published root,
/// each with the root it needs reader on, as `access`'s actor sees them: what
/// the agents a pack ships need before they run. A User grants each with
/// [`grant_workspace_root_reader`](super::grant_workspace_root_reader); an
/// agent that installed the pack reports [`reader_grant_step`] instead.
pub async fn agents_tool_root_readers(
    access: &gents_store::config_client::ConfigAccess,
    node_did: &str,
    agents: &[String],
) -> Result<Vec<(String, ToolRootReader)>> {
    let mut readers = Vec::new();
    for agent_did in agents {
        match agent_tool_root_reader(access, node_did, agent_did).await? {
            ToolRootReader::NotNeeded => {}
            reader => readers.push((agent_did.clone(), reader)),
        }
    }
    Ok(readers)
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
    known_missing: bool,
) -> serde_json::Value {
    let path = published_root.unwrap_or("PUBLISHED_ROOT");
    let answer = if known_missing {
        format!("Saved, but {agent_did} cannot use its host tools yet: a User must grant it reader on the published WorkspaceRoot {path}. Until then the runtime cannot load its tools and reports the agent unavailable.")
    } else {
        format!("{agent_did} uses host tools only if a User granted it reader on the published WorkspaceRoot {path}; without that grant the runtime cannot load its tools and the agent is unavailable (agent_new to it is not offered).")
    };
    serde_json::json!({
        "state": if known_missing { "waiting_for_user" } else { "requires_user_grant" },
        "published_root": published_root,
        "answer": answer,
        "user_command": format!(
            "gents config workspace-root grant-reader --graphql GRAPHQL_URL --path {path} --agent-did {agent_did}"
        ),
        "how_to_proceed": "Keep these Tools and give the User user_command to run from their own client; no config or policy call, and no change to your own Tools, can grant it.",
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
        anyhow::ensure!(
            !policy.configured || !requires_root,
            "active host tools require an explicit root in Tools.host.root while WorkspaceRoot policy is configured. Set host.root to an admitted absolute workspace path (a sibling of host.files and host.bash). In config, read tools get first, then tools update with set.host preserving its other fields; do not put root inside host.files"
        );
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
        node.add_collection_as(&context, &format!(
            "type WorkspaceRoot @policy(id: \"{policy_id}\", resource: \"roots\") {{ node_did: String root_path: String enabled: Boolean }}"
        )).await?;
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
        assert!(error.to_string().contains("require an explicit root"));
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
