use anyhow::{Context, Result};
use defra_agent::graphql::escape_graphql_string;
use serde_json::{json, Value};

use crate::cli::*;
use crate::config_writes::ConfigAccess;
use crate::{extract_mutation_doc_id, print_json, EXPORT_SKILL_FIELDS};

fn gql_opt_string(name: &str, value: Option<&str>) -> String {
    match value {
        Some(value) => format!(r#"{name}: "{}""#, escape_graphql_string(value)),
        None => format!("{name}: null"),
    }
}

fn gql_string_list(values: &[String]) -> String {
    let items = values
        .iter()
        .map(|value| format!(r#""{}""#, escape_graphql_string(value)))
        .collect::<Vec<_>>()
        .join(", ");
    format!("[{items}]")
}

/// A skill to create/update, independent of where it came from (CLI flags or a
/// SKILL.md file).
struct SkillInput {
    skill_id: String,
    agent_did: String,
    scope: String,
    name: Option<String>,
    description: Option<String>,
    instructions: Option<String>,
    tool_refs: Vec<String>,
    display_name: Option<String>,
    enabled: bool,
}

/// Upsert a Skill document, returning its `_docID`. An empty `tool_refs` is
/// OMITTED rather than written as `[]`: DefraDB cannot type an empty array
/// literal and rejects it on a later update.
async fn upsert_skill(access: &ConfigAccess, skill: &SkillInput) -> Result<String> {
    let skill_id = escape_graphql_string(&skill.skill_id);
    let mut fields = vec![
        gql_opt_string("agent_did", Some(&skill.agent_did)),
        gql_opt_string("scope", Some(&skill.scope)),
        gql_opt_string("name", skill.name.as_deref()),
        gql_opt_string("description", skill.description.as_deref()),
        gql_opt_string("instructions", skill.instructions.as_deref()),
        gql_opt_string("display_name", skill.display_name.as_deref()),
        format!("enabled: {}", skill.enabled),
    ];
    if !skill.tool_refs.is_empty() {
        fields.push(format!("tool_refs: {}", gql_string_list(&skill.tool_refs)));
    }
    let mutable = fields.join(",\n                    ");
    let created_at = escape_graphql_string(&chrono::Utc::now().to_rfc3339());
    let mutation = format!(
        r#"mutation {{
            upsert_Skill(
                filter: {{ skill_id: {{ _eq: "{skill_id}" }} }},
                add: {{
                    skill_id: "{skill_id}",
                    {mutable},
                    created_at: "{created_at}"
                }},
                update: {{ {mutable} }}
            ) {{ _docID }}
        }}"#
    );
    let response = access.execute(&mutation).await?;
    extract_mutation_doc_id(&response, "Skill")
}

fn validate_scope(scope: &str) -> Result<()> {
    if !matches!(scope, "principal" | "behavior") {
        anyhow::bail!("skill scope must be \"principal\" or \"behavior\", got {scope:?}");
    }
    Ok(())
}

pub(super) async fn skill_add(args: SkillAddArgs) -> Result<()> {
    validate_scope(&args.scope)?;
    let instructions = match args.instructions_file {
        Some(ref path) => Some(
            std::fs::read_to_string(path)
                .with_context(|| format!("reading instructions from {}", path.display()))?,
        ),
        None => args.instructions.clone(),
    };
    let access = ConfigAccess::Graphql(args.graphql.clone());
    let skill = SkillInput {
        skill_id: args.skill_id.clone(),
        agent_did: args.agent_did.clone(),
        scope: args.scope.clone(),
        name: args.name.clone(),
        description: args.description.clone(),
        instructions,
        tool_refs: args.tool_refs.clone(),
        display_name: args.display_name.clone(),
        enabled: args.enabled,
    };
    let doc_id = upsert_skill(&access, &skill).await?;
    print_json(&json!({
        "doc_id": doc_id,
        "skill_id": args.skill_id,
        "agent_did": args.agent_did,
        "scope": args.scope,
        "enabled": args.enabled,
    }))?;
    Ok(())
}

fn skill_rows(response: &Value) -> Vec<Value> {
    response
        .get("data")
        .and_then(|data| data.get("Skill"))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default()
}

pub(super) async fn skill_list(args: SkillListArgs) -> Result<()> {
    let access = ConfigAccess::Graphql(args.graphql.clone());
    let agent_did = escape_graphql_string(&args.agent_did);
    let query = format!(
        r#"{{ Skill(filter: {{ agent_did: {{ _eq: "{agent_did}" }} }}) {{ {EXPORT_SKILL_FIELDS} }} }}"#
    );
    let response = access.execute(&query).await?;
    let mut skills = skill_rows(&response);
    skills.sort_by(|a, b| {
        a.get("skill_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .cmp(b.get("skill_id").and_then(Value::as_str).unwrap_or_default())
    });
    print_json(&json!({ "agent_did": args.agent_did, "count": skills.len(), "skills": skills }))?;
    Ok(())
}

pub(super) async fn skill_show(args: SkillShowArgs) -> Result<()> {
    let access = ConfigAccess::Graphql(args.graphql.clone());
    let skill_id = escape_graphql_string(&args.skill_id);
    let query = format!(
        r#"{{ Skill(filter: {{ skill_id: {{ _eq: "{skill_id}" }} }}, limit: 1) {{ {EXPORT_SKILL_FIELDS} }} }}"#
    );
    let response = access.execute(&query).await?;
    match skill_rows(&response).into_iter().next() {
        Some(skill) => print_json(&skill)?,
        None => anyhow::bail!("no Skill document with skill_id {:?}", args.skill_id),
    }
    Ok(())
}

pub(super) async fn skill_rm(args: SkillRefArgs) -> Result<()> {
    let access = ConfigAccess::Graphql(args.graphql.clone());
    let skill_id = escape_graphql_string(&args.skill_id);
    let mutation = format!(
        r#"mutation {{ delete_Skill(filter: {{ skill_id: {{ _eq: "{skill_id}" }} }}) {{ _docID }} }}"#
    );
    let response = access.execute(&mutation).await?;
    let deleted = response
        .get("data")
        .and_then(|data| data.get("delete_Skill"))
        .and_then(Value::as_array)
        .map(|rows| rows.len())
        .unwrap_or(0);
    if deleted == 0 {
        anyhow::bail!("no Skill document with skill_id {:?}", args.skill_id);
    }
    print_json(&json!({ "deleted": deleted, "skill_id": args.skill_id }))?;
    Ok(())
}

pub(super) async fn skill_set_enabled(args: SkillRefArgs, enabled: bool) -> Result<()> {
    let access = ConfigAccess::Graphql(args.graphql.clone());
    let skill_id = escape_graphql_string(&args.skill_id);
    let mutation = format!(
        r#"mutation {{
            update_Skill(
                filter: {{ skill_id: {{ _eq: "{skill_id}" }} }},
                input: {{ enabled: {enabled} }}
            ) {{ _docID }}
        }}"#
    );
    let response = access.execute(&mutation).await?;
    let updated = response
        .get("data")
        .and_then(|data| data.get("update_Skill"))
        .and_then(Value::as_array)
        .map(|rows| rows.len())
        .unwrap_or(0);
    if updated == 0 {
        anyhow::bail!("no Skill document with skill_id {:?}", args.skill_id);
    }
    print_json(&json!({ "skill_id": args.skill_id, "enabled": enabled, "updated": updated }))?;
    Ok(())
}

// ---- Import a Codex-format SKILL.md directory tree ----

#[derive(Default, serde::Deserialize)]
struct SkillFrontmatter {
    name: Option<String>,
    description: Option<String>,
}

#[derive(Default, serde::Deserialize)]
struct OpenAiYaml {
    interface: Option<OpenAiInterface>,
    dependencies: Option<OpenAiDependencies>,
}
#[derive(Default, serde::Deserialize)]
struct OpenAiInterface {
    display_name: Option<String>,
}
#[derive(Default, serde::Deserialize)]
struct OpenAiDependencies {
    #[serde(default)]
    tools: Vec<OpenAiTool>,
}
#[derive(Default, serde::Deserialize)]
struct OpenAiTool {
    value: Option<String>,
}

/// Split a SKILL.md into (frontmatter, body): the YAML between a leading `---`
/// line and the next `---` line, then everything after as the instruction body.
fn parse_skill_md(contents: &str) -> (SkillFrontmatter, String) {
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
            let frontmatter = serde_yaml::from_str(&yaml).unwrap_or_default();
            return (frontmatter, body.join("\n").trim().to_string());
        }
    }
    (SkillFrontmatter::default(), contents.trim().to_string())
}

/// Derive a stable `skill_id` from a skill directory name: lowercased, with
/// runs of non-alphanumeric characters collapsed to single hyphens.
fn skill_id_from_dir(dir: &std::path::Path) -> Option<String> {
    let raw = dir.file_name()?.to_string_lossy();
    let mut id = String::new();
    let mut prev_dash = false;
    for ch in raw.chars() {
        if ch.is_ascii_alphanumeric() {
            id.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash && !id.is_empty() {
            id.push('-');
            prev_dash = true;
        }
    }
    let id = id.trim_end_matches('-').to_string();
    (!id.is_empty()).then_some(id)
}

/// Recursively find directories that directly contain a `SKILL.md`, depth-bounded.
fn find_skill_dirs(root: &std::path::Path, max_depth: usize) -> Vec<std::path::PathBuf> {
    let mut found = Vec::new();
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') {
                continue;
            }
            let path = entry.path();
            if path.is_file() && name == "SKILL.md" {
                found.push(dir.clone());
            } else if path.is_dir() && depth < max_depth {
                stack.push((path, depth + 1));
            }
        }
    }
    found.sort();
    found.dedup();
    found
}

pub(super) async fn skill_import(args: SkillImportArgs) -> Result<()> {
    validate_scope(&args.scope)?;
    if !args.dir.is_dir() {
        anyhow::bail!("{} is not a directory", args.dir.display());
    }
    let access = ConfigAccess::Graphql(args.graphql.clone());

    let mut imported = Vec::new();
    let mut errors = Vec::new();

    for dir in find_skill_dirs(&args.dir, 6) {
        let Some(skill_id) = skill_id_from_dir(&dir) else {
            errors.push(json!({
                "dir": dir.display().to_string(),
                "error": "could not derive skill_id from directory name",
            }));
            continue;
        };
        let contents = match std::fs::read_to_string(dir.join("SKILL.md")) {
            Ok(contents) => contents,
            Err(error) => {
                errors.push(json!({ "skill_id": skill_id, "error": format!("reading SKILL.md: {error}") }));
                continue;
            }
        };
        let (frontmatter, body) = parse_skill_md(&contents);

        // Optional agents/openai.yaml: tool dependencies + display name.
        let mut tool_refs = Vec::new();
        let mut display_name = None;
        if let Ok(yaml) = std::fs::read_to_string(dir.join("agents").join("openai.yaml")) {
            match serde_yaml::from_str::<OpenAiYaml>(&yaml) {
                Ok(parsed) => {
                    if let Some(deps) = parsed.dependencies {
                        tool_refs = deps
                            .tools
                            .into_iter()
                            .filter_map(|tool| tool.value)
                            .filter(|value| !value.trim().is_empty())
                            .collect();
                    }
                    display_name = parsed.interface.and_then(|interface| interface.display_name);
                }
                Err(error) => errors.push(json!({
                    "skill_id": skill_id,
                    "error": format!("parsing agents/openai.yaml: {error}"),
                })),
            }
        }

        let name = frontmatter.name.clone().unwrap_or_else(|| skill_id.clone());
        let skill = SkillInput {
            skill_id: skill_id.clone(),
            agent_did: args.agent_did.clone(),
            scope: args.scope.clone(),
            name: Some(name.clone()),
            description: frontmatter.description.clone(),
            instructions: (!body.is_empty()).then_some(body),
            tool_refs: tool_refs.clone(),
            display_name,
            enabled: !args.disabled,
        };

        if args.dry_run {
            imported.push(json!({
                "skill_id": skill_id,
                "name": name,
                "description": frontmatter.description,
                "tool_refs": tool_refs,
                "source": dir.join("SKILL.md").display().to_string(),
            }));
            continue;
        }

        match upsert_skill(&access, &skill).await {
            Ok(doc_id) => {
                imported.push(json!({ "skill_id": skill_id, "name": name, "doc_id": doc_id }))
            }
            Err(error) => {
                errors.push(json!({ "skill_id": skill_id, "error": error.to_string() }))
            }
        }
    }

    print_json(&json!({
        "agent_did": args.agent_did,
        "scope": args.scope,
        "dry_run": args.dry_run,
        "imported_count": imported.len(),
        "imported": imported,
        "errors": errors,
    }))?;
    if !errors.is_empty() {
        anyhow::bail!("{} skill(s) failed to import", errors.len());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_skill_md_splits_frontmatter_and_body() {
        let md = "---\nname: Research\ndescription: Find sources\n---\n\nAlways cite your sources.\n";
        let (fm, body) = parse_skill_md(md);
        assert_eq!(fm.name.as_deref(), Some("Research"));
        assert_eq!(fm.description.as_deref(), Some("Find sources"));
        assert_eq!(body, "Always cite your sources.");
    }

    #[test]
    fn parse_skill_md_without_frontmatter_is_all_body() {
        let (fm, body) = parse_skill_md("Just instructions.\n");
        assert!(fm.name.is_none());
        assert_eq!(body, "Just instructions.");
    }

    #[test]
    fn skill_id_from_dir_sanitizes() {
        let id = skill_id_from_dir(std::path::Path::new("/x/Code Review_v2")).unwrap();
        assert_eq!(id, "code-review-v2");
    }

    #[test]
    fn openai_yaml_extracts_tool_refs_and_display_name() {
        let yaml = "interface:\n  display_name: Research\ndependencies:\n  tools:\n    - type: mcp\n      value: web_search\n    - type: mcp\n      value: read_file\n";
        let parsed: OpenAiYaml = serde_yaml::from_str(yaml).unwrap();
        let tools: Vec<String> = parsed
            .dependencies
            .unwrap()
            .tools
            .into_iter()
            .filter_map(|tool| tool.value)
            .collect();
        assert_eq!(tools, vec!["web_search", "read_file"]);
        assert_eq!(
            parsed.interface.unwrap().display_name.as_deref(),
            Some("Research")
        );
    }

    #[test]
    fn find_skill_dirs_discovers_nested_skill_md() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        std::fs::create_dir_all(root.join("research")).unwrap();
        std::fs::write(root.join("research/SKILL.md"), "x").unwrap();
        std::fs::create_dir_all(root.join("group/writing/scripts")).unwrap();
        std::fs::write(root.join("group/writing/SKILL.md"), "y").unwrap();
        std::fs::create_dir_all(root.join(".hidden")).unwrap();
        std::fs::write(root.join(".hidden/SKILL.md"), "z").unwrap();

        let dirs = find_skill_dirs(root, 6);
        let names: Vec<String> = dirs
            .iter()
            .map(|d| d.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(names.contains(&"research".to_string()));
        assert!(names.contains(&"writing".to_string()));
        assert!(!names.iter().any(|n| n == ".hidden")); // dotdirs skipped
        assert_eq!(dirs.len(), 2);
    }
}
