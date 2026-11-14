use anyhow::{Context, Result};
use gents_protocol::operations::document::WriteMode;
use gents_protocol::operations::skill::{SkillEdit, SkillOperationError};
use gents_protocol::operations::StringUpdate;
use serde_json::{json, Value};

use crate::cli::*;
use crate::commands::refusal::Refusal;
use crate::config_writes::ConfigAccess;
use crate::print_json;
use gents::operations::skill::{SkillImport, SkillSourceRoot, SkillWrite};

async fn access(graphql: &str) -> Result<ConfigAccess> {
    Ok(ConfigAccess::Graphql(
        gents_service::home_state::resolve_graphql_endpoint(Some(graphql), None).await?,
    ))
}

fn refused(error: SkillOperationError) -> anyhow::Error {
    let next = error.recovery();
    Refusal::new(error, next).into()
}

fn text(value: Option<String>, cleared: bool) -> StringUpdate {
    match value {
        Some(value) => StringUpdate::Set(value),
        None if cleared => StringUpdate::Clear,
        None => StringUpdate::Omitted,
    }
}

pub(super) async fn skill_add(args: SkillAddArgs) -> Result<()> {
    const CLEARABLE: [&str; 5] = [
        "name",
        "description",
        "instructions",
        "display_name",
        "tool_refs",
    ];
    for field in &args.clear {
        anyhow::ensure!(
            CLEARABLE.contains(&field.as_str()),
            "--clear accepts {}; got {field:?}",
            CLEARABLE.join(", ")
        );
    }
    let cleared = |field: &str| args.clear.iter().any(|name| name == field);
    let instructions = match args.instructions_file {
        Some(ref path) => Some(
            std::fs::read_to_string(path)
                .with_context(|| format!("reading instructions from {}", path.display()))?,
        ),
        None => args.instructions.clone(),
    };
    let edit = SkillEdit {
        name: text(args.name.clone(), cleared("name")),
        description: text(args.description.clone(), cleared("description")),
        instructions: text(instructions, cleared("instructions")),
        display_name: text(args.display_name.clone(), cleared("display_name")),
        tool_refs: if args.tool_refs.is_empty() {
            cleared("tool_refs").then(Vec::new)
        } else {
            Some(args.tool_refs.clone())
        },
        enabled: args.enabled,
    };
    let access = access(&args.graphql).await?;
    let receipt = gents::operations::skill::save(
        &access,
        &SkillWrite {
            owner: &args.node_did,
            skill_id: &args.skill_id,
            mode: WriteMode::Save,
            patch: &edit.patch(),
            node_binding: None,
        },
    )
    .await
    .map_err(refused)?;
    print_json(&json!({
        "doc_id": receipt.doc_id,
        "skill_id": receipt.id,
        "node_did": receipt.node_did,
        "created": receipt.created,
        "changed": receipt.changed.iter().map(|change| &change.field).collect::<Vec<_>>(),
    }))
}

pub(super) async fn skill_list(args: SkillListArgs) -> Result<()> {
    let skills =
        gents::operations::skill::list(&access(&args.graphql).await?, &args.node_did).await?;
    print_json(&json!({ "node_did": args.node_did, "count": skills.len(), "skills": skills }))
}

pub(super) async fn skill_show(args: SkillShowArgs) -> Result<()> {
    let access = access(&args.graphql).await?;
    let owner =
        gents::operations::skill::resolve_owner(&access, args.node_did.as_deref(), &args.skill_id)
            .await
            .map_err(refused)?;
    let skill = gents::operations::skill::get(&access, &owner, &args.skill_id)
        .await
        .map_err(refused)?;
    print_json(&skill)
}

pub(super) async fn skill_rm(args: SkillRefArgs) -> Result<()> {
    let access = access(&args.graphql).await?;
    let owner =
        gents::operations::skill::resolve_owner(&access, args.node_did.as_deref(), &args.skill_id)
            .await
            .map_err(refused)?;
    let receipt = gents::operations::skill::delete(&access, &owner, &args.skill_id)
        .await
        .map_err(refused)?;
    print_json(&json!({
        "deleted": receipt.targets.len(),
        "skill_id": args.skill_id,
        "node_did": receipt.owner,
    }))
}

pub(super) async fn skill_set_enabled(args: SkillRefArgs, enabled: bool) -> Result<()> {
    let access = access(&args.graphql).await?;
    let owner =
        gents::operations::skill::resolve_owner(&access, args.node_did.as_deref(), &args.skill_id)
            .await
            .map_err(refused)?;
    let receipt = gents::operations::skill::save(
        &access,
        &SkillWrite {
            owner: &owner,
            skill_id: &args.skill_id,
            mode: WriteMode::Update,
            patch: &SkillEdit::enabled(enabled).patch(),
            node_binding: None,
        },
    )
    .await
    .map_err(refused)?;
    print_json(&json!({
        "skill_id": receipt.id,
        "node_did": receipt.node_did,
        "enabled": enabled,
    }))
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
    let source = std::fs::canonicalize(&args.dir)
        .with_context(|| format!("resolving {}", args.dir.display()))?;
    let access = access(&args.graphql).await?;

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
        let import = SkillImport {
            owner: &args.node_did,
            skill_id: &skill_id,
            source: &dir,
            root: SkillSourceRoot::Unconfined,
            overwrite: args.overwrite,
            enabled: !args.disabled,
            node_binding: None,
        };
        let result = if args.dry_run {
            gents::operations::skill::preview_import(&access, &import).await
        } else {
            gents::operations::skill::import(&access, &import).await
        };
        match result {
            Ok(receipt) => imported.push(json!({
                "skill_id": skill_id,
                "created": receipt.created,
                "doc_id": receipt.doc_id,
                "source": dir.join("SKILL.md").display().to_string(),
            })),
            Err(error) => errors.push(json!({
                "skill_id": skill_id,
                "error": refused(error).to_string(),
            })),
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
    let skills =
        gents::operations::skill::list(&access(&args.graphql).await?, &args.node_did).await?;

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
