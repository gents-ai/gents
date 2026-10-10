use std::sync::Arc;

use anyhow::{Context, Result};
use gents::document_config::Agent;
use gents::graphql::escape_graphql_string;
use gents::self_config::{configure_agent, ConfigureAgentParams, StringUpdate};
use gents::tool_surface::presets;
use gents::{default_agent_id_for_node, Collection, NodeIdentity};
use serde_json::{json, Value};

use crate::cli::output_format::OutputFormat;
use crate::cli::*;
use crate::config_writes::{write_agent_document, ConfigAccess};
use crate::request_helpers::resolve_dual_id;
use crate::{
    graphql_rows, load_initialized_home_identity, print_json, read_init_config,
    resolve_config_access, resolve_home_dir,
};

pub(super) async fn agent_set(args: AgentUpsertArgs) -> Result<()> {
    let agent_id = args
        .agent_id
        .clone()
        .unwrap_or_else(|| default_agent_id_for_node(&args.node_did));
    let access = ConfigAccess::Graphql(crate::resolve_graphql_endpoint(Some(&args.graphql), None)?);
    // Complete replacement clears omitted optionals. The shared writer
    // validates same-node references in the publication transaction.
    let agent = Agent {
        agent_id: agent_id.clone(),
        node_did: args.node_did.clone(),
        display_name: args.display_name.clone(),
        description: args.description.clone(),
        context_id: args.context_id.clone(),
        inference_profile_id: args.inference_profile_id.clone(),
        enabled: args.enabled,
        tags: args.tags.clone(),
        created_at: Some(chrono::Utc::now().to_rfc3339()),
    };
    let doc_id = write_agent_document(&access, &agent).await?;
    let output = json!({
        "doc_id": doc_id,
        "agent_id": agent_id,
        "node_did": args.node_did,
        "context_id": args.context_id,
        "inference_profile_id": args.inference_profile_id,
        "enabled": args.enabled,
    });
    print_json(&output)?;
    Ok(())
}

fn local_identity(home: Option<&std::path::Path>) -> Result<Arc<dyn NodeIdentity>> {
    let home_dir = resolve_home_dir(home);
    let config = read_init_config(&home_dir)?.with_context(|| {
        format!(
            "no init config found in {}; run `gents init` first",
            home_dir.display()
        )
    })?;
    load_initialized_home_identity(&home_dir, &config)
}

async fn submit_agent(
    graphql: &str,
    home: Option<&std::path::Path>,
    owner: &str,
    params: ConfigureAgentParams,
) -> Result<()> {
    let identity = local_identity(home)?;
    let access = ConfigAccess::Graphql(crate::resolve_graphql_endpoint(Some(graphql), home)?);
    let result = configure_agent(
        &access,
        owner,
        identity.as_ref(),
        &params,
        &Default::default(),
    )
    .await?;
    print_json(&serde_json::from_str::<Value>(&result)?)
}

fn optional_update(value: Option<String>) -> StringUpdate {
    value.map(StringUpdate::Set).unwrap_or_default()
}

pub(super) async fn agent_create(args: AgentCreateArgs) -> Result<()> {
    let action = if args.clone_from.is_some() {
        "clone"
    } else {
        "create"
    };
    submit_agent(
        &args.graphql,
        args.home.as_deref(),
        &args.node_did,
        ConfigureAgentParams {
            action: action.into(),
            clone_from: args.clone_from,
            display_name: StringUpdate::Set(args.display_name),
            description: optional_update(args.description),
            system_prompt: optional_update(args.system_prompt),
            root: optional_update(args.root),
            preset: optional_update(args.preset),
            profile_id: StringUpdate::Set(args.profile_id),
            ..Default::default()
        },
    )
    .await
}

pub(super) async fn agent_clone(args: AgentCloneArgs) -> Result<()> {
    let owner = local_identity(args.home.as_deref())?.did().to_owned();
    submit_agent(
        &args.graphql,
        args.home.as_deref(),
        &owner,
        ConfigureAgentParams {
            action: "clone".into(),
            clone_from: Some(args.source_agent_id),
            display_name: StringUpdate::Set(args.display_name),
            description: optional_update(args.description),
            system_prompt: optional_update(args.system_prompt),
            root: optional_update(args.root),
            profile_id: StringUpdate::Set(args.profile_id),
            ..Default::default()
        },
    )
    .await
}

pub(super) async fn agent_disable(args: AgentDisableArgs) -> Result<()> {
    let owner = local_identity(args.home.as_deref())?.did().to_owned();
    submit_agent(
        &args.graphql,
        args.home.as_deref(),
        &owner,
        ConfigureAgentParams {
            action: "disable".into(),
            agent_id: Some(args.agent_id),
            ..Default::default()
        },
    )
    .await
}

async fn load_document(
    access: &ConfigAccess,
    collection: Collection,
    fields: &str,
    owner: Option<&str>,
    id: &str,
) -> Result<Option<Value>> {
    let escaped_id = escape_graphql_string(id);
    let filter = match owner {
        Some(owner) => format!(
            r#"node_did: {{ _eq: "{}" }}, "#,
            escape_graphql_string(owner)
        ),
        None => String::new(),
    };
    let query = format!(
        r#"{{
            {collection_type}(filter: {{ {owner_filter}{unique_field}: {{ _eq: "{escaped_id}" }} }}, limit: 2) {{
                {fields}
            }}
        }}"#,
        collection_type = collection.graphql_type(),
        owner_filter = filter,
        unique_field = collection.unique_field(),
    );
    let rows = graphql_rows(access, collection.graphql_type(), &query).await?;
    // Exact scoped lookup: a logical id resolving to more than one live row
    // (unscoped lookup or duplicate scoped key) is ambiguous, not selectable.
    anyhow::ensure!(
        rows.len() <= 1,
        "ambiguous {} {id:?}: {} live rows resolve this logical id",
        collection.graphql_type(),
        rows.len()
    );
    Ok(rows.into_iter().next())
}

async fn classify_preset(access: &ConfigAccess, value: &Value) -> Result<Option<&'static str>> {
    use gents::document_config::{
        merge_datastore_tool_surfaces, DatastoreToolSurfaceDocument, Tools,
    };
    let tools: Tools = serde_json::from_value(value.clone())?;
    tools.validate()?;
    let mut surfaces = Vec::<DatastoreToolSurfaceDocument>::new();
    for id in tools
        .datastore
        .as_ref()
        .and_then(|d| d.datastore_tool_surface_ids.as_ref())
        .into_iter()
        .flatten()
    {
        let row = load_document(
            access,
            Collection::DatastoreToolSurface,
            "surface_id node_did display_name enabled entries created_at tags",
            Some(&tools.node_did),
            id,
        )
        .await?
        .with_context(|| format!("missing selected datastore surface {id:?}"))?;
        surfaces.push(serde_json::from_value(row)?);
    }
    let merged = merge_datastore_tool_surfaces(&tools, &surfaces)?;
    presets::classify_tools(&tools, &merged)
}

const CANONICAL_AGENT_SHOW_FIELDS: &str = "agent_id node_did display_name description context_id inference_profile_id enabled tags created_at";

const CANONICAL_PROFILE_SHOW_FIELDS: &str = "profile_id display_name description backend_id model_name reasoning_effort context_window max_output_tokens sampling_id execution_id tags";

pub(super) async fn agent_show(args: ConfigShowArgs) -> Result<()> {
    let id = resolve_dual_id("agent", "--id", args.id.as_deref(), args.id_flag.as_deref())?;
    args.output
        .ensure_supported("config agent show", &[OutputFormat::Json])?;
    let (access, _) = resolve_config_access(args.home.as_deref(), args.graphql.as_deref())
        .await
        .context("resolving access for config agent show")?;

    let mut row = load_document(
        &access,
        Collection::Agent,
        CANONICAL_AGENT_SHOW_FIELDS,
        None,
        &id,
    )
    .await?
    .ok_or_else(|| anyhow::anyhow!("not found: no Agent document with agent_id {id}"))?;

    let node_did = row
        .get("node_did")
        .and_then(Value::as_str)
        .context("Agent is missing its owner DID")?
        .to_string();
    let context_id = row
        .get("context_id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned);
    let profile_id = row
        .get("inference_profile_id")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(ToOwned::to_owned);

    let context = match context_id.as_deref() {
        Some(context_id) => {
            load_document(
                &access,
                Collection::AgentContext,
                "context_id node_did display_name description system_prompt tools_id compaction_id skill_ids tags",
                Some(&node_did),
                context_id,
            )
            .await?
        }
        None => None,
    };
    let tools = match context
        .as_ref()
        .and_then(|context| context.get("tools_id"))
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
    {
        Some(tools_id) => {
            load_document(
                &access,
                Collection::Tools,
                "tools_id node_did display_name host remote agents built_ins datastore integrations self_config tags",
                Some(&node_did),
                tools_id,
            )
            .await?
        }
        None => None,
    };
    let profile = match profile_id.as_deref() {
        Some(profile_id) => {
            load_document(
                &access,
                Collection::InferenceProfile,
                CANONICAL_PROFILE_SHOW_FIELDS,
                Some(&node_did),
                profile_id,
            )
            .await?
        }
        None => None,
    };
    let preset_name = match tools.as_ref() {
        Some(tools) => Some(classify_preset(&access, tools).await?.unwrap_or("custom")),
        None => None,
    };
    let root = tools
        .as_ref()
        .and_then(|tools| tools.get("host"))
        .and_then(|host| host.get("root"))
        .cloned()
        .unwrap_or(Value::Null);

    let resolved = json!({
        "root": root,
        "preset_name": preset_name,
        "profile": profile,
        "context": context,
        "tools": tools,
    });
    if let Value::Object(ref mut map) = row {
        map.insert("resolved".to_string(), resolved);
    }
    print_json(&row)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use defra_node::EmbeddedNode;
    use gents::document_config::Tools;
    use gents::{ensure_runtime_schemas, upsert_agent};
    use serde_json::Value;

    use super::*;

    /// Read one canonical document by its scoped logical key through the raw
    /// GraphQL door. Tests only; production code reads through the shared
    /// scoped readers.
    async fn read_canonical(
        node: &EmbeddedNode,
        collection: &str,
        id_field: &str,
        owner: &str,
        id: &str,
        extra_fields: &str,
    ) -> Result<Option<Value>> {
        let query = format!(
            r#"{{ {collection}(filter: {{ node_did: {{ _eq: "{}" }}, {id_field}: {{ _eq: "{}" }} }}, limit: 2) {{ {id_field} node_did {extra_fields} }} }}"#,
            escape_graphql_string(owner),
            escape_graphql_string(id),
        );
        let response = node.execute(&query).await;
        anyhow::ensure!(!response.has_errors(), "query {collection} failed");
        let rows = response
            .data
            .as_ref()
            .and_then(|data| data.get(collection))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        anyhow::ensure!(
            rows.len() <= 1,
            "ambiguous {collection} {id:?} under {owner:?}"
        );
        Ok(rows.into_iter().next())
    }

    #[tokio::test]
    async fn agent_create_and_clone_materialize_canonical_chain() -> Result<()> {
        let tempdir = tempfile::tempdir()?;
        let node = Arc::new(
            EmbeddedNode::builder()
                .data_path(tempdir.path().join("data"))
                .build()
                .await
                .expect("embedded node boots"),
        );
        ensure_runtime_schemas(&node).await?;
        let identity = gents::identity::KeyIdentity::load_or_create(
            tempdir.path().join("identity.key"),
            None,
        )?;
        let owner = identity.did();
        let actor = identity::Did::new(owner.to_string()).expect("valid test ACP actor");
        gents::ensure_node(&node, owner).await?;
        use gents::config_client::{
            apply_desired_state_plan, DesiredStateApplyDocument, DesiredStateApplyPlan,
        };
        let documents = [
            (Collection::InferenceBackend,json!({"node_did":owner,"backend_id":"backend","name":"Test","provider_kind":"OpenAiCompatible","endpoint":"http://127.0.0.1:1/v1","auth":{"kind":"unauthenticated"}})),
            (Collection::InferenceProfile,json!({"node_did":owner,"profile_id":"profile-1","backend_id":"backend","model_name":"test"})),
        ].into_iter().map(|(collection,value)| DesiredStateApplyDocument{collection,add:value.clone(),update:value}).collect();
        let plan = DesiredStateApplyPlan::new(documents)?;
        ConfigAccess::transact_local(&node, Some(actor.clone()), "test.agent.profile", |txn| {
            let plan = &plan;
            Box::pin(async move { apply_desired_state_plan(txn, plan).await })
        })
        .await?;

        let access = ConfigAccess::Local(node.clone());
        let doc = ConfigureAgentParams {
            action: "create".into(),
            display_name: StringUpdate::Set("Canonical".into()),
            description: StringUpdate::Set("Canonical test agent".into()),
            system_prompt: StringUpdate::Set("Perform the requested work and verify it.".into()),
            preset: StringUpdate::Set("write".into()),
            profile_id: StringUpdate::Set("profile-1".into()),
            ..Default::default()
        };
        let outcome: Value = serde_json::from_str(
            &configure_agent(&access, owner, &identity, &doc, &Default::default()).await?,
        )?;
        assert_eq!(outcome["committed"], true);
        let agent_id = outcome["agent_id"].as_str().context("created agent ID")?;

        let agent = gents::load_agent(&node, agent_id)
            .await?
            .expect("created agent exists");
        assert_eq!(agent.inference_profile_id, "profile-1");
        let context_id = agent.context_id.clone().expect("create mints a context");
        let context_row = read_canonical(
            &node,
            "AgentContext",
            "context_id",
            owner,
            &context_id,
            "tools_id",
        )
        .await?
        .expect("created context exists");
        let tools_id = context_row
            .get("tools_id")
            .and_then(Value::as_str)
            .expect("write preset mints tools")
            .to_string();
        let tools_row = read_canonical(&node, "Tools", "tools_id", owner, &tools_id, "host")
            .await?
            .expect("created tools exist");
        let tools: Tools = serde_json::from_value(tools_row.clone())?;
        assert_eq!(
            tools
                .host
                .as_ref()
                .and_then(|host| host.bash.as_ref())
                .map(|bash| bash.mode),
            Some(gents::tool_surface::BashMode::Unrestricted),
        );

        let clone_doc = ConfigureAgentParams {
            action: "clone".into(),
            clone_from: Some(agent_id.to_owned()),
            display_name: StringUpdate::Set("Cloned".into()),
            profile_id: StringUpdate::Set("profile-1".into()),
            ..Default::default()
        };
        let clone_outcome: Value = serde_json::from_str(
            &configure_agent(&access, owner, &identity, &clone_doc, &Default::default()).await?,
        )?;
        assert_eq!(clone_outcome["committed"], true);
        let cloned_id = clone_outcome["agent_id"]
            .as_str()
            .context("cloned agent ID")?;
        assert_ne!(cloned_id, agent_id);
        let cloned = gents::load_agent(&node, cloned_id)
            .await?
            .expect("cloned agent exists");
        let cloned_context_id = cloned
            .context_id
            .clone()
            .expect("clone mints a private context");
        assert_ne!(cloned_context_id, context_id);
        let cloned_context_row = read_canonical(
            &node,
            "AgentContext",
            "context_id",
            owner,
            &cloned_context_id,
            "tools_id",
        )
        .await?
        .expect("cloned context exists");
        let cloned_tools_id = cloned_context_row
            .get("tools_id")
            .and_then(Value::as_str)
            .expect("cloned tools")
            .to_string();
        assert_ne!(
            cloned_tools_id, tools_id,
            "clone must not mutate the shared source tools"
        );
        let access = ConfigAccess::Local(node.clone());
        assert!(load_document(
            &access,
            Collection::Agent,
            "agent_id node_did",
            Some("did:key:foreign"),
            agent_id
        )
        .await?
        .is_none());
        assert!(load_document(
            &access,
            Collection::Agent,
            "agent_id node_did",
            Some(owner),
            agent_id
        )
        .await?
        .is_some());
        let preset = classify_preset(&access, &tools_row).await?;
        assert_eq!(preset, Some("write"));
        // The source chain is untouched by the clone.
        let source_tools = read_canonical(&node, "Tools", "tools_id", owner, &tools_id, "host")
            .await?
            .expect("source tools unchanged");
        assert_eq!(source_tools.get("tools_id"), Some(&json!(tools_id)));
        Ok(())
    }

    /// The raw `agent set` door is a complete canonical replacement through
    /// the shared writer: omitted optionals clear and a dangling
    /// context/profile reference fails the write transaction.
    #[tokio::test]
    async fn raw_set_rejects_dangling_profile_reference() -> Result<()> {
        let tempdir = tempfile::tempdir()?;
        let node = Arc::new(
            EmbeddedNode::builder()
                .data_path(tempdir.path().join("data"))
                .build()
                .await
                .expect("embedded node boots"),
        );
        ensure_runtime_schemas(&node).await?;
        gents::ensure_node(&node, "did:key:set-owner").await?;
        let agent = Agent {
            agent_id: "b1".to_string(),
            node_did: "did:key:set-owner".to_string(),
            display_name: None,
            description: None,
            context_id: None,
            inference_profile_id: "missing-profile".to_string(),
            enabled: true,
            tags: Vec::new(),
            created_at: None,
        };
        // The shared writer agent_set drives owns the reference validation;
        // a dangling profile rejects inside its transaction.
        assert!(
            format!("{:#}", upsert_agent(&node, &agent).await.unwrap_err())
                .contains("missing-profile")
        );
        // Nothing was published: the canonical chain stays empty.
        let response = node
            .execute("{ Agent {agent_id} AgentContext {context_id} Tools {tools_id} }")
            .await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        for name in ["Agent", "AgentContext", "Tools"] {
            assert_eq!(
                response.data.as_ref().unwrap()[name],
                serde_json::json!([]),
                "{name} must stay empty after a rejected write"
            );
        }
        Ok(())
    }
}
