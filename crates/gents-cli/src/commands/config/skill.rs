use anyhow::{Context, Result};
use gents::graphql::escape_graphql_string;
use serde_json::{json, Value};

use crate::cli::*;
use crate::config_writes::ConfigAccess;
use crate::{extract_mutation_doc_id, print_json, EXPORT_SKILL_FIELDS};

async fn execute_committed(access: &ConfigAccess, mutation: &str) -> Result<Value> {
    let txn = access.begin_apply_txn().await?;
    match txn.execute(mutation).await {
        Ok(response) => {
            txn.commit().await?;
            Ok(response)
        }
        Err(error) => {
            let _ = txn.discard().await;
            Err(error)
        }
    }
}

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

struct SkillInput {
    skill_id: String,
    agent_did: String,
    scope: String,
    name: Option<String>,
    description: Option<String>,
    instructions: Option<String>,
    tool_refs: Vec<String>,
    display_name: Option<String>,
    interface_json: Option<String>,
    enabled: bool,
}

/// Upsert a Skill document, returning its `_docID`. An empty `tool_refs` is
/// written as `null`, not `[]` (DefraDB cannot type an empty array literal):
/// `null` is accepted on create and, crucially, CLEARS a previously non-empty
/// list on the upsert's update path (omitting it would leave the stale value).
async fn upsert_skill(access: &ConfigAccess, skill: &SkillInput) -> Result<String> {
    let skill_id = escape_graphql_string(&skill.skill_id);
    let tool_refs = if skill.tool_refs.is_empty() {
        "tool_refs: null".to_string()
    } else {
        format!("tool_refs: {}", gql_string_list(&skill.tool_refs))
    };
    let fields = vec![
        gql_opt_string("agent_did", Some(&skill.agent_did)),
        gql_opt_string("scope", Some(&skill.scope)),
        gql_opt_string("name", skill.name.as_deref()),
        gql_opt_string("description", skill.description.as_deref()),
        gql_opt_string("instructions", skill.instructions.as_deref()),
        gql_opt_string("display_name", skill.display_name.as_deref()),
        gql_opt_string("interface_json", skill.interface_json.as_deref()),
        format!("enabled: {}", skill.enabled),
        tool_refs,
    ];
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
    let response = execute_committed(access, &mutation).await?;
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
    let access = crate::authenticated_default_graphql_access(&args.graphql).await?;
    let skill = SkillInput {
        skill_id: args.skill_id.clone(),
        agent_did: args.agent_did.clone(),
        scope: args.scope.clone(),
        name: args.name.clone(),
        description: args.description.clone(),
        instructions,
        tool_refs: args.tool_refs.clone(),
        display_name: args.display_name.clone(),
        interface_json: None,
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
    let access = crate::authenticated_default_graphql_access(&args.graphql).await?;
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
            .cmp(
                b.get("skill_id")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            )
    });
    print_json(&json!({ "agent_did": args.agent_did, "count": skills.len(), "skills": skills }))?;
    Ok(())
}

pub(super) async fn skill_show(args: SkillShowArgs) -> Result<()> {
    let access = crate::authenticated_default_graphql_access(&args.graphql).await?;
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
    let access = crate::authenticated_default_graphql_access(&args.graphql).await?;
    let skill_id = escape_graphql_string(&args.skill_id);
    let mutation = format!(
        r#"mutation {{ delete_Skill(filter: {{ skill_id: {{ _eq: "{skill_id}" }} }}) {{ _docID }} }}"#
    );
    let response = execute_committed(&access, &mutation).await?;
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
    let access = crate::authenticated_default_graphql_access(&args.graphql).await?;
    let skill_id = escape_graphql_string(&args.skill_id);
    let mutation = format!(
        r#"mutation {{
            update_Skill(
                filter: {{ skill_id: {{ _eq: "{skill_id}" }} }},
                input: {{ enabled: {enabled} }}
            ) {{ _docID }}
        }}"#
    );
    let response = execute_committed(&access, &mutation).await?;
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

#[derive(Default, serde::Deserialize)]
struct SkillFrontmatter {
    name: Option<String>,
    description: Option<String>,
}

#[derive(Default, serde::Deserialize)]
struct OpenAiYaml {
    interface: Option<serde_yaml::Value>,
    dependencies: Option<OpenAiDependencies>,
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
    let access = crate::authenticated_default_graphql_access(&args.graphql).await?;

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
                errors.push(
                    json!({ "skill_id": skill_id, "error": format!("reading SKILL.md: {error}") }),
                );
                continue;
            }
        };
        let (frontmatter, body) = parse_skill_md(&contents);

        let mut tool_refs = Vec::new();
        let mut display_name = None;
        let mut interface_json = None;
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
                    if let Some(interface) = parsed.interface {
                        display_name = interface
                            .get("display_name")
                            .and_then(serde_yaml::Value::as_str)
                            .filter(|value| !value.trim().is_empty())
                            .map(ToOwned::to_owned);
                        interface_json = serde_json::to_string(&interface).ok();
                    }
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
            interface_json,
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
            Err(error) => errors.push(json!({ "skill_id": skill_id, "error": error.to_string() })),
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
    if imported.is_empty() && !errors.is_empty() {
        anyhow::bail!("no skills imported ({} error(s))", errors.len());
    }
    Ok(())
}

fn render_skill_md(skill: &Value) -> Result<String> {
    #[derive(serde::Serialize)]
    struct Frontmatter<'a> {
        name: &'a str,
        #[serde(skip_serializing_if = "Option::is_none")]
        description: Option<&'a str>,
    }
    let name = skill
        .get("name")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .or_else(|| skill.get("skill_id").and_then(Value::as_str))
        .unwrap_or_default();
    let description = skill
        .get("description")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty());
    let instructions = skill
        .get("instructions")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let frontmatter = serde_yaml::to_string(&Frontmatter { name, description })?;
    Ok(format!("---\n{frontmatter}---\n\n{instructions}\n"))
}

fn render_openai_yaml(skill: &Value) -> Result<Option<String>> {
    #[derive(serde::Serialize)]
    struct Tool {
        #[serde(rename = "type")]
        kind: &'static str,
        value: String,
    }
    #[derive(serde::Serialize)]
    struct Dependencies {
        tools: Vec<Tool>,
    }
    #[derive(serde::Serialize)]
    struct OpenAi {
        #[serde(skip_serializing_if = "Option::is_none")]
        interface: Option<serde_yaml::Value>,
        #[serde(skip_serializing_if = "Option::is_none")]
        dependencies: Option<Dependencies>,
    }
    let tool_refs: Vec<String> = skill
        .get("tool_refs")
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(ToOwned::to_owned)
                .collect()
        })
        .unwrap_or_default();
    let interface = match skill
        .get("interface_json")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
    {
        Some(raw) => {
            let json: Value = serde_json::from_str(raw).context("parsing stored interface_json")?;
            Some(serde_yaml::to_value(&json)?)
        }
        None => skill
            .get("display_name")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(|display_name| {
                serde_yaml::to_value(serde_json::json!({ "display_name": display_name }))
            })
            .transpose()?,
    };
    if tool_refs.is_empty() && interface.is_none() {
        return Ok(None);
    }
    let openai = OpenAi {
        interface,
        dependencies: (!tool_refs.is_empty()).then(|| Dependencies {
            tools: tool_refs
                .into_iter()
                .map(|value| Tool { kind: "mcp", value })
                .collect(),
        }),
    };
    Ok(Some(serde_yaml::to_string(&openai)?))
}

pub(super) async fn skill_export(args: SkillExportArgs) -> Result<()> {
    let access = crate::authenticated_default_graphql_access(&args.graphql).await?;
    let agent_did = escape_graphql_string(&args.agent_did);
    let query = format!(
        r#"{{ Skill(filter: {{ agent_did: {{ _eq: "{agent_did}" }} }}) {{ {EXPORT_SKILL_FIELDS} }} }}"#
    );
    let response = access.execute(&query).await?;
    let skills = skill_rows(&response);

    let mut exported = Vec::new();
    for skill in &skills {
        let skill_id = skill
            .get("skill_id")
            .and_then(Value::as_str)
            .unwrap_or_default();
        if skill_id.trim().is_empty() {
            continue;
        }
        let dir = args.dir.join(skill_id);
        std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        std::fs::write(dir.join("SKILL.md"), render_skill_md(skill)?)
            .with_context(|| format!("writing {}/SKILL.md", dir.display()))?;
        if let Some(yaml) = render_openai_yaml(skill)? {
            let agents_dir = dir.join("agents");
            std::fs::create_dir_all(&agents_dir)
                .with_context(|| format!("creating {}", agents_dir.display()))?;
            std::fs::write(agents_dir.join("openai.yaml"), yaml)
                .with_context(|| format!("writing {}/openai.yaml", agents_dir.display()))?;
        }
        exported.push(json!({
            "skill_id": skill_id,
            "path": dir.join("SKILL.md").display().to_string(),
        }));
    }

    print_json(&json!({
        "agent_did": args.agent_did,
        "dir": args.dir.display().to_string(),
        "exported_count": exported.len(),
        "exported": exported,
    }))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_skill_md_splits_frontmatter_and_body() {
        let md =
            "---\nname: Research\ndescription: Find sources\n---\n\nAlways cite your sources.\n";
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
            parsed
                .interface
                .unwrap()
                .get("display_name")
                .and_then(serde_yaml::Value::as_str),
            Some("Research")
        );
    }

    #[test]
    fn export_render_round_trips_through_import_parser() {
        let skill = json!({
            "skill_id": "research",
            "name": "Research",
            "description": "Find sources: cite everything.",
            "instructions": "Always cite your sources.\n\nUse primary references.",
        });
        let md = render_skill_md(&skill).unwrap();
        let (fm, body) = parse_skill_md(&md);
        assert_eq!(fm.name.as_deref(), Some("Research"));
        assert_eq!(
            fm.description.as_deref(),
            Some("Find sources: cite everything.")
        );
        assert_eq!(body, "Always cite your sources.\n\nUse primary references.");
    }

    #[test]
    fn export_openai_yaml_round_trips_tool_refs() {
        let skill = json!({
            "skill_id": "research",
            "tool_refs": ["web_search", "read_file"],
            "display_name": "Research",
        });
        let yaml = render_openai_yaml(&skill).unwrap().expect("openai.yaml");
        let parsed: OpenAiYaml = serde_yaml::from_str(&yaml).unwrap();
        let tools: Vec<String> = parsed
            .dependencies
            .unwrap()
            .tools
            .into_iter()
            .filter_map(|tool| tool.value)
            .collect();
        assert_eq!(tools, vec!["web_search", "read_file"]);
        assert_eq!(
            parsed
                .interface
                .unwrap()
                .get("display_name")
                .and_then(serde_yaml::Value::as_str),
            Some("Research")
        );

        assert!(render_openai_yaml(&json!({ "skill_id": "x" }))
            .unwrap()
            .is_none());
    }

    #[test]
    fn import_captures_and_export_round_trips_opaque_interface() {
        let yaml = "interface:\n  display_name: Research\n  icon: telescope\n  brand:\n    color: \"#0af\"\n";
        let parsed: OpenAiYaml = serde_yaml::from_str(yaml).unwrap();
        let interface = parsed.interface.expect("interface present");
        let display_name = interface
            .get("display_name")
            .and_then(serde_yaml::Value::as_str)
            .map(ToOwned::to_owned);
        let interface_json = serde_json::to_string(&interface).unwrap();
        assert_eq!(display_name.as_deref(), Some("Research"));
        assert!(interface_json.contains("icon"));
        assert!(interface_json.contains("telescope"));

        let skill = json!({
            "skill_id": "research",
            "display_name": display_name,
            "interface_json": interface_json,
        });
        let exported = render_openai_yaml(&skill).unwrap().expect("openai.yaml");
        let reparsed: OpenAiYaml = serde_yaml::from_str(&exported).unwrap();
        let reparsed_interface = reparsed.interface.expect("interface round-trips");
        assert_eq!(
            reparsed_interface
                .get("display_name")
                .and_then(serde_yaml::Value::as_str),
            Some("Research")
        );
        assert_eq!(
            reparsed_interface
                .get("icon")
                .and_then(serde_yaml::Value::as_str),
            Some("telescope"),
            "opaque interface fields beyond display_name must round-trip"
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
        assert!(!names.iter().any(|n| n == ".hidden"));
        assert_eq!(dirs.len(), 2);
    }
}
