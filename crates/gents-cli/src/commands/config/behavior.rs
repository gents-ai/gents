use anyhow::{Context, Result};
use gents::documents::Agent;
use gents::store::graphql::escape_graphql_string;
use gents::tools::builtin::self_config::{configure_agent, ConfigureAgentParams, StringUpdate};
use gents::tools::surface::presets;
use gents::Collection;
use serde_json::{json, Value};

use crate::cli::output_format::OutputFormat;
use crate::cli::*;
use crate::config_writes::{write_agent_document, ConfigAccess};
use crate::request_helpers::resolve_dual_id;
use crate::{graphql_rows, print_json, read_init_config, resolve_config_access, resolve_home_dir};

pub(super) async fn agent_set(args: AgentUpsertArgs) -> Result<()> {
    let endpoint = crate::resolve_graphql_endpoint(Some(&args.graphql), None)?;
    let access = ConfigAccess::Graphql(endpoint.clone());
    let binding = match args.agent_did.as_deref() {
        Some(agent_did) => {
            let owner = &args.node_did;
            let row = access
                .transact_readonly("cli.agent.identity", |txn| {
                    Box::pin(async move {
                        gents::store::config_client::read_desired_state_record_in_txn(
                            txn,
                            Collection::Agent,
                            owner,
                            agent_did,
                        )
                        .await
                    })
                })
                .await?
                .context("Agent DID does not exist; omit --agent-did to provision a new agent")?;
            serde_json::from_value::<Agent>(row.1)?.identity_binding()
        }
        None => gents_desktop_core::agent_identity::provision(&endpoint, &args.node_did).await?,
    };
    let agent_did = binding.agent_did.clone();
    let agent = Agent {
        agent_did: binding.agent_did,
        creation_nonce: binding.creation_nonce,
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
        "agent_did": agent_did,
        "node_did": args.node_did,
        "context_id": args.context_id,
        "inference_profile_id": args.inference_profile_id,
        "enabled": args.enabled,
    });
    print_json(&output)?;
    Ok(())
}

fn local_node_did(home: Option<&std::path::Path>) -> Result<String> {
    read_init_config(&resolve_home_dir(home))?
        .map(|config| config.node_did)
        .context("No runtime configured; pass the target runtime home.")
}

async fn submit_agent(graphql: &str, owner: &str, mut params: ConfigureAgentParams) -> Result<()> {
    let endpoint = crate::resolve_graphql_endpoint(Some(graphql), None)?;
    if matches!(params.action.as_str(), "create" | "clone") {
        params.identity_binding =
            Some(gents_desktop_core::agent_identity::provision(&endpoint, owner).await?);
    }
    let access = ConfigAccess::Graphql(endpoint);
    let result = configure_agent(&access, owner, &params, &Default::default()).await?;
    print_json(&serde_json::from_str::<Value>(&result)?)
}

fn optional_update(value: Option<String>) -> StringUpdate {
    value.map(StringUpdate::Set).unwrap_or_default()
}

pub(super) async fn agent_self_config_permission(
    args: AgentSelfConfigPermissionArgs,
    grant: bool,
) -> Result<()> {
    use gents::store::config_client::AgentSelfConfigPermission;

    let access = ConfigAccess::Graphql(crate::resolve_graphql_endpoint(Some(&args.graphql), None)?);
    let permission = if grant {
        AgentSelfConfigPermission::Grant
    } else {
        AgentSelfConfigPermission::Revoke
    };
    access
        .set_agent_self_config_permission(&args.node_did, &args.agent_did, permission)
        .await?;
    print_json(&json!({
        "node_did": args.node_did,
        "agent_did": args.agent_did,
        "self_config_granted": grant,
        "independent_document_grants_changed": false,
    }))
}

pub(super) async fn agent_create(args: AgentCreateArgs) -> Result<()> {
    let action = if args.clone_from.is_some() {
        "clone"
    } else {
        "create"
    };
    submit_agent(
        &args.graphql,
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
    let owner = local_node_did(args.home.as_deref())?;
    submit_agent(
        &args.graphql,
        &owner,
        ConfigureAgentParams {
            action: "clone".into(),
            clone_from: Some(args.source_agent_did),
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
    let owner = local_node_did(args.home.as_deref())?;
    submit_agent(
        &args.graphql,
        &owner,
        ConfigureAgentParams {
            action: "disable".into(),
            agent_did: Some(args.agent_did),
            ..Default::default()
        },
    )
    .await
}

/// Resolve one logical ID to one row, the same way the `config` tool does:
/// a bare ID prefers the local node's row, and an ambiguity names the
/// candidate rows instead of counting them.
async fn load_document(
    access: &ConfigAccess,
    collection: Collection,
    fields: &str,
    owner: Option<&str>,
    local_owner: Option<&str>,
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
            {collection_type}(filter: {{ {owner_filter}{unique_field}: {{ _eq: "{escaped_id}" }} }}) {{
                {fields} node_did
            }}
        }}"#,
        collection_type = collection.graphql_type(),
        owner_filter = filter,
        unique_field = collection.unique_field(),
    );
    let rows = graphql_rows(access, collection.graphql_type(), &query).await?;
    if owner.is_some() {
        anyhow::ensure!(
            rows.len() <= 1,
            "ambiguous {} {id:?}: {} live rows resolve this logical id",
            collection.graphql_type(),
            rows.len()
        );
        return Ok(rows.into_iter().next());
    }
    // A bare ID prefers the local node's row; a replicated peer row with the
    // same logical ID is not an ambiguity.
    let mut local = rows
        .iter()
        .filter(|row| {
            local_owner
                .is_some_and(|owner| row.get("node_did").and_then(Value::as_str) == Some(owner))
        })
        .cloned()
        .collect::<Vec<_>>();
    if local.len() == 1 {
        return Ok(local.pop());
    }
    anyhow::ensure!(
        rows.len() <= 1,
        "ambiguous {} {id:?}: {} live rows resolve this logical id: {}. Use the full <node_did>/<id> form to name one",
        collection.graphql_type(),
        rows.len(),
        rows.iter()
            .map(|row| format!(
                "{}/{}",
                row.get("node_did").and_then(Value::as_str).unwrap_or("?"),
                id
            ))
            .collect::<Vec<_>>()
            .join(", ")
    );
    Ok(rows.into_iter().next())
}

async fn classify_preset(access: &ConfigAccess, value: &Value) -> Result<Option<&'static str>> {
    use gents::documents::{merge_datastore_tool_surfaces, DatastoreToolSurfaceDocument, Tools};
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

const CANONICAL_AGENT_SHOW_FIELDS: &str = "agent_did creation_nonce node_did display_name description context_id inference_profile_id enabled tags created_at";

const CANONICAL_PROFILE_SHOW_FIELDS: &str = "profile_id display_name description backend_id model_name reasoning_effort service_tier context_window max_output_tokens sampling_id execution_id tags";

pub(super) async fn agent_show(args: ConfigShowArgs) -> Result<()> {
    let id = resolve_dual_id("agent", "--id", args.id.as_deref(), args.id_flag.as_deref())?;
    args.output
        .ensure_supported("config agent show", &[OutputFormat::Json])?;
    let (access, _) = resolve_config_access(args.home.as_deref(), args.graphql.as_deref())
        .await
        .context("resolving access for config agent show")?;
    // The local DID only decides which row a bare ID prefers. An explicit
    // --graphql endpoint can be read from a clean home, so a missing local DID
    // is not an error here: the exact-ID read below still resolves.
    let local_owner = crate::resolve_node_did(args.home.as_deref(), None).ok();

    let mut row = load_document(
        &access,
        Collection::Agent,
        CANONICAL_AGENT_SHOW_FIELDS,
        None,
        local_owner.as_deref(),
        &id,
    )
    .await?
    .ok_or_else(|| anyhow::anyhow!("not found: no Agent document with agent_did {id}"))?;

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
    use gents::NodeIdentity as _;

    use defra_node::Node as EmbeddedNode;
    use gents::documents::Tools;
    use serde_json::Value;

    use super::*;

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

    /// #3330 "Done when": slug resolution with a replicated peer row present.
    /// A bare ID prefers the local node's row; a genuine ambiguity names the
    /// candidate rows instead of counting them.
    #[tokio::test]
    async fn bare_id_prefers_the_local_row_and_ambiguity_names_candidates() -> Result<()> {
        use gents::store::config_client::{
            apply_desired_state_plan, DesiredStateApplyDocument, DesiredStateApplyPlan,
        };
        // Agent DIDs are derived per runtime, so the shared logical ID is a
        // profile label the local Node and a replicated peer both use.
        let client = crate::shared::test_support::ClientHome::new().await;
        let peer_dir = tempfile::tempdir()?;
        let peer = gents::KeyIdentity::load_or_create(peer_dir.path().join("peer.key"), None)?;
        let local = client.node_did().to_owned();
        let peer = peer.did().to_owned();
        let (local, peer) = (local.as_str(), peer.as_str());
        let documents = [local, peer]
            .into_iter()
            .flat_map(|owner| {
                [
                    (Collection::InferenceBackend, json!({"node_did":owner,"backend_id":"backend","name":"Test","provider_kind":"OpenAiCompatible","endpoint":"http://127.0.0.1:1/v1","auth":{"kind":"unauthenticated"}})),
                    (Collection::InferenceProfile, json!({"node_did":owner,"profile_id":"profile-1","backend_id":"backend","model_name":owner})),
                ]
            })
            .chain([(Collection::Node, json!({"node_did":peer}))])
            .map(|(collection, value)| DesiredStateApplyDocument {
                collection,
                add: value.clone(),
                update: value,
            })
            .collect();
        let plan = DesiredStateApplyPlan::new(documents)?;
        let access = client.access.clone();
        access
            .transact_write("test.slug.profile", |txn| {
                let plan = &plan;
                Box::pin(async move { apply_desired_state_plan(txn, plan).await })
            })
            .await?;

        // A replicated peer row with the same logical ID is not an ambiguity.
        let row = load_document(
            &access,
            Collection::InferenceProfile,
            "profile_id node_did model_name",
            None,
            Some(local),
            "profile-1",
        )
        .await?
        .expect("the local row resolves");
        assert_eq!(row["node_did"], local);
        assert_eq!(row["model_name"], local);

        // A clean home has no local DID to prefer. Two rows are then a real
        // ambiguity, and it must be reported as one — not as a missing-home
        // error, which is what `resolve_node_did(...)?` used to produce before
        // the read was ever attempted.
        let error = load_document(
            &access,
            Collection::InferenceProfile,
            "profile_id node_did model_name",
            None,
            None,
            "profile-1",
        )
        .await
        .expect_err("two rows with no local DID to prefer is ambiguous");
        let message = format!("{error:#}");
        assert!(message.contains("ambiguous"), "{message}");
        assert!(
            message.contains(&format!("{local}/profile-1"))
                && message.contains(&format!("{peer}/profile-1")),
            "the error must name both candidate rows: {message}"
        );

        let error = load_document(
            &access,
            Collection::InferenceProfile,
            "profile_id node_did model_name",
            None,
            Some("did:key:absent-owner"),
            "profile-1",
        )
        .await
        .expect_err("a home preferring an absent owner keeps both rows ambiguous");
        let message = format!("{error:#}");
        assert!(message.contains("ambiguous"), "{message}");
        assert!(
            message.contains(&format!("{peer}/profile-1")),
            "the error must name the candidate rows: {message}"
        );
        Ok(())
    }

    #[tokio::test]
    async fn agent_create_and_clone_materialize_canonical_chain() -> Result<()> {
        let client = crate::shared::test_support::ClientHome::new().await;
        let node = client.node().clone();
        let owner = client.node_did();
        use gents::store::config_client::{
            apply_desired_state_plan, DesiredStateApplyDocument, DesiredStateApplyPlan,
        };
        let documents = [
            (Collection::InferenceBackend,json!({"node_did":owner,"backend_id":"backend","name":"Test","provider_kind":"OpenAiCompatible","endpoint":"http://127.0.0.1:1/v1","auth":{"kind":"unauthenticated"}})),
            (Collection::InferenceProfile,json!({"node_did":owner,"profile_id":"profile-1","backend_id":"backend","model_name":"test"})),
        ].into_iter().map(|(collection,value)| DesiredStateApplyDocument{collection,add:value.clone(),update:value}).collect();
        let plan = DesiredStateApplyPlan::new(documents)?;
        let access = client.access.clone();
        access
            .transact_write("test.agent.profile", |txn| {
                let plan = &plan;
                Box::pin(async move { apply_desired_state_plan(txn, plan).await })
            })
            .await?;
        // Write-preset host tools run only under a User-published root.
        let workspace = tempfile::tempdir()?;
        let root = std::fs::canonicalize(workspace.path())?
            .to_string_lossy()
            .into_owned();
        gents::tools::surface::set_workspace_root_with_readers(&access, owner, &root, None, true, &[])
            .await?;
        let user = identity::Did::new(client.user.did())?;
        let provision = || {
            gents::store::identity::provision_agent_identity_authorized(
                &node,
                client.home.identity.as_ref(),
                &user,
            )
        };
        let doc = ConfigureAgentParams {
            identity_binding: Some(provision().await?),
            action: "create".into(),
            display_name: StringUpdate::Set("Canonical".into()),
            description: StringUpdate::Set("Canonical test agent".into()),
            system_prompt: StringUpdate::Set("Perform the requested work and verify it.".into()),
            preset: StringUpdate::Set("write".into()),
            root: StringUpdate::Set(root.clone()),
            profile_id: StringUpdate::Set("profile-1".into()),
            ..Default::default()
        };
        let outcome: Value = serde_json::from_str(
            &configure_agent(&access, owner, &doc, &Default::default()).await?,
        )?;
        assert_eq!(outcome["committed"], true);
        let agent_did = outcome["agent_did"].as_str().context("created agent ID")?;

        let agent = gents::load_agent(&node, agent_did)
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
            Some(gents::tools::surface::BashMode::Unrestricted),
        );

        let clone_doc = ConfigureAgentParams {
            identity_binding: Some(provision().await?),
            action: "clone".into(),
            clone_from: Some(agent_did.to_owned()),
            display_name: StringUpdate::Set("Cloned".into()),
            profile_id: StringUpdate::Set("profile-1".into()),
            ..Default::default()
        };
        let clone_outcome: Value = serde_json::from_str(
            &configure_agent(&access, owner, &clone_doc, &Default::default()).await?,
        )?;
        assert_eq!(clone_outcome["committed"], true);
        let cloned_id = clone_outcome["agent_did"]
            .as_str()
            .context("cloned agent ID")?;
        assert_ne!(cloned_id, agent_did);
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
        assert!(load_document(
            &access,
            Collection::Agent,
            "agent_did creation_nonce node_did",
            Some("did:key:foreign"),
            Some(owner),
            agent_did
        )
        .await?
        .is_none());
        assert!(load_document(
            &access,
            Collection::Agent,
            "agent_did creation_nonce node_did",
            Some(owner),
            Some(owner),
            agent_did
        )
        .await?
        .is_some());
        let preset = classify_preset(&access, &tools_row).await?;
        assert_eq!(preset, Some("write"));
        let source_tools = read_canonical(&node, "Tools", "tools_id", owner, &tools_id, "host")
            .await?
            .expect("source tools unchanged");
        assert_eq!(source_tools.get("tools_id"), Some(&json!(tools_id)));
        Ok(())
    }

    #[tokio::test]
    async fn raw_set_rejects_dangling_profile_reference() -> Result<()> {
        use gents::store::config_client::{
            apply_desired_state_plan, DesiredStateApplyDocument, DesiredStateApplyPlan,
        };
        let client = crate::shared::test_support::ClientHome::new().await;
        let binding = gents::store::identity::provision_agent_identity(client.home.identity.as_ref())?;
        let agent = json!({
            "creation_nonce": binding.creation_nonce,
            "agent_did": binding.agent_did,
            "node_did": client.node_did(),
            "inference_profile_id": "missing-profile",
            "enabled": true,
        });
        let plan = DesiredStateApplyPlan::new(vec![DesiredStateApplyDocument {
            collection: Collection::Agent,
            add: agent.clone(),
            update: agent,
        }])?;
        let error = client
            .access
            .transact_write("test.agent.dangling_profile", |txn| {
                let plan = &plan;
                Box::pin(async move { apply_desired_state_plan(txn, plan).await })
            })
            .await
            .unwrap_err();
        assert!(format!("{error:#}").contains("missing-profile"), "{error:#}");
        let response = client
            .access
            .execute("{ Agent {agent_did} AgentContext {context_id} Tools {tools_id} }")
            .await?;
        for name in ["Agent", "AgentContext", "Tools"] {
            assert_eq!(
                response["data"][name],
                serde_json::json!([]),
                "{name} must stay empty after a rejected write"
            );
        }
        Ok(())
    }
}
