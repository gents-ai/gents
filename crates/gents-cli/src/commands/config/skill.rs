use anyhow::{Context, Result};
use gents::documents::SkillDocument;
use gents::store::graphql::escape_graphql_string;
use gents::Collection;
use serde_json::{json, Value};

use crate::cli::*;
use crate::config_writes::ConfigAccess;
use crate::{print_json, EXPORT_SKILL_FIELDS};

/// Applies `skill` through the desired-state owner, so a served endpoint
/// signs it as the client. `null` fields clear stale values on update, and
/// `created_at` is set only when the Skill is created.
async fn upsert_skill(access: &ConfigAccess, skill: &SkillDocument) -> Result<String> {
    use gents::documents::apply_desired_state_plan;
    use gents::store::config_client::{
        read_desired_state_record_in_txn, DesiredStateApplyDocument, DesiredStateApplyPlan,
    };
    let mut update = serde_json::to_value(skill)?;
    let fields = update
        .as_object_mut()
        .context("Skill document is an object")?;
    fields.remove("created_at");
    if skill.tool_refs.is_empty() {
        fields.insert("tool_refs".into(), Value::Null);
    }
    if skill.tags.is_empty() {
        fields.insert("tags".into(), Value::Null);
    }
    let mut add = update.clone();
    add["created_at"] = json!(chrono::Utc::now().to_rfc3339());
    let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
        collection: Collection::Skill,
        add,
        update,
    }])?;
    let (node_did, skill_id) = (skill.node_did.as_str(), skill.skill_id.as_str());
    access
        .transact_write("cli.config.skill.upsert", |txn| {
            let plan = &plan;
            Box::pin(async move {
                apply_desired_state_plan(txn, plan).await?;
                let (doc_id, _) =
                    read_desired_state_record_in_txn(txn, Collection::Skill, node_did, skill_id)
                        .await?
                        .context("the applied Skill is not readable")?;
                Ok::<_, anyhow::Error>(doc_id)
            })
        })
        .await
}

/// The Skill rows with `skill_id`, any owner.
async fn find_skills(access: &ConfigAccess, skill_id: &str) -> Result<Vec<Value>> {
    let skill_id = escape_graphql_string(skill_id);
    let response = access
        .execute(&format!(
            r#"{{ Skill(filter: {{ skill_id: {{ _eq: "{skill_id}" }} }}) {{ {EXPORT_SKILL_FIELDS} }} }}"#
        ))
        .await?;
    Ok(skill_rows(&response))
}

pub(super) async fn skill_add(args: SkillAddArgs) -> Result<()> {
    let instructions = match args.instructions_file {
        Some(ref path) => Some(
            std::fs::read_to_string(path)
                .with_context(|| format!("reading instructions from {}", path.display()))?,
        ),
        None => args.instructions.clone(),
    };
    let access = ConfigAccess::Graphql(crate::resolve_graphql_endpoint(Some(&args.graphql), None)?);
    let skill = SkillDocument {
        skill_id: args.skill_id.clone(),
        node_did: args.node_did.clone(),
        name: args.name.clone(),
        description: args.description.clone(),
        instructions,
        source_directory: None,
        tool_refs: args.tool_refs.clone(),
        display_name: args.display_name.clone(),
        interface_json: None,
        enabled: args.enabled,
        created_at: None,
        tags: Vec::new(),
    };
    let doc_id = upsert_skill(&access, &skill).await?;
    print_json(&json!({
        "doc_id": doc_id,
        "skill_id": args.skill_id,
        "node_did": args.node_did,
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
    let access = ConfigAccess::Graphql(crate::resolve_graphql_endpoint(Some(&args.graphql), None)?);
    let node_did = escape_graphql_string(&args.node_did);
    let query = format!(
        r#"{{ Skill(filter: {{ node_did: {{ _eq: "{node_did}" }} }}) {{ {EXPORT_SKILL_FIELDS} }} }}"#
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
    print_json(&json!({ "node_did": args.node_did, "count": skills.len(), "skills": skills }))?;
    Ok(())
}

pub(super) async fn skill_show(args: SkillShowArgs) -> Result<()> {
    let access = ConfigAccess::Graphql(crate::resolve_graphql_endpoint(Some(&args.graphql), None)?);
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
    let access = ConfigAccess::Graphql(crate::resolve_graphql_endpoint(Some(&args.graphql), None)?);
    let mut deleted = 0;
    for skill in find_skills(&access, &args.skill_id).await? {
        let node_did = skill["node_did"]
            .as_str()
            .context("Skill row has no node_did")?
            .to_owned();
        let ids = [args.skill_id.clone()];
        deleted += access
            .transact_write("cli.config.skill.delete", |txn| {
                let (node_did, ids) = (&node_did, &ids);
                Box::pin(async move {
                    crate::config_import::apply_delete_collection(
                        txn,
                        Collection::Skill,
                        node_did,
                        ids,
                    )
                    .await
                })
            })
            .await?;
    }
    if deleted == 0 {
        anyhow::bail!("no Skill document with skill_id {:?}", args.skill_id);
    }
    print_json(&json!({ "deleted": deleted, "skill_id": args.skill_id }))?;
    Ok(())
}

pub(super) async fn skill_set_enabled(args: SkillRefArgs, enabled: bool) -> Result<()> {
    let access = ConfigAccess::Graphql(crate::resolve_graphql_endpoint(Some(&args.graphql), None)?);
    let mut updated = 0;
    for row in find_skills(&access, &args.skill_id).await? {
        let mut skill: SkillDocument =
            serde_json::from_value(row).context("decoding the stored Skill")?;
        skill.enabled = enabled;
        upsert_skill(&access, &skill).await?;
        updated += 1;
    }
    if updated == 0 {
        anyhow::bail!("no Skill document with skill_id {:?}", args.skill_id);
    }
    print_json(&json!({ "skill_id": args.skill_id, "enabled": enabled, "updated": updated }))?;
    Ok(())
}

use gents::documents::skills::import::load_skill_source;

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
    let source = std::fs::canonicalize(&args.dir)
        .with_context(|| format!("resolving {}", args.dir.display()))?;
    let access = ConfigAccess::Graphql(crate::resolve_graphql_endpoint(Some(&args.graphql), None)?);

    let mut imported = Vec::new();
    let mut errors = Vec::new();

    let sources = if source.is_file() {
        anyhow::ensure!(
            source.file_name().is_some_and(|name| name == "SKILL.md"),
            "expected SKILL.md"
        );
        vec![source
            .parent()
            .context("SKILL.md has no parent")?
            .to_path_buf()]
    } else {
        find_skill_dirs(&source, 6)
    };
    for dir in sources {
        let Some(skill_id) = skill_id_from_dir(&dir) else {
            errors.push(json!({
                "dir": dir.display().to_string(),
                "error": "could not derive skill_id from directory name",
            }));
            continue;
        };
        let loaded = match load_skill_source(&dir, &skill_id, &args.node_did, |path| {
            std::fs::canonicalize(path).with_context(|| format!("resolving {}", path.display()))
        }) {
            Ok(skill) => skill,
            Err(error) => {
                errors.push(json!({"skill_id": skill_id, "error": format!("{error:#}")}));
                continue;
            }
        };
        let name = loaded.name.clone().unwrap_or_else(|| skill_id.clone());
        let mut skill = loaded;
        skill.enabled = !args.disabled;

        if args.dry_run {
            imported.push(json!({
                "skill_id": skill_id,
                "name": name,
                "description": skill.description,
                "tool_refs": skill.tool_refs,
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
        "node_did": args.node_did,
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
    let access = ConfigAccess::Graphql(crate::resolve_graphql_endpoint(Some(&args.graphql), None)?);
    let node_did = escape_graphql_string(&args.node_did);
    let query = format!(
        r#"{{ Skill(filter: {{ node_did: {{ _eq: "{node_did}" }} }}) {{ {EXPORT_SKILL_FIELDS} }} }}"#
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
        "node_did": args.node_did,
        "dir": args.dir.display().to_string(),
        "exported_count": exported.len(),
        "exported": exported,
    }))?;
    Ok(())
}
