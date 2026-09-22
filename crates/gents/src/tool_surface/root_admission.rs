//! Canonical filesystem-root resolution and containment.
//!
//! All configuration-time root gates use this owner. Paths are resolved again
//! by the execution/tool construction boundary; this module does not provide a
//! filesystem handle and therefore does not claim TOCTOU safety.

use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::Deserialize;

/// One operator-local `WorkspaceRoot` row. The schema intentionally has no
/// principal field: these rows are host policy shared by every local
/// principal and never replicate.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct WorkspaceRootDocument {
    pub root_path: Option<String>,
    pub enabled: Option<bool>,
}

/// Published filesystem policy after canonical resolution and the process
/// ceiling meet. `configured` records the presence of any document, including
/// disabled or invalid rows, and also records an explicitly supplied process
/// ceiling that failed resolution, so explicit policy cannot fall back to the
/// legacy self-admission path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkspaceRootPolicy {
    pub configured: bool,
    pub published: Vec<PathBuf>,
}

/// Execution-boundary observation retained beside the already-compiled host
/// tool set. Tool construction and every fresh owned request reload current
/// WorkspaceRoot rows and re-resolve this canonical persisted root. This
/// narrows stale-policy races but holds no filesystem handle and makes no
/// TOCTOU guarantee.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RootExecutionGuard {
    pub(crate) behavior_id: String,
    pub(crate) selected_root: Option<PathBuf>,
    pub(crate) ceiling_root: Option<PathBuf>,
}

impl RootExecutionGuard {
    pub(crate) async fn validate(&self, node: &defra_node::EmbeddedNode) -> Result<()> {
        let policy = load_workspace_root_policy(node, self.ceiling_root.as_deref()).await?;
        let Some(selected_root) = self.selected_root.as_deref() else {
            anyhow::ensure!(
                !policy.configured,
                "behavior {} has active host tools but no explicit root under current WorkspaceRoot policy",
                self.behavior_id
            );
            return Ok(());
        };
        match policy.admit(selected_root)? {
            RootAdmission::Admitted(root) if root == selected_root => Ok(()),
            RootAdmission::Admitted(root) => anyhow::bail!(
                "behavior {} persisted root {} re-resolved to {} at the execution boundary",
                self.behavior_id,
                selected_root.display(),
                root.display()
            ),
            denied @ RootAdmission::Denied { .. } => anyhow::bail!(
                "behavior {} persisted root {} is not admitted by current WorkspaceRoot policy at the execution boundary: {}",
                self.behavior_id,
                selected_root.display(),
                denied.denial_reason().expect("denied outcome has a reason")
            ),
        }
    }
}

impl WorkspaceRootPolicy {
    pub(crate) fn published_strings(&self) -> impl Iterator<Item = String> + '_ {
        self.published
            .iter()
            .map(|path| path.to_string_lossy().into_owned())
    }

    pub(crate) fn admit(&self, candidate: &Path) -> Result<RootAdmission> {
        anyhow::ensure!(
            candidate.is_absolute(),
            "configured tool root must be absolute, got {}",
            candidate.display()
        );
        // With no operator policy and no process ceiling, preserve the legacy
        // embedder contract: an authored absolute root narrows to its own
        // canonical resolution. Configured/revoked policy never takes this
        // branch and remains fail-closed.
        if !self.configured && self.published.is_empty() {
            return resolve_configured_tool_root(candidate).map(RootAdmission::Admitted);
        }
        resolve_admitted_tool_root(candidate, self.published.iter().map(PathBuf::as_path))
    }
}

/// Project the single canonical published-root policy. With no documents, the
/// process ceiling is the compatibility default. Once any document exists,
/// only enabled, safely resolved rows contained by the ceiling are published;
/// an all-disabled or all-invalid policy publishes nothing.
pub fn project_workspace_root_policy(
    documents: Vec<WorkspaceRootDocument>,
    ceiling_root: Option<&Path>,
) -> WorkspaceRootPolicy {
    let resolved_ceiling = ceiling_root.and_then(|ceiling| resolve_tool_root(ceiling).ok());
    let configured =
        !documents.is_empty() || (ceiling_root.is_some() && resolved_ceiling.is_none());

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

pub(crate) async fn load_workspace_root_policy_in_txn(
    txn: &crate::config_client::ConfigApplyTxn<'_>,
    ceiling_root: Option<&Path>,
) -> Result<WorkspaceRootPolicy> {
    let response = txn
        .execute("{ WorkspaceRoot { root_path enabled } }")
        .await
        .context("querying WorkspaceRoot policy in configuration transaction")?;
    Ok(project_workspace_root_policy(
        decode_workspace_root_rows(&response)?,
        ceiling_root,
    ))
}

pub(crate) async fn load_workspace_root_policy(
    node: &defra_node::EmbeddedNode,
    ceiling_root: Option<&Path>,
) -> Result<WorkspaceRootPolicy> {
    let response = node
        .execute("{ WorkspaceRoot { root_path enabled } }")
        .await;
    if response.has_errors() {
        anyhow::bail!("querying WorkspaceRoot policy: {:?}", response.errors);
    }
    let value = serde_json::json!({"data": response.data.unwrap_or(serde_json::Value::Null)});
    Ok(project_workspace_root_policy(
        decode_workspace_root_rows(&value)?,
        ceiling_root,
    ))
}

/// Validate and canonicalize the existing `Tools.host.root` field against a
/// published policy. This mutates only the canonical Tools document shape; it
/// does not introduce a parallel root field. Active host tools under explicit
/// policy must name a root instead of inheriting the broader process ceiling.
pub(crate) fn canonicalize_tools_root(
    tools: &mut crate::document_config::Tools,
    policy: &WorkspaceRootPolicy,
) -> Result<()> {
    let selection = crate::tool_surface::ResolvedToolSelection::from_document(tools)?;
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
            "active host tools require an explicit root while WorkspaceRoot policy is configured"
        );
        if let Some(host) = tools.host.as_mut() {
            host.root = None;
        }
        return Ok(());
    };

    let canonical = match policy.admit(Path::new(authored_root))? {
        RootAdmission::Admitted(root) => root,
        denied @ RootAdmission::Denied { .. } => anyhow::bail!(
            "Tools.host.root {:?} is not admitted by published WorkspaceRoot policy: {}",
            authored_root,
            denied.denial_reason().expect("denied outcome has a reason")
        ),
    };
    tools
        .host
        .as_mut()
        .expect("authored root came from host")
        .root = Some(canonical.to_string_lossy().into_owned());
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RootDenialReason {
    NoAllowedRoots,
    PrefixSibling,
    TraversalEscape,
    SymlinkEscape,
    NonexistentOutside,
    Outside,
}

impl std::fmt::Display for RootDenialReason {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::NoAllowedRoots => "no allowed roots were published",
            Self::PrefixSibling => "resolved path is a prefix-confusable sibling",
            Self::TraversalEscape => "resolved traversal escapes the allowed roots",
            Self::SymlinkEscape => "resolved symlink target escapes the allowed roots",
            Self::NonexistentOutside => "nonexistent path is outside the allowed roots",
            Self::Outside => "resolved path is outside the allowed roots",
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedToolRoot {
    path: PathBuf,
    observed_parent: bool,
    observed_symlinks: Vec<PathBuf>,
    nonexistent_suffix: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RootAdmission {
    Admitted(PathBuf),
    Denied {
        resolved_candidate: PathBuf,
        reason: RootDenialReason,
    },
}

impl RootAdmission {
    pub(crate) fn admitted(self) -> Option<PathBuf> {
        match self {
            Self::Admitted(path) => Some(path),
            Self::Denied { .. } => None,
        }
    }

    pub(crate) fn denial_reason(&self) -> Option<RootDenialReason> {
        match self {
            Self::Admitted(_) => None,
            Self::Denied { reason, .. } => Some(*reason),
        }
    }
}

/// Resolve existing prefixes (including symlinks) and retain a normalized,
/// nonexistent suffix. Broken or otherwise unresolvable symlinks fail closed.
pub(crate) fn resolve_configured_tool_root(path: &Path) -> Result<PathBuf> {
    resolve_tool_root(path).map(|resolved| resolved.path)
}

fn resolve_tool_root(path: &Path) -> Result<ResolvedToolRoot> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .with_context(|| format!("resolving relative tool root {}", path.display()))?
            .join(path)
    };

    resolve_path_with_canonical_prefix(&absolute)
}

/// Resolve `candidate` and admit it only when it is equal to or below one of
/// the supplied roots. Comparisons are component-wise over resolved paths.
/// An empty root iterator denies the candidate.
pub(crate) fn resolve_admitted_tool_root<'a>(
    candidate: &Path,
    allowed_roots: impl IntoIterator<Item = &'a Path>,
) -> Result<RootAdmission> {
    let candidate = resolve_tool_root(candidate)
        .with_context(|| format!("resolving candidate tool root {}", candidate.display()))?;

    let mut resolved_allowed = Vec::new();
    for allowed in allowed_roots {
        let allowed = match resolve_tool_root(allowed)
            .with_context(|| format!("resolving allowed tool root {}", allowed.display()))
        {
            Ok(allowed) => allowed,
            // A malformed catalog row is not authoritative. Publication drops
            // it; direct callers use the same behavior so BTree ordering cannot
            // turn a bad sibling row into a candidate-resolution error.
            Err(_) => continue,
        };
        if candidate.path.starts_with(&allowed.path) {
            return Ok(RootAdmission::Admitted(candidate.path));
        }
        resolved_allowed.push(allowed);
    }

    let reason = if resolved_allowed.is_empty() {
        RootDenialReason::NoAllowedRoots
    } else if candidate.observed_symlinks.iter().any(|symlink| {
        !resolved_allowed
            .iter()
            .any(|allowed| allowed.observed_symlinks.contains(symlink))
    }) {
        RootDenialReason::SymlinkEscape
    } else if candidate.observed_parent {
        RootDenialReason::TraversalEscape
    } else if candidate.nonexistent_suffix {
        RootDenialReason::NonexistentOutside
    } else if resolved_allowed.iter().any(|allowed| {
        candidate
            .path
            .to_string_lossy()
            .starts_with(allowed.path.to_string_lossy().as_ref())
    }) {
        RootDenialReason::PrefixSibling
    } else {
        RootDenialReason::Outside
    };

    Ok(RootAdmission::Denied {
        resolved_candidate: candidate.path,
        reason,
    })
}

fn resolve_path_with_canonical_prefix(path: &Path) -> Result<ResolvedToolRoot> {
    let mut resolved = PathBuf::new();
    let mut missing_tail = false;
    let mut observed_parent = false;
    let mut observed_symlinks = Vec::new();

    for component in path.components() {
        match component {
            std::path::Component::Prefix(_) | std::path::Component::RootDir => {
                resolved.push(component.as_os_str());
            }
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                observed_parent = true;
                resolved.pop();
                // `..` may leave a missing suffix and return to an existing
                // prefix. Re-check later components so a following symlink is
                // canonicalized rather than treated as lexical text.
                missing_tail = false;
            }
            std::path::Component::Normal(name) => {
                let candidate = resolved.join(name);
                if missing_tail {
                    resolved.push(name);
                    continue;
                }

                match std::fs::symlink_metadata(&candidate) {
                    Ok(metadata) => {
                        if metadata.file_type().is_symlink() {
                            observed_symlinks.push(candidate.clone());
                        }
                        resolved = std::fs::canonicalize(&candidate).with_context(|| {
                            format!("canonicalizing tool root {}", candidate.display())
                        })?;
                    }
                    Err(error) if error.kind() == ErrorKind::NotFound => {
                        missing_tail = true;
                        resolved.push(name);
                    }
                    Err(error) => {
                        return Err(error).with_context(|| {
                            format!("inspecting tool root {}", candidate.display())
                        });
                    }
                }
            }
        }
    }

    Ok(ResolvedToolRoot {
        path: resolved,
        observed_parent,
        observed_symlinks,
        nonexistent_suffix: missing_tail,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nonexistent_descendant_is_retained_and_admitted() {
        let root = tempfile::tempdir().expect("tempdir");
        let candidate = root.path().join("future").join("project");
        let admitted = resolve_admitted_tool_root(&candidate, [root.path()])
            .expect("nonexistent suffix resolves")
            .admitted()
            .expect("descendant admitted");
        let expected = std::fs::canonicalize(root.path())
            .expect("canonical root")
            .join("future")
            .join("project");
        assert_eq!(admitted, expected);
    }

    #[test]
    fn component_prefix_rejects_string_prefix_sibling() {
        let parent = tempfile::tempdir().expect("tempdir");
        let root = parent.path().join("root");
        let sibling = parent.path().join("root-other");
        std::fs::create_dir_all(&root).expect("root");
        std::fs::create_dir_all(&sibling).expect("sibling");

        let decision =
            resolve_admitted_tool_root(&sibling, [root.as_path()]).expect("paths resolve");
        assert_eq!(
            decision.denial_reason(),
            Some(RootDenialReason::PrefixSibling)
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlink_to_inside_target_is_admitted_on_resolved_components() {
        let root = tempfile::tempdir().expect("tempdir");
        let nested = root.path().join("nested");
        std::fs::create_dir_all(&nested).expect("nested");
        let links = tempfile::tempdir().expect("tempdir");
        let link = links.path().join("inside");
        std::os::unix::fs::symlink(&nested, &link).expect("symlink");

        let admitted = resolve_admitted_tool_root(&link, [root.path()])
            .expect("paths resolve")
            .admitted()
            .expect("inside target admitted");
        assert_eq!(admitted, std::fs::canonicalize(nested).expect("canonical"));
    }

    #[cfg(unix)]
    #[test]
    fn parent_after_missing_suffix_rechecks_following_symlink() {
        let root = tempfile::tempdir().expect("tempdir");
        let outside = tempfile::tempdir().expect("tempdir");
        let link = root.path().join("escape");
        std::os::unix::fs::symlink(outside.path(), &link).expect("symlink");
        let candidate = root.path().join("missing").join("..").join("escape");

        let decision =
            resolve_admitted_tool_root(&candidate, [root.path()]).expect("path resolves");
        assert_eq!(
            decision.denial_reason(),
            Some(RootDenialReason::SymlinkEscape)
        );
    }

    #[cfg(unix)]
    #[test]
    fn broken_symlink_fails_closed_instead_of_becoming_a_missing_suffix() {
        let root = tempfile::tempdir().expect("tempdir");
        let link = root.path().join("broken");
        std::os::unix::fs::symlink(root.path().join("absent-target"), &link).expect("symlink");

        let error = resolve_admitted_tool_root(&link, [root.path()])
            .expect_err("broken symlink must fail resolution");
        assert!(error.to_string().contains("resolving candidate tool root"));
    }

    #[cfg(unix)]
    #[test]
    fn broken_allowed_root_does_not_hide_a_later_valid_root() {
        let parent = tempfile::tempdir().expect("tempdir");
        let broken = parent.path().join("broken");
        std::os::unix::fs::symlink(parent.path().join("missing"), &broken).expect("symlink");
        let valid = tempfile::tempdir().expect("valid root");
        let candidate = valid.path().join("nested");
        std::fs::create_dir_all(&candidate).expect("candidate");

        let admitted = resolve_admitted_tool_root(&candidate, [broken.as_path(), valid.path()])
            .expect("a broken sibling policy row must not make ordering authoritative")
            .admitted();
        assert_eq!(
            admitted,
            Some(std::fs::canonicalize(candidate).expect("canonical"))
        );
    }

    #[cfg(unix)]
    #[test]
    fn all_broken_allowed_roots_are_an_empty_publication_not_an_error() {
        let parent = tempfile::tempdir().expect("tempdir");
        let broken = parent.path().join("broken");
        std::os::unix::fs::symlink(parent.path().join("missing"), &broken).expect("symlink");
        let candidate = tempfile::tempdir().expect("candidate");

        let denied = resolve_admitted_tool_root(candidate.path(), [broken.as_path()])
            .expect("invalid policy rows are skipped")
            .denial_reason();
        assert_eq!(denied, Some(RootDenialReason::NoAllowedRoots));
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
        let mut tools = crate::document_config::Tools {
            host: Some(crate::document_config::HostTools {
                cli: vec![crate::document_config::CliTool {
                    name: "git".to_string(),
                    ..Default::default()
                }],
                ..Default::default()
            }),
            ..Default::default()
        };
        let error = canonicalize_tools_root(&mut tools, &policy)
            .expect_err("CLI uses the shared root and cannot inherit cwd under explicit policy");
        assert!(error.to_string().contains("require an explicit root"));
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

        let mut tools = crate::document_config::Tools {
            host: Some(crate::document_config::HostTools {
                files: Some(crate::document_config::FileTools {
                    mode: crate::tool_surface::FileToolMode::ReadOnly,
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };
        canonicalize_tools_root(&mut tools, &policy)
            .expect("absence of operator root policy retains the legacy runtime default");

        let authored = tempfile::tempdir().expect("authored root");
        tools.host.as_mut().expect("host tools").root =
            Some(authored.path().to_string_lossy().into_owned());
        canonicalize_tools_root(&mut tools, &policy)
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
}
