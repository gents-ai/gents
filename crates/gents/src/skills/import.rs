//! Importing skill instructions never executes supporting files.

use std::io::Read;
use std::path::{Path, PathBuf};

type Result<T, E = SkillsError> = std::result::Result<T, E>;

#[derive(Debug, thiserror::Error)]
pub enum SkillsError {
    #[error("invalid SKILL.md frontmatter: {error}")]
    Frontmatter { error: serde_yaml::Error },
    #[error("SKILL.md frontmatter is missing its closing --- delimiter")]
    UnclosedFrontmatter,
    #[error("{} is not a regular file", path.display())]
    NotRegularFile { path: PathBuf },
    #[error("opening {}", path.display())]
    Open {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("{} exceeds the 1 MiB skill source limit", path.display())]
    TooLarge { path: PathBuf },
    #[error("{} must be UTF-8", path.display())]
    NotUtf8 {
        path: PathBuf,
        #[source]
        source: std::string::FromUtf8Error,
    },
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error("skill_id must not be blank")]
    SkillIdBlank,
    #[error("expected a skill directory or a SKILL.md file")]
    NotSkillSource,
    #[error("SKILL.md must contain instructions after its frontmatter")]
    MissingInstructions,
    #[error("SKILL.md has no parent directory")]
    NoParentDirectory,
    #[error("invalid metadata in {}", path.display())]
    InvalidMetadata {
        path: PathBuf,
        #[source]
        source: serde_yaml::Error,
    },
    #[error("checking agents/openai.yaml")]
    CheckingMetadata(#[source] std::io::Error),
    #[error("skill source directory must be UTF-8")]
    SourceDirectoryNotUtf8,
}

use crate::document_config::SkillDocument;

const MAX_SOURCE_BYTES: u64 = 1024 * 1024;

#[derive(Default, serde::Deserialize)]
pub struct SkillFrontmatter {
    pub name: Option<String>,
    pub description: Option<String>,
    #[serde(default)]
    pub license: Option<String>,
    #[serde(default, rename = "allowed-tools")]
    pub allowed_tools: Option<String>,
}

pub fn parse_skill_md(contents: &str) -> Result<(SkillFrontmatter, String)> {
    let mut lines = contents.lines();
    if lines.next().map(str::trim) == Some("---") {
        let mut yaml = String::new();
        let mut closed = false;
        let mut body = Vec::new();
        for line in lines {
            if !closed {
                if line.trim() == "---" {
                    closed = true;
                    continue;
                }
                yaml.push_str(line);
                yaml.push('\n');
            } else {
                body.push(line);
            }
        }
        if closed {
            let frontmatter =
                serde_yaml::from_str(&yaml).map_err(|error| SkillsError::Frontmatter { error })?;
            return Ok((frontmatter, body.join("\n").trim().to_string()));
        }
        return Err(SkillsError::UnclosedFrontmatter);
    }
    Ok((SkillFrontmatter::default(), contents.trim().to_string()))
}

#[derive(Default, serde::Deserialize)]
pub struct OpenAiYaml {
    pub interface: Option<serde_yaml::Value>,
    pub dependencies: Option<OpenAiDependencies>,
}

#[derive(Default, serde::Deserialize)]
pub struct OpenAiDependencies {
    #[serde(default)]
    pub tools: Vec<OpenAiTool>,
}

#[derive(Default, serde::Deserialize)]
pub struct OpenAiTool {
    pub value: Option<String>,
}

/// Load a single directory containing SKILL.md, or that file directly. The
/// caller resolves/authorizes each path before reading.
pub fn load_skill_source<E>(
    source: &Path,
    skill_id: &str,
    node_did: &str,
    resolve: impl Fn(&Path) -> Result<PathBuf, E>,
) -> Result<SkillDocument, E>
where
    E: From<SkillsError>,
{
    if skill_id.trim().is_empty() {
        return Err(SkillsError::SkillIdBlank.into());
    }
    let source = resolve(source)?;
    let file = if source.is_dir() {
        resolve(&source.join("SKILL.md"))?
    } else {
        if source.file_name().is_none_or(|n| n != "SKILL.md") {
            return Err(SkillsError::NotSkillSource.into());
        }
        source
    };
    let contents = read_source(&file)?;
    let (frontmatter, body) = parse_skill_md(&contents)?;
    if body.is_empty() {
        return Err(SkillsError::MissingInstructions.into());
    }
    let metadata_path = file
        .parent()
        .ok_or(SkillsError::NoParentDirectory)?
        .join("agents/openai.yaml");
    let metadata = match std::fs::symlink_metadata(&metadata_path) {
        Ok(_) => {
            let path = resolve(&metadata_path)?;
            serde_yaml::from_str::<OpenAiYaml>(&read_source(&path)?).map_err(|source| {
                SkillsError::InvalidMetadata {
                    path: path.clone(),
                    source,
                }
            })?
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => OpenAiYaml::default(),
        Err(error) => return Err(SkillsError::CheckingMetadata(error).into()),
    };
    let tool_refs = metadata
        .dependencies
        .map(|deps| {
            deps.tools
                .into_iter()
                .filter_map(|tool| tool.value)
                .filter(|v| !v.trim().is_empty())
                .collect()
        })
        .unwrap_or_default();
    let display_name = metadata
        .interface
        .as_ref()
        .and_then(|v| v.get("display_name"))
        .and_then(serde_yaml::Value::as_str)
        .filter(|v| !v.trim().is_empty())
        .map(str::to_owned);
    let interface_json = metadata
        .interface
        .as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(SkillsError::from)?;
    Ok(SkillDocument {
        skill_id: skill_id.to_owned(),
        node_did: node_did.to_owned(),
        name: Some(frontmatter.name.unwrap_or_else(|| skill_id.to_owned())),
        description: frontmatter.description,
        instructions: Some(body),
        source_directory: Some(
            file.parent()
                .ok_or(SkillsError::NoParentDirectory)?
                .to_str()
                .ok_or(SkillsError::SourceDirectoryNotUtf8)?
                .to_owned(),
        ),
        tool_refs,
        display_name,
        interface_json,
        enabled: true,
        created_at: None,
        tags: Vec::new(),
    })
}

fn read_source(path: &Path) -> Result<String> {
    if !std::fs::metadata(path)?.is_file() {
        return Err(SkillsError::NotRegularFile {
            path: path.to_owned(),
        });
    }
    let file = std::fs::File::open(path).map_err(|source| SkillsError::Open {
        path: path.to_owned(),
        source,
    })?;
    if !file.metadata()?.is_file() {
        return Err(SkillsError::NotRegularFile {
            path: path.to_owned(),
        });
    }
    let mut bytes = Vec::new();
    file.take(MAX_SOURCE_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_SOURCE_BYTES {
        return Err(SkillsError::TooLarge {
            path: path.to_owned(),
        });
    }
    String::from_utf8(bytes).map_err(|source| SkillsError::NotUtf8 {
        path: path.to_owned(),
        source,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    type TestResult<T = ()> = std::result::Result<T, Box<dyn std::error::Error + Send + Sync>>;

    fn resolve(path: &Path) -> TestResult<PathBuf> {
        Ok(std::fs::canonicalize(path)?)
    }

    #[test]
    fn directory_and_file_load_the_same_canonical_document() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("SKILL.md"),
            "---\nname: Review\ndescription: Review changes\n---\nRead references/checklist.md.",
        )
        .unwrap();
        std::fs::create_dir(root.path().join("agents")).unwrap();
        std::fs::write(root.path().join("agents/openai.yaml"),
            "interface:\n  display_name: Code Review\ndependencies:\n  tools:\n    - value: read_file\n").unwrap();
        let directory = load_skill_source(root.path(), "review", "did:test", resolve).unwrap();
        let file = load_skill_source(&root.path().join("SKILL.md"), "review", "did:test", resolve)
            .unwrap();
        assert_eq!(directory, file);
        assert_eq!(
            file.source_directory.as_deref(),
            root.path().canonicalize().unwrap().to_str()
        );
        assert_eq!(file.name.as_deref(), Some("Review"));
        assert_eq!(file.display_name.as_deref(), Some("Code Review"));
        assert_eq!(file.tool_refs, vec!["read_file"]);
        assert_eq!(
            file.instructions.as_deref(),
            Some("Read references/checklist.md.")
        );
    }

    #[test]
    fn optional_frontmatter_does_not_become_tool_authority() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("SKILL.md"),
            "---\nname: code-review\ndescription: >-\n  Review changes\n  with evidence.\nlicense: MIT\ncompatibility: Requires local source files\nmetadata:\n  author: example\n  version: '1.0'\nallowed-tools: Bash Read\n---\nRead references/checklist.md.\n",
        )
        .unwrap();
        let skill = load_skill_source(root.path(), "review", "did:test", resolve).unwrap();
        assert_eq!(skill.name.as_deref(), Some("code-review"));
        assert_eq!(
            skill.description.as_deref(),
            Some("Review changes with evidence.")
        );
        assert_eq!(
            skill.instructions.as_deref(),
            Some("Read references/checklist.md.")
        );
        assert!(skill.tool_refs.is_empty());
        assert!(skill.interface_json.is_none());
    }

    #[test]
    fn malformed_frontmatter_and_oversized_sources_fail() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("SKILL.md");
        for body in [
            "---\nname: broken",
            "---\nname: [\n---\nBody",
            "---\nname: empty\n---",
        ] {
            std::fs::write(&path, body).unwrap();
            assert!(load_skill_source(&path, "review", "did:test", resolve).is_err());
        }
        std::fs::write(&path, vec![b'x'; MAX_SOURCE_BYTES as usize + 1]).unwrap();
        assert!(load_skill_source(&path, "review", "did:test", resolve)
            .unwrap_err()
            .to_string()
            .contains("1 MiB"));
    }

    #[cfg(unix)]
    #[test]
    fn metadata_is_resolved_through_the_callers_boundary() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("SKILL.md"), "Read the code.").unwrap();
        std::fs::create_dir(root.path().join("agents")).unwrap();
        std::fs::write(outside.path().join("metadata"), "interface: {}").unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("metadata"),
            root.path().join("agents/openai.yaml"),
        )
        .unwrap();
        let allowed = root.path().canonicalize().unwrap();
        let result = load_skill_source(root.path(), "review", "did:test", |path| {
            let path = resolve(path)?;
            if !path.starts_with(&allowed) {
                return Err("outside allowed root".into());
            }
            Ok(path)
        });
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("outside allowed root"));
    }
}
