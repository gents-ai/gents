use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{bail, Context, Result};
use serde_json::Value;

use super::load::scenario_config;
use super::manifest::ScenarioManifest;
use super::prepare;
use gents::store::graphql::validate_collection_identifier;

pub(super) fn validate_task_goal_declarations(manifest: &ScenarioManifest) -> Result<()> {
    let config = scenario_config(manifest)?;
    for task in &config.tasks {
        let Some(objective) = task.goal_objective_template.as_deref() else {
            if task.goal_token_budget.is_some() {
                bail!(
                    "Task {} sets goal_token_budget without goal_objective_template",
                    task.task_id
                );
            }
            continue;
        };
        if objective.trim().is_empty() {
            bail!(
                "Task {} goal_objective_template must be non-empty",
                task.task_id
            );
        }
        if let Some(budget) = task.goal_token_budget {
            if budget <= 0 {
                bail!("Task {} goal_token_budget must be positive", task.task_id);
            }
        }

        let agent = config
            .agents
            .iter()
            .find(|agent| agent.agent_id == task.agent_id)
            .with_context(|| format!("Task {} references a missing agent", task.task_id))?;
        let context = agent
            .context_id
            .as_deref()
            .and_then(|id| {
                config
                    .contexts
                    .iter()
                    .find(|context| context.context_id == id)
            })
            .with_context(|| format!("Task {} agent has no context", task.task_id))?;
        let tools = context
            .tools_id
            .as_deref()
            .and_then(|id| config.tools.iter().find(|tools| tools.tools_id == id))
            .with_context(|| format!("Task {} context has no tools", task.task_id))?;
        let goal_tools_enabled = tools
            .built_ins
            .as_ref()
            .and_then(|built_ins| built_ins.enable_goal_tools)
            .unwrap_or(false);
        if !goal_tools_enabled {
            bail!(
                "Task {} declares a durable goal but Tools {} does not enable goal tools",
                task.task_id,
                tools.tools_id
            );
        }
        if tools
            .built_ins
            .as_ref()
            .and_then(|built_ins| built_ins.enable_goal_creation)
            .unwrap_or(false)
        {
            bail!(
                "Task {} declares a controller-provisioned durable goal but Tools {} enables model goal creation",
                task.task_id,
                tools.tools_id
            );
        }
    }
    Ok(())
}

pub(super) fn validate_prompt_tool_contracts(
    pack: &Path,
    manifest: &ScenarioManifest,
) -> Result<()> {
    if manifest.expect.prompt_tool_contracts.is_empty() {
        return Ok(());
    }
    let schemas = std::fs::read_dir(pack.join("schemas"))
        .with_context(|| format!("reading {}", pack.join("schemas").display()))?
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "graphql"))
        .map(|entry| std::fs::read_to_string(entry.path()))
        .collect::<std::io::Result<Vec<_>>>()?
        .join("\n");

    let config = scenario_config(manifest)?;
    for contract in &manifest.expect.prompt_tool_contracts {
        let task = config
            .tasks
            .iter()
            .find(|task| task.task_id == contract.task_id)
            .with_context(|| format!("missing Task {}", contract.task_id))?;
        let agent_id = task.agent_id.as_str();
        let agent = config
            .agents
            .iter()
            .find(|agent| agent.agent_id == agent_id)
            .with_context(|| format!("missing Agent {agent_id}"))?;
        let context = agent
            .context_id
            .as_deref()
            .and_then(|id| {
                config
                    .contexts
                    .iter()
                    .find(|context| context.context_id == id)
            })
            .with_context(|| format!("Agent {agent_id} has no context"))?;
        let tools = context
            .tools_id
            .as_deref()
            .and_then(|id| config.tools.iter().find(|tools| tools.tools_id == id))
            .with_context(|| format!("AgentContext {} has no tools", context.context_id))?;
        let task_prompt = task.prompt_template.as_str();
        let system_prompt = context.system_prompt.as_deref().unwrap_or_default();

        let mut advertised = std::collections::BTreeSet::new();
        let datastore = tools.datastore.as_ref();
        let query_collections = datastore
            .and_then(|datastore| datastore.defra_query_collections.as_deref())
            .unwrap_or_default()
            .iter()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>();
        if datastore
            .and_then(|datastore| datastore.enable_defra_query)
            .unwrap_or(false)
        {
            advertised.insert(gents::defra_query::DEFRA_QUERY_TOOL_NAME.to_string());
        }
        if datastore
            .and_then(|settings| settings.write_collections.as_ref())
            .is_some_and(|collections| !collections.is_empty())
        {
            advertised.insert(gents::plugin::application_write::WRITE_TOOL_NAME.to_string());
        }
        for surface_id in datastore
            .and_then(|datastore| datastore.datastore_tool_surface_ids.as_deref())
            .unwrap_or_default()
        {
            let surface = config
                .datastore_tool_surfaces
                .iter()
                .find(|surface| surface.surface_id == *surface_id)
                .with_context(|| format!("missing DatastoreToolSurface {surface_id}"))?;
            for entry in surface.entries.as_deref().unwrap_or_default() {
                let entry = serde_json::to_value(entry)?;
                let tool_name = entry
                    .get("tool_name")
                    .and_then(Value::as_str)
                    .context("datastore tool entry has no tool_name")?;
                let collection = entry
                    .get("collection")
                    .and_then(Value::as_str)
                    .context("datastore tool entry has no collection")?;
                if !schemas.contains(&format!("type {collection} {{")) {
                    bail!(
                        "tool {tool_name} targets collection {collection}, but the pack has no matching schema"
                    );
                }
                let type_start = schemas
                    .find(&format!("type {collection} {{"))
                    .context("schema type disappeared during tool validation")?;
                let type_body = schemas[type_start..]
                    .split_once('}')
                    .map(|(body, _)| body)
                    .context("schema type has no closing brace")?;
                let mut field_names = Vec::new();
                for field in entry
                    .get("fields")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if let Some(name) = field.as_str() {
                        field_names.push(name.to_string());
                    } else if let Some(name) = field.get("name").and_then(Value::as_str) {
                        field_names.push(name.to_string());
                    } else {
                        bail!("datastore tool field has no name");
                    }
                }
                for field in entry
                    .get("filter_fields")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    let field_name = field
                        .get("name")
                        .and_then(Value::as_str)
                        .context("datastore tool filter field has no name")?;
                    field_names.push(field_name.to_string());
                }
                for field_name in field_names {
                    if !type_body.lines().any(|line| {
                        line.trim_start()
                            .strip_prefix(&field_name)
                            .is_some_and(|rest| rest.trim_start().starts_with(':'))
                    }) {
                        bail!(
                            "tool {tool_name} exposes field {field_name}, but {collection} has no matching schema field"
                        );
                    }
                }
                advertised.insert(tool_name.to_string());
            }
        }

        let combined_prompt = format!("{system_prompt}\n{task_prompt}");
        for tool_name in &contract.required_tool_names {
            if !advertised.contains(tool_name) {
                bail!(
                    "task {} requires tool {tool_name}, but agent {agent_id} does not advertise it",
                    contract.task_id
                );
            }
            if !combined_prompt.contains(&format!("`{tool_name}`")) {
                bail!(
                    "task {} exposes tool {tool_name}, but its prompts do not name it exactly as `{tool_name}`",
                    contract.task_id
                );
            }
        }
        for collection in &contract.required_query_collections {
            if !query_collections.contains(collection.as_str()) {
                bail!(
                    "task {} asks defra_query for {collection}, but agent {agent_id} cannot query it",
                    contract.task_id
                );
            }
            if !combined_prompt.contains(&format!("`{collection}`")) {
                bail!(
                    "task {} can query {collection}, but its prompts do not name that collection exactly",
                    contract.task_id
                );
            }
        }
    }
    Ok(())
}

pub(super) fn validate_manifest(manifest: &ScenarioManifest) -> Result<()> {
    let mut allowed = BTreeSet::new();
    for folder in &manifest.allowed_folders {
        let path = folder.path.trim();
        if path.is_empty() {
            bail!("allowed_folders entries need a path");
        }
        if !allowed.insert(path) {
            bail!("allowed_folders names {path} twice");
        }
    }
    if !allowed.is_empty()
        && manifest
            .init
            .tool_root
            .as_deref()
            .is_none_or(|root| root.trim().is_empty())
    {
        bail!("allowed_folders needs init.tool_root: every allowed folder stays inside it");
    }
    if !manifest.expect.source_edges.is_empty() && !manifest.expect.signed_provenance {
        bail!("expect.source_edges requires expect.signed_provenance=true");
    }
    validate_tool_package(&manifest.init.tool_package)?;
    let mut dependency_packages = BTreeSet::new();
    for package in &manifest.dependencies {
        let package = package.trim();
        if package.is_empty() {
            bail!("manifest dependencies entries must not be empty");
        }
        if !dependency_packages.insert(package) {
            bail!("manifest dependencies contain duplicate {package}");
        }
    }
    let mut result_collections = BTreeSet::new();
    for result in &manifest.expect.result_documents {
        validate_collection_identifier(&result.collection)?;
        gents::store::graphql::validate_graphql_name(&result.correlation_field)?;
        if result.fields.is_empty() {
            bail!(
                "expect.result_documents entry for {} must select at least one field",
                result.collection
            );
        }
        for field in &result.fields {
            gents::store::graphql::validate_graphql_name(field)?;
        }
        if !result_collections.insert(&result.collection) {
            bail!(
                "expect.result_documents contains duplicate collection {}",
                result.collection
            );
        }
    }
    for (trigger_id, count) in &manifest.expect.trigger_request_counts {
        if !manifest.expect.trigger_ids.contains(trigger_id) {
            bail!("expect.trigger_request_counts names unknown trigger {trigger_id}");
        }
        if *count == 0 {
            bail!("expect.trigger_request_counts[{trigger_id}] must be greater than zero");
        }
    }
    for (trigger_id, source) in &manifest.expect.trigger_request_count_sources {
        if !manifest.expect.trigger_ids.contains(trigger_id) {
            bail!("expect.trigger_request_count_sources names unknown trigger {trigger_id}");
        }
        if manifest
            .expect
            .trigger_request_counts
            .contains_key(trigger_id)
        {
            bail!("trigger {trigger_id} has both a fixed request count and a request count source");
        }
        validate_collection_identifier(&source.collection)?;
        gents::store::graphql::validate_graphql_name(&source.correlation_field)?;
        match (
            source
                .match_field
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty()),
            source
                .match_value
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty()),
        ) {
            (Some(match_field), Some(_)) => {
                gents::store::graphql::validate_graphql_name(match_field)?;
            }
            (None, None) => {
                gents::store::graphql::validate_graphql_name(&source.expected_count_field)?;
            }
            _ => {
                bail!(
                    "expect.trigger_request_count_sources[{trigger_id}] needs both match_field and match_value"
                );
            }
        }
    }
    let mut sequenced_triggers = BTreeSet::new();
    for expected in &manifest.expect.stage_tool_sequences {
        if !manifest.expect.trigger_ids.contains(&expected.trigger_id) {
            bail!(
                "expect.stage_tool_sequences names unknown trigger {}",
                expected.trigger_id
            );
        }
        if !sequenced_triggers.insert(&expected.trigger_id) {
            bail!(
                "expect.stage_tool_sequences contains duplicate trigger {}",
                expected.trigger_id
            );
        }
        if expected.boundary_tool_name.trim().is_empty() {
            bail!("expect.stage_tool_sequences boundary_tool_name must not be empty");
        }
        if expected.exact_boundary_calls == 0 {
            bail!("expect.stage_tool_sequences exact_boundary_calls must be greater than zero");
        }
        if expected.max_calls_per_message_before_boundary == 0 {
            bail!(
                "expect.stage_tool_sequences max_calls_per_message_before_boundary must be greater than zero"
            );
        }
        if !expected
            .allowed_at_or_after_boundary
            .iter()
            .any(|name| name == &expected.boundary_tool_name)
        {
            bail!(
                "expect.stage_tool_sequences for {} must allow its boundary tool {}",
                expected.trigger_id,
                expected.boundary_tool_name
            );
        }
    }
    if let Some(expected) = &manifest.expect.workspace_receipt_paths {
        validate_collection_identifier(&expected.work_unit_collection)?;
        for field in [
            &expected.correlation_field,
            &expected.work_unit_id_field,
            &expected.owned_paths_field,
        ] {
            gents::store::graphql::validate_graphql_name(field)?;
        }
    }
    if let Some(fan_in) = &manifest.expect.fan_in {
        if fan_in.min_expected_count == Some(0) || fan_in.max_expected_count == Some(0) {
            bail!("expect.fan_in count bounds must be greater than zero");
        }
        if matches!(
            (fan_in.min_expected_count, fan_in.max_expected_count),
            (Some(minimum), Some(maximum)) if minimum > maximum
        ) {
            bail!("expect.fan_in minimum count cannot exceed its maximum count");
        }
    }
    if let Some(expected) = &manifest.expect.background_completion {
        if expected.min_completed_caused_requests == 0
            || expected.min_completed_wakes == 0
            || expected.min_acknowledged_notifications == 0
        {
            bail!("expect.background_completion minimums must all be greater than zero");
        }
    }
    if tool_package_needs_root(&manifest.init.tool_package)
        && manifest
            .init
            .tool_root
            .as_deref()
            .map(str::trim)
            .unwrap_or("")
            .is_empty()
    {
        bail!(
            "init.tool_package={} requires init.tool_root (a read-only/write ceiling needs a workspace root)",
            manifest.init.tool_package
        );
    }
    prepare::validate_prepare_steps(manifest)?;
    Ok(())
}

fn validate_tool_package(package: &str) -> Result<()> {
    match package {
        "minimal" | "introspection" | "readonly" | "write" | "yolo" => Ok(()),
        other => bail!("unknown init.tool_package {other}"),
    }
}

pub(super) fn tool_package_needs_root(package: &str) -> bool {
    matches!(package, "readonly" | "write" | "yolo")
}
