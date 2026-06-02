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

pub(super) async fn skill_add(args: SkillAddArgs) -> Result<()> {
    if !matches!(args.scope.as_str(), "principal" | "behavior") {
        anyhow::bail!(
            "skill scope must be \"principal\" or \"behavior\", got {:?}",
            args.scope
        );
    }
    let instructions = match args.instructions_file {
        Some(ref path) => Some(
            std::fs::read_to_string(path)
                .with_context(|| format!("reading instructions from {}", path.display()))?,
        ),
        None => args.instructions.clone(),
    };
    let access = ConfigAccess::Graphql(args.graphql.clone());
    let skill_id = escape_graphql_string(&args.skill_id);

    // Fields shared by `add` and `update` (everything except the immutable
    // skill_id / created_at). Reused so an `add` of an existing skill_id
    // updates it in place. An empty `tool_refs` is OMITTED rather than written
    // as `[]`: DefraDB cannot type an empty array literal and rejects it on a
    // later update.
    let mut fields = vec![
        gql_opt_string("agent_did", Some(&args.agent_did)),
        gql_opt_string("scope", Some(&args.scope)),
        gql_opt_string("name", args.name.as_deref()),
        gql_opt_string("description", args.description.as_deref()),
        gql_opt_string("instructions", instructions.as_deref()),
        gql_opt_string("display_name", args.display_name.as_deref()),
        format!("enabled: {}", args.enabled),
    ];
    if !args.tool_refs.is_empty() {
        fields.push(format!("tool_refs: {}", gql_string_list(&args.tool_refs)));
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
    let doc_id = extract_mutation_doc_id(&response, "Skill")?;
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
