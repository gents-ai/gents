use caseless::Caseless;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use unicode_normalization::UnicodeNormalization;

#[derive(Debug, thiserror::Error)]
pub enum PathCapabilityError {
    #[error("duplicate workspace capability path: {path}")]
    DuplicatePath { path: String },
    #[error("workspace path component aliases conflict: {previous} and {prefix}")]
    AliasConflict { previous: String, prefix: String },
    #[error("workspace path is both file and parent directory: {prefix}")]
    FileAndParent { prefix: String },
    #[error("read workspace root {}", root.display())]
    ReadRoot {
        root: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("workspace root must be a real directory: {}", root.display())]
    RootNotDirectory { root: PathBuf },
    #[error("inspect workspace path parent {}", parent.display())]
    InspectParent {
        parent: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("workspace path spelling/alias mismatch: requested {requested}, found {found} in {}", parent.display())]
    SpellingMismatch {
        requested: String,
        found: String,
        parent: PathBuf,
    },
    #[error("ambiguous workspace path: {path}")]
    AmbiguousPath { path: String },
    #[error("workspace capability traverses or names a symlink: {}", found.display())]
    Symlink { found: PathBuf },
    #[error("workspace path parent is not a directory: {}", found.display())]
    ParentNotDirectory { found: PathBuf },
    #[error("workspace exact path must name a regular file: {}", found.display())]
    NotRegularFile { found: PathBuf },
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error("changed workspace path aliases conflict: {previous} and {prefix}")]
    ChangedAliasConflict { previous: String, prefix: String },
    #[error("changed workspace path component aliases conflict: {changed} and {prefix}")]
    ChangedComponentAlias { changed: String, prefix: String },
    #[error("workspace path must not be empty")]
    Empty,
    #[error("workspace path contains control or invisible formatting characters")]
    ControlOrInvisible,
    #[error(
        "workspace path contains unsupported separator, glob or platform alias syntax: {path}"
    )]
    UnsupportedSyntax { path: String },
    #[error("workspace path must be an exact relative path: {path}")]
    NotExactRelative { path: String },
    #[error("workspace path has a platform-ambiguous suffix: {path}")]
    PlatformAmbiguousSuffix { path: String },
    #[error("workspace path must not name Git metadata: {path}")]
    GitMetadata { path: String },
    #[error("workspace path has an ambiguous compatibility spelling: {path}")]
    AmbiguousCompatibility { path: String },
    #[error("workspace path names a reserved device: {path}")]
    ReservedDevice { path: String },
}

type Result<T, E = PathCapabilityError> = std::result::Result<T, E>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "mode", deny_unknown_fields)]
pub enum WorkspacePathCapability {
    #[serde(rename = "exactPaths")]
    ExactPaths { paths: Vec<String> },
    #[serde(rename = "unrestrictedCompatibility")]
    UnrestrictedCompatibility,
}

impl<'de> Deserialize<'de> for WorkspacePathCapability {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(tag = "mode", deny_unknown_fields)]
        enum Wire {
            #[serde(rename = "exactPaths")]
            ExactPaths { paths: Vec<String> },
            #[serde(rename = "unrestrictedCompatibility")]
            UnrestrictedCompatibility {},
        }
        match Wire::deserialize(deserializer)? {
            Wire::ExactPaths { paths } => {
                Self::exact_paths(paths).map_err(serde::de::Error::custom)
            }
            Wire::UnrestrictedCompatibility {} => Ok(Self::UnrestrictedCompatibility),
        }
    }
}

impl WorkspacePathCapability {
    pub fn exact_paths(mut paths: Vec<String>) -> Result<Self> {
        paths.sort();
        let capability = Self::ExactPaths { paths };
        capability.validate()?;
        Ok(capability)
    }

    pub fn validate(&self) -> Result<()> {
        let Self::ExactPaths { paths } = self else {
            return Ok(());
        };
        let mut literals = BTreeSet::new();
        let mut aliases = BTreeMap::<String, String>::new();
        for path in paths {
            validate_relative_path(path)?;
            if !literals.insert(path.as_str()) {
                return Err(PathCapabilityError::DuplicatePath { path: path.clone() });
            }
            let mut prefix = String::new();
            for component in path.split('/') {
                if !prefix.is_empty() {
                    prefix.push('/');
                }
                prefix.push_str(component);
                let key = alias_key(&prefix);
                if let Some(previous) = aliases.insert(key, prefix.clone()) {
                    if previous != prefix {
                        return Err(PathCapabilityError::AliasConflict { previous, prefix });
                    }
                }
            }
        }
        for path in paths {
            let mut prefix = String::new();
            let components: Vec<_> = path.split('/').collect();
            for component in &components[..components.len() - 1] {
                if !prefix.is_empty() {
                    prefix.push('/');
                }
                prefix.push_str(component);
                if literals.contains(prefix.as_str()) {
                    return Err(PathCapabilityError::FileAndParent { prefix });
                }
            }
        }
        Ok(())
    }

    pub fn is_exact(&self) -> bool {
        matches!(self, Self::ExactPaths { .. })
    }

    pub fn canonical_json(&self) -> String {
        let mut canonical = self.clone();
        if let Self::ExactPaths { paths } = &mut canonical {
            paths.sort();
        }
        serde_json::to_string(&canonical).expect("workspace capability contains only JSON strings")
    }

    pub fn digest(&self) -> String {
        format!("{:x}", Sha256::digest(self.canonical_json().as_bytes()))
    }

    pub fn authorizes(&self, path: &str) -> bool {
        match self {
            Self::UnrestrictedCompatibility => true,
            Self::ExactPaths { paths } => {
                validate_relative_path(path).is_ok() && paths.iter().any(|allowed| allowed == path)
            }
        }
    }

    /// No-follow check; missing leaves and parent suffixes are allowed for new
    /// files. A check, not a filesystem race lock.
    pub fn validate_paths_at(&self, root: &Path) -> Result<()> {
        self.validate()?;
        let Self::ExactPaths { paths } = self else {
            return Ok(());
        };
        let root_metadata =
            std::fs::symlink_metadata(root).map_err(|source| PathCapabilityError::ReadRoot {
                root: root.to_path_buf(),
                source,
            })?;
        if root_metadata.file_type().is_symlink() || !root_metadata.is_dir() {
            return Err(PathCapabilityError::RootNotDirectory {
                root: root.to_path_buf(),
            });
        }
        for path in paths {
            let components: Vec<_> = path.split('/').collect();
            let mut parent = root.to_path_buf();
            for (index, component) in components.iter().enumerate() {
                let wanted_alias = alias_key(component);
                let mut exact = None;
                let entries = std::fs::read_dir(&parent).map_err(|source| {
                    PathCapabilityError::InspectParent {
                        parent: parent.clone(),
                        source,
                    }
                })?;
                for entry in entries {
                    let entry = entry?;
                    let filename = entry.file_name();
                    let Some(spelling) = filename.to_str() else {
                        continue;
                    };
                    if alias_key(spelling) != wanted_alias {
                        continue;
                    }
                    if spelling != *component {
                        return Err(PathCapabilityError::SpellingMismatch {
                            requested: (*component).to_owned(),
                            found: spelling.to_owned(),
                            parent: parent.clone(),
                        });
                    }
                    if exact.is_some() {
                        return Err(PathCapabilityError::AmbiguousPath { path: path.clone() });
                    }
                    exact = Some(entry.path());
                }
                let Some(found) = exact else {
                    break;
                };
                let metadata = std::fs::symlink_metadata(&found)?;
                if metadata.file_type().is_symlink() {
                    return Err(PathCapabilityError::Symlink { found });
                }
                if index + 1 < components.len() {
                    if !metadata.is_dir() {
                        return Err(PathCapabilityError::ParentNotDirectory { found });
                    }
                } else if !metadata.is_file() {
                    return Err(PathCapabilityError::NotRegularFile { found });
                }
                parent = found;
            }
        }
        Ok(())
    }
}

pub(crate) fn validate_changed_path_aliases<'a>(
    changed: &[&str],
    tree_paths: impl IntoIterator<Item = &'a str>,
) -> Result<()> {
    let mut changed_prefixes = BTreeMap::new();
    for path in changed {
        validate_relative_path(path)?;
        let mut prefix = String::new();
        for component in path.split('/') {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(component);
            let key = alias_key(&prefix);
            if let Some(previous) = changed_prefixes.insert(key, prefix.clone()) {
                if previous != prefix {
                    return Err(PathCapabilityError::ChangedAliasConflict { previous, prefix });
                }
            }
        }
    }
    for path in tree_paths {
        let mut prefix = String::new();
        for component in path.split('/') {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(component);
            if let Some(changed) = changed_prefixes.get(&alias_key(&prefix)) {
                if changed != &prefix {
                    return Err(PathCapabilityError::ChangedComponentAlias {
                        changed: changed.clone(),
                        prefix,
                    });
                }
            }
        }
    }
    Ok(())
}

/// Compatibility caseless key (caseless::compatibility_caseless_match); never
/// an admitted path rewrite.
pub(crate) fn alias_key(value: &str) -> String {
    value
        .chars()
        .filter(|c| !ambiguous_format_character(*c))
        .nfd()
        .default_case_fold()
        .nfkd()
        .default_case_fold()
        .nfkd()
        .collect()
}

fn ambiguous_format_character(c: char) -> bool {
    matches!(c, '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{feff}')
}

pub(crate) fn validate_relative_path(path: &str) -> Result<()> {
    if path.is_empty() {
        return Err(PathCapabilityError::Empty);
    }
    if path
        .chars()
        .any(|c| c.is_control() || ambiguous_format_character(c))
    {
        return Err(PathCapabilityError::ControlOrInvisible);
    }
    if path.chars().any(|c| {
        matches!(
            c,
            '\\' | ':' | '*' | '?' | '[' | ']' | '{' | '}' | '~' | '<' | '>' | '"' | '|'
        )
    }) {
        return Err(PathCapabilityError::UnsupportedSyntax {
            path: path.to_owned(),
        });
    }
    for component in path.split('/') {
        if component.is_empty() || component == "." || component == ".." {
            return Err(PathCapabilityError::NotExactRelative {
                path: path.to_owned(),
            });
        }
        if component.ends_with(['.', ' ']) {
            return Err(PathCapabilityError::PlatformAmbiguousSuffix {
                path: path.to_owned(),
            });
        }
        let folded = alias_key(component);
        if folded == ".git" {
            return Err(PathCapabilityError::GitMetadata {
                path: path.to_owned(),
            });
        }
        // NFKD exposes fullwidth dot/separators and superscript device digits.
        if folded.contains(['/', '\\', ':'])
            || folded == "."
            || folded == ".."
            || folded.ends_with(['.', ' '])
        {
            return Err(PathCapabilityError::AmbiguousCompatibility {
                path: path.to_owned(),
            });
        }
        let stem = folded.split('.').next().unwrap_or_default();
        if matches!(stem, "con" | "prn" | "aux" | "nul" | "clock$")
            || ((stem.starts_with("com") || stem.starts_with("lpt"))
                && stem.len() == 4
                && matches!(stem.as_bytes()[3], b'1'..=b'9'))
        {
            return Err(PathCapabilityError::ReservedDevice {
                path: path.to_owned(),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn exact(paths: &[&str]) -> Result<WorkspacePathCapability> {
        WorkspacePathCapability::exact_paths(paths.iter().map(|p| (*p).into()).collect())
    }

    #[cfg(all(unix, not(target_os = "macos")))]
    #[test]
    fn unrelated_non_utf8_sibling_does_not_reject_owned_path() {
        use std::os::unix::ffi::OsStringExt;
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join(std::ffi::OsString::from_vec(vec![0xff])),
            b"unchanged",
        )
        .unwrap();
        std::fs::write(root.path().join("owned.rs"), b"owned").unwrap();
        exact(&["owned.rs"])
            .unwrap()
            .validate_paths_at(root.path())
            .unwrap();
    }

    #[test]
    fn rejects_unsafe_names_and_metadata_aliases() {
        for path in [
            "",
            "/tmp/x",
            "a//b",
            "a/",
            "./a",
            "a/../b",
            "C:/x",
            "a\\b",
            "a\0b",
            "a\nb",
            "*.rs",
            "a[0]",
            "a~1",
            "a.",
            "a ",
            ".git/config",
            ".GIT/config",
            "git~1/config",
            ".ＧＩＴ/config",
            ".gi\u{200c}t/config",
            "CON",
            "nul.txt",
            "COM¹.txt",
            "a/．．/x",
            "a／b",
            "a:b",
        ] {
            assert!(validate_relative_path(path).is_err(), "accepted {path:?}");
        }
    }

    #[test]
    fn rejects_file_and_directory_alias_collisions_without_rewriting() {
        for paths in [
            vec!["a", "a"],
            vec!["src/A.rs", "SRC/B.rs"],
            vec!["Straße/a", "STRASSE/b"],
            vec!["é/a", "e\u{301}/b"],
            vec!["ｆｏｏ/a", "foo/b"],
            vec!["src", "src/file"],
        ] {
            assert!(exact(&paths).is_err(), "accepted {paths:?}");
        }
        let cap = exact(&["src/é.rs", "src/z.rs"]).unwrap();
        assert!(cap.authorizes("src/é.rs"));
        assert!(!cap.authorizes("src/e\u{301}.rs"));
    }

    #[test]
    fn canonical_digest_is_order_independent_but_mode_and_path_sensitive() {
        let a = exact(&["z", "a"]).unwrap();
        let b = exact(&["a", "z"]).unwrap();
        assert_eq!(a, b);
        assert_eq!(a.digest(), b.digest());
        assert_ne!(a.digest(), exact(&["a"]).unwrap().digest());
        assert_ne!(
            exact(&[]).unwrap().digest(),
            WorkspacePathCapability::UnrestrictedCompatibility.digest()
        );
        for raw in [
            r#"{}"#,
            r#"{"mode":"exactPaths"}"#,
            r#"{"mode":"exactPaths","paths":[],"extra":true}"#,
            r#"{"mode":"unknown"}"#,
            r#"{"mode":"unrestrictedCompatibility","paths":[]}"#,
        ] {
            assert!(
                serde_json::from_str::<WorkspacePathCapability>(raw).is_err(),
                "accepted {raw}"
            );
        }
    }

    #[test]
    fn filesystem_spelling_is_exact_and_new_suffix_is_allowed() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("Source")).unwrap();
        std::fs::write(root.path().join("Source/file.rs"), "file").unwrap();
        assert!(exact(&["Source/file.rs", "Source/new/sub/file.rs"])
            .unwrap()
            .validate_paths_at(root.path())
            .is_ok());
        assert!(exact(&["source/file.rs"])
            .unwrap()
            .validate_paths_at(root.path())
            .is_err());
        assert!(exact(&["Source/file.rs/child"])
            .unwrap()
            .validate_paths_at(root.path())
            .is_err());
        assert!(exact(&["Source"])
            .unwrap()
            .validate_paths_at(root.path())
            .is_err());
    }

    #[cfg(unix)]
    #[test]
    fn symlink_parent_and_leaf_cannot_escape_workspace() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("sentinel"), "unchanged").unwrap();
        std::os::unix::fs::symlink(outside.path(), root.path().join("link")).unwrap();
        for path in ["link", "link/sentinel", "link/new/file"] {
            assert!(exact(&[path])
                .unwrap()
                .validate_paths_at(root.path())
                .is_err());
        }
        assert_eq!(
            std::fs::read_to_string(outside.path().join("sentinel")).unwrap(),
            "unchanged"
        );
    }
}
