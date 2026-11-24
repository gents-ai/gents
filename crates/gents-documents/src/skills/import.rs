//! Importing skill instructions never executes supporting files.

use std::io::Read;
use std::path::Path;

use anyhow::{Context, Result};
use gents_fs_runner::beneath::{Access, Located};

use crate::SkillDocument;

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

pub fn parse_skill_md(contents: &str) -> anyhow::Result<(SkillFrontmatter, String)> {
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
            let frontmatter = serde_yaml::from_str(&yaml)
                .map_err(|error| anyhow::anyhow!("invalid SKILL.md frontmatter: {error}"))?;
            return Ok((frontmatter, body.join("\n").trim().to_string()));
        }
        anyhow::bail!("SKILL.md frontmatter is missing its closing --- delimiter");
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
/// caller resolves and authorizes each path, and every read goes through the
/// directory handle it was resolved in.
pub fn load_skill_source(
    source: &Path,
    skill_id: &str,
    node_did: &str,
    resolve: impl Fn(&Path) -> Result<Located>,
) -> Result<SkillDocument> {
    anyhow::ensure!(!skill_id.trim().is_empty(), "skill_id must not be blank");
    let source = resolve(source)?;
    let file = if source.is_dir() {
        resolve(&source.path().join("SKILL.md"))?
    } else {
        anyhow::ensure!(
            source.path().file_name().is_some_and(|n| n == "SKILL.md"),
            "expected a skill directory or a SKILL.md file"
        );
        source
    };
    let contents = read_source(&file)?;
    let (frontmatter, body) = parse_skill_md(&contents)?;
    anyhow::ensure!(
        !body.is_empty(),
        "SKILL.md must contain instructions after its frontmatter"
    );
    let directory = file
        .path()
        .parent()
        .context("SKILL.md has no parent directory")?
        .to_path_buf();
    let metadata_relative = file
        .relative()
        .parent()
        .unwrap_or(Path::new(""))
        .join("agents/openai.yaml");
    let metadata = match file.root().kind(&metadata_relative) {
        Ok(None) => OpenAiYaml::default(),
        Ok(Some(_)) | Err(_) => {
            let located = resolve(&directory.join("agents/openai.yaml"))?;
            serde_yaml::from_str::<OpenAiYaml>(&read_source(&located)?)
                .with_context(|| format!("invalid metadata in {}", located.path().display()))?
        }
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
        .transpose()?;
    Ok(SkillDocument {
        skill_id: skill_id.to_owned(),
        node_did: node_did.to_owned(),
        name: Some(frontmatter.name.unwrap_or_else(|| skill_id.to_owned())),
        description: frontmatter.description,
        instructions: Some(body),
        source_directory: Some(
            directory
                .to_str()
                .context("skill source directory must be UTF-8")?
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

fn read_source(file: &Located) -> Result<String> {
    let path = file.path();
    let opened = file
        .open(Access::Read)
        .with_context(|| format!("{} is not a regular file", path.display()))?;
    let mut bytes = Vec::new();
    opened.take(MAX_SOURCE_BYTES + 1).read_to_end(&mut bytes)?;
    anyhow::ensure!(
        bytes.len() as u64 <= MAX_SOURCE_BYTES,
        "{} exceeds the 1 MiB skill source limit",
        path.display()
    );
    String::from_utf8(bytes).with_context(|| format!("{} must be UTF-8", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve(path: &Path) -> Result<Located> {
        Ok(Located::at(
            path,
            gents_fs_runner::beneath::Intent::Existing,
        )?)
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
        let allowed = gents_fs_runner::beneath::RootDir::open(root.path()).unwrap();
        let result = load_skill_source(root.path(), "review", "did:test", |path| {
            Ok(allowed.locate(
                path,
                allowed.path(),
                gents_fs_runner::beneath::Intent::Existing,
            )?)
        });
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("outside the allowed tool root"));
    }
}
