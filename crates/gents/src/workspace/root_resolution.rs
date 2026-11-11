use std::io::ErrorKind;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

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
pub(crate) struct ResolvedToolRoot {
    pub(crate) path: PathBuf,
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
pub fn resolve_configured_tool_root(path: &Path) -> Result<PathBuf> {
    resolve_tool_root(path).map(|resolved| resolved.path)
}

pub(crate) fn resolve_tool_root(path: &Path) -> Result<ResolvedToolRoot> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .with_context(|| format!("resolving relative tool root {}", path.display()))?
            .join(path)
    };

    resolve_path_with_canonical_prefix(&absolute)
}

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
            // A malformed catalog row is not authoritative: publication drops
            // it so a bad sibling row cannot become a candidate error.
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
}
