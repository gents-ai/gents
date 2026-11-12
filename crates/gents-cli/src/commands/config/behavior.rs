use anyhow::{Context, Result};
use gents_documents::Agent;
use gents_protocol::agent_identity::AgentIdentityBinding;
use gents_protocol::operations::agent::{
    AgentCreateInput, AgentCreateOperation, AgentOperationError,
};
use gents_protocol::operations::NextCall;
use gents_protocol::operations::permission::{
    AgentPermission, AgentPermissionInput, AgentPermissionReceipt, PermissionChange,
};
use gents_protocol::operations::StringUpdate;
use gents_store::collection::Collection;
use gents_store::graphql::escape_graphql_string;
use gents_tools::surface::presets;
use serde_json::{json, Value};

use crate::cli::output_format::OutputFormat;
use crate::cli::*;
use crate::config_writes::ConfigAccess;
use crate::request_helpers::resolve_dual_id;
use crate::{graphql_rows, print_json, resolve_config_access};
use gents_service::home_state::read_init_config;
use gents_service::home_state::resolve_home_dir;

pub(super) async fn agent_set(args: AgentUpsertArgs) -> Result<()> {
    let endpoint = gents_service::home_state::resolve_graphql_endpoint(Some(&args.graphql), None)?;
    let access = ConfigAccess::Graphql(endpoint.clone());
    let owner = &args.node_did;
    let (agent, binding) = match args.agent_did.as_deref() {
        Some(agent_did) => {
            let row = access
                .transact_readonly("cli.agent.identity", |txn| {
                    Box::pin(async move {
                        gents_documents::read_desired_state_record_in_txn(
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
            let mut agent = serde_json::from_value::<Agent>(row.1)?;
            if args.display_name.is_some() {
                agent.display_name = args.display_name.clone();
            }
            if args.description.is_some() {
                agent.description = args.description.clone();
            }
            if args.context_id.is_some() {
                agent.context_id = args.context_id.clone();
            }
            agent.inference_profile_id = args.inference_profile_id.clone();
            agent.enabled = args.enabled;
            if !args.tags.is_empty() {
                agent.tags = args.tags.clone();
            }
            (agent, None)
        }
        None => {
            let binding =
                gents_desktop_core::agent_identity::provision(&endpoint, &args.node_did).await?;
            let agent = Agent {
                agent_did: binding.agent_did.clone(),
                creation_nonce: binding.creation_nonce.clone(),
                node_did: args.node_did.clone(),
                display_name: args.display_name.clone(),
                description: args.description.clone(),
                context_id: args.context_id.clone(),
                inference_profile_id: args.inference_profile_id.clone(),
                enabled: args.enabled,
                tags: args.tags.clone(),
                created_at: Some(chrono::Utc::now().to_rfc3339()),
            };
            (agent, Some(binding))
        }
    };
    let creating = binding.is_some();
    let receipt =
        gents::operations::agent::put(&access, owner, &agent, binding, false, &Default::default())
            .await
            .map_err(|error| {
                if creating {
                    AgentFailure::create(error, AgentCreateOperation::Create)
                } else {
                    AgentFailure::edit(error)
                }
            })?;
    print_json(&json!({
        "operation": receipt.operation,
        "agent_did": receipt.agent_did,
        "node_did": args.node_did,
        "context_id": receipt.agent.context_id,
        "inference_profile_id": receipt.agent.inference_profile_id,
        "enabled": receipt.agent.enabled,
    }))
}

fn local_node_did(home: Option<&std::path::Path>) -> Result<String> {
    read_init_config(&resolve_home_dir(home))?
        .map(|config| config.node_did)
        .context("No runtime configured; pass the target runtime home.")
}

/// A refused agent operation, shown with the command that corrects it. Its
/// causes stay those of the refusal.
#[derive(Debug)]
pub(crate) struct AgentFailure {
    error: AgentOperationError,
    next: NextCall,
}

impl AgentFailure {
    /// A failed create or clone.
    pub(crate) fn create(error: AgentOperationError, operation: AgentCreateOperation) -> Self {
        let next = error.recovery(operation);
        Self { error, next }
    }

    /// A failed edit, disable or default choice on an existing Agent.
    pub(crate) fn edit(error: AgentOperationError) -> Self {
        let next = error.edit_recovery();
        Self { error, next }
    }
}

impl std::fmt::Display for AgentFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let next = crate::next_call::command(self.next, None);
        write!(formatter, "{}\nNext: {next}", self.error)?;
        if matches!(self.error, AgentOperationError::CatalogNotHeld) {
            write!(
                formatter,
                "\nA Node owner or administrator grants the catalog: gents config agent grant-catalog --graphql <GRAPHQL_URL> --node-did <NODE_DID> <AGENT_DID>"
            )?;
        }
        Ok(())
    }
}

impl std::error::Error for AgentFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.error.source()
    }
}

async fn create_agent(
    access: &ConfigAccess,
    owner: &str,
    identity_binding: AgentIdentityBinding,
    input: &AgentCreateInput,
) -> Result<Value> {
    let receipt = gents::operations::agent::create(
        access,
        owner,
        identity_binding,
        input,
        &Default::default(),
    )
    .await
    .map_err(|error| AgentFailure::create(error, input.operation))?;
    Ok(serde_json::to_value(receipt)?)
}

async fn submit_create(graphql: &str, owner: &str, input: AgentCreateInput) -> Result<()> {
    let endpoint = gents_service::home_state::resolve_graphql_endpoint(Some(graphql), None)?;
    let identity_binding = gents_desktop_core::agent_identity::provision(&endpoint, owner).await?;
    let access = ConfigAccess::Graphql(endpoint);
    print_json(&create_agent(&access, owner, identity_binding, &input).await?)
}

fn optional_update(value: Option<String>) -> StringUpdate {
    value.map(StringUpdate::Set).unwrap_or_default()
}

async fn change_permission(
    graphql: &str,
    node_did: String,
    agent_did: String,
    permission: AgentPermission,
    grant: bool,
) -> Result<AgentPermissionReceipt> {
    let access = ConfigAccess::Graphql(gents_service::home_state::resolve_graphql_endpoint(
        Some(graphql),
        None,
    )?);
    let input = AgentPermissionInput {
        node_did,
        agent_did,
        permission,
        change: if grant {
            PermissionChange::Grant
        } else {
            PermissionChange::Revoke
        },
    };
    Ok(
        gents_documents::agent_permission::change_agent_permission(&access, &input)
            .await
            .map_err(|error| {
                let next = error.recovery();
                crate::commands::refusal::Refusal::new(error, next)
            })?,
    )
}

pub(super) async fn agent_self_config_permission(
    args: AgentSelfConfigPermissionArgs,
    grant: bool,
) -> Result<()> {
    let receipt = change_permission(
        &args.graphql,
        args.node_did,
        args.agent_did,
        AgentPermission::SelfConfig,
        grant,
    )
    .await?;
    print_json(&json!({
        "node_did": receipt.node_did,
        "agent_did": receipt.agent_did,
        "self_config_granted": receipt.granted,
        "independent_document_grants_changed": false,
    }))
}

pub(super) async fn agent_catalog_permission(
    args: AgentCatalogPermissionArgs,
    grant: bool,
) -> Result<()> {
    let receipt = change_permission(
        &args.graphql,
        args.node_did,
        args.agent_did,
        AgentPermission::Catalog,
        grant,
    )
    .await?;
    print_json(&json!({
        "node_did": receipt.node_did,
        "agent_did": receipt.agent_did,
        "catalog_granted": receipt.granted,
    }))
}

pub(super) async fn agent_create(args: AgentCreateArgs) -> Result<()> {
    let operation = if args.clone_from.is_some() {
        AgentCreateOperation::Clone
    } else {
        AgentCreateOperation::Create
    };
    submit_create(
        &args.graphql,
        &args.node_did,
        AgentCreateInput {
            operation,
            clone_from: args.clone_from,
            display_name: args.display_name,
            description: optional_update(args.description),
            system_prompt: optional_update(args.system_prompt),
            profile_id: args.profile_id,
            preset: args.preset,
            root: optional_update(args.root),
            make_default: false,
        },
    )
    .await
}

pub(super) async fn agent_clone(args: AgentCloneArgs) -> Result<()> {
    let owner = local_node_did(args.home.as_deref())?;
    submit_create(
        &args.graphql,
        &owner,
        AgentCreateInput {
            operation: AgentCreateOperation::Clone,
            clone_from: Some(args.source_agent_did),
            display_name: args.display_name,
            description: optional_update(args.description),
            system_prompt: optional_update(args.system_prompt),
            profile_id: args.profile_id,
            preset: None,
            root: optional_update(args.root),
            make_default: false,
        },
    )
    .await
}

pub(super) async fn agent_disable(args: AgentDisableArgs) -> Result<()> {
    let owner = local_node_did(args.home.as_deref())?;
    let access = ConfigAccess::Graphql(gents_service::home_state::resolve_graphql_endpoint(
        Some(&args.graphql),
        None,
    )?);
    let receipt = gents::operations::agent::disable(&access, &owner, &args.agent_did)
        .await
        .map_err(AgentFailure::edit)?;
    print_json(&serde_json::to_value(receipt)?)
}

pub(super) async fn agent_default(args: AgentDefaultArgs) -> Result<()> {
    let owner = local_node_did(args.home.as_deref())?;
    let access = ConfigAccess::Graphql(gents_service::home_state::resolve_graphql_endpoint(
        Some(&args.graphql),
        None,
    )?);
    let receipt =
        gents::operations::agent::choose_default(&access, &owner, &args.agent_did, args.enable)
            .await
            .map_err(AgentFailure::edit)?;
    print_json(&serde_json::to_value(receipt)?)
}

/// The row of `id` that `owner` holds. A caller that has only the ID resolves
/// its owner first through `gents_documents::operations::owner`.
async fn load_document(
    access: &ConfigAccess,
    collection: Collection,
    fields: &str,
    owner: &str,
    id: &str,
) -> Result<Option<Value>> {
    let query = format!(
        r#"{{
            {collection_type}(filter: {{ node_did: {{ _eq: "{owner}" }}, {unique_field}: {{ _eq: "{id}" }} }}) {{
                {fields} node_did
            }}
        }}"#,
        collection_type = collection.graphql_type(),
        owner = escape_graphql_string(owner),
        unique_field = collection.unique_field(),
        id = escape_graphql_string(id),
    );
    let rows = graphql_rows(access, collection.graphql_type(), &query).await?;
    anyhow::ensure!(
        rows.len() <= 1,
        "ambiguous {} {id:?}: {} live rows of {owner} resolve this logical id",
        collection.graphql_type(),
        rows.len()
    );
    Ok(rows.into_iter().next())
}

async fn classify_preset(access: &ConfigAccess, value: &Value) -> Result<Option<&'static str>> {
    use gents_documents::{merge_datastore_tool_surfaces, DatastoreToolSurfaceDocument, Tools};
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
            &tools.node_did,
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

pub(super) async fn agent_show(args: AgentShowArgs) -> Result<()> {
    let AgentShowArgs { show: args, node_did } = args;
    let id = resolve_dual_id("agent", "--id", args.id.as_deref(), args.id_flag.as_deref())?;
    args.output
        .ensure_supported("config agent show", &[OutputFormat::Json])?;
    let (access, _) = resolve_config_access(args.home.as_deref(), args.graphql.as_deref())
        .await
        .context("resolving access for config agent show")?;
    let owner =
        gents_documents::operations::owner::resolve_agent_owner(&access, node_did.as_deref(), &id)
            .await
            .map_err(|error| {
                let next = format!("gents config agent show {id} --node-did <NODE_DID>");
                let message = format!("{error}\nNext: {next}");
                anyhow::Error::new(error).context(message)
            })?;

    let mut row = load_document(
        &access,
        Collection::Agent,
        CANONICAL_AGENT_SHOW_FIELDS,
        &owner,
        &id,
    )
    .await?
    .ok_or_else(|| anyhow::anyhow!("not found: no Agent document with agent_did {id} on {owner}"))?;

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
                &node_did,
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
                &node_did,
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
                &node_did,
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
    use gents_store::identity::NodeIdentity as _;

    use defra_node::Node as EmbeddedNode;
    use gents_documents::Tools;
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
    async fn agent_create_and_clone_materialize_canonical_chain() -> Result<()> {
        let client = gents_host::test_support::ClientHome::new().await;
        let node = client.node().clone();
        let owner = client.node_did();
        use gents_documents::apply_desired_state_plan;
        use gents_documents::{DesiredStateApplyDocument, DesiredStateApplyPlan};
        let documents = [
            (Collection::InferenceBackend,json!({"node_did":owner,"backend_id":"backend","name":"Test","provider_kind":"OpenAiCompatible","endpoint":"http://127.0.0.1:1/v1","auth":{"kind":"unauthenticated"}})),
            (Collection::InferenceProfile,json!({"node_did":owner,"profile_id":"profile-1","backend_id":"backend","model_name":"test"})),
        ].into_iter().map(|(collection,value)| DesiredStateApplyDocument{collection,add:value.clone(),update:value}).collect();
        let plan = DesiredStateApplyPlan::new(documents)?;
        let access = client.access.clone();
        access
            .transact_write("test.agent.profile", |txn| {
                let plan = &plan;
                Box::pin(async move {
                    apply_desired_state_plan(
                        txn,
                        plan,
                        &gents_inference::resolved::ProviderProfilePolicy,
                    )
                    .await
                })
            })
            .await?;
        // Write-preset host tools run only under a User-published root.
        let workspace = tempfile::tempdir()?;
        let root = std::fs::canonicalize(workspace.path())?
            .to_string_lossy()
            .into_owned();
        gents_tools::surface::set_workspace_root_with_readers(
            &access,
            owner,
            &root,
            None,
            true,
            &[],
        )
        .await?;
        let user = identity::Did::new(client.user.did())?;
        let provision = || {
            gents_store::identity::provision_agent_identity_authorized(
                &node,
                client.home.identity.as_ref(),
                &user,
            )
        };
        let create = |operation| AgentCreateInput {
            operation,
            clone_from: None,
            display_name: "Canonical".into(),
            description: StringUpdate::Omitted,
            system_prompt: StringUpdate::Omitted,
            profile_id: "profile-1".into(),
            preset: None,
            root: StringUpdate::Omitted,
            make_default: false,
        };
        let doc = AgentCreateInput {
            description: StringUpdate::Set("Canonical test agent".into()),
            system_prompt: StringUpdate::Set("Perform the requested work and verify it.".into()),
            preset: Some("write".into()),
            root: StringUpdate::Set(root.clone()),
            ..create(AgentCreateOperation::Create)
        };
        let outcome = create_agent(&access, owner, provision().await?, &doc).await?;
        assert_eq!(outcome["committed"], true);
        let agent_did = outcome["agent_did"].as_str().context("created agent ID")?;

        let agent = gents_documents::load_agent(&node, owner, agent_did)
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
            Some(gents_documents::BashMode::Unrestricted),
        );

        let clone_doc = AgentCreateInput {
            clone_from: Some(agent_did.to_owned()),
            display_name: "Cloned".into(),
            ..create(AgentCreateOperation::Clone)
        };
        let clone_outcome = create_agent(&access, owner, provision().await?, &clone_doc).await?;
        assert_eq!(clone_outcome["committed"], true);
        let cloned_id = clone_outcome["agent_did"]
            .as_str()
            .context("cloned agent ID")?;
        assert_ne!(cloned_id, agent_did);
        let cloned = gents_documents::load_agent(&node, owner, cloned_id)
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
            "did:key:foreign",
            agent_did
        )
        .await?
        .is_none());
        assert!(load_document(
            &access,
            Collection::Agent,
            "agent_did creation_nonce node_did",
            owner,
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

        let mut printed = String::new();
        let named = |text: String| {
            let mut text = text;
            for (value, name) in [
                (outcome["agent"]["creation_nonce"].as_str(), "<NONCE>"),
                (
                    clone_outcome["agent"]["creation_nonce"].as_str(),
                    "<CLONE_NONCE>",
                ),
                (Some(agent_did), "<CREATED>"),
                (Some(cloned_id), "<CLONED>"),
                (Some(root.as_str()), "<ROOT>"),
                (Some(owner), "<NODE>"),
            ] {
                text = text.replace(value.expect("receipt names it"), name);
            }
            text
        };
        for (label, receipt) in [("create", &outcome), ("clone", &clone_outcome)] {
            printed.push_str(&format!(
                "## {label}\n{}\n",
                named(serde_json::to_string_pretty(receipt)?)
            ));
        }
        for (label, input) in [
            (
                "create with an unpublished profile",
                AgentCreateInput {
                    system_prompt: StringUpdate::Set("Work.".into()),
                    profile_id: "absent".into(),
                    ..create(AgentCreateOperation::Create)
                },
            ),
            (
                "create with an unpublished root",
                AgentCreateInput {
                    system_prompt: StringUpdate::Set("Work.".into()),
                    preset: Some("readonly".into()),
                    root: StringUpdate::Set("/pinned/unpublished".into()),
                    ..create(AgentCreateOperation::Create)
                },
            ),
            (
                "clone with a preset",
                AgentCreateInput {
                    clone_from: Some(agent_did.to_owned()),
                    display_name: "Cloned".into(),
                    preset: Some("write".into()),
                    ..create(AgentCreateOperation::Clone)
                },
            ),
        ] {
            let error = create_agent(&access, owner, provision().await?, &input)
                .await
                .expect_err(label);
            printed.push_str(&format!(
                "## {label}\n{}\n",
                named(format!("Error: {error:?}"))
            ));
        }
        assert!(
            printed == PINNED_OUTPUT,
            "agent create/clone CLI output changed\n<<<TRANSCRIPT\n{printed}TRANSCRIPT>>>"
        );
        Ok(())
    }

    const PINNED_OUTPUT: &str = r###"## create
{
  "admitted": true,
  "agent": {
    "agent_did": "<CREATED>",
    "context_id": "<CREATED>:context",
    "creation_nonce": "<NONCE>",
    "description": "Canonical test agent",
    "display_name": "Canonical",
    "inference_profile_id": "profile-1",
    "node_did": "<NODE>"
  },
  "agent_did": "<CREATED>",
  "committed": true,
  "host_root_access": {
    "answer": "Saved, but <CREATED> cannot use its host tools yet: a User must grant it reader on the published WorkspaceRoot <ROOT>. Until then the runtime cannot load its tools and reports the agent unavailable.",
    "how_to_proceed": "Keep these Tools and give the User user_command to run from their own client; no config or policy call, and no change to your own Tools, can grant it.",
    "published_root": "<ROOT>",
    "state": "waiting_for_user",
    "user_command": "gents config workspace-root grant-reader --graphql GRAPHQL_URL --path <ROOT> --agent-did <CREATED>"
  },
  "make_default": false,
  "operation": "create"
}
## clone
{
  "admitted": true,
  "agent": {
    "agent_did": "<CLONED>",
    "context_id": "<CLONED>:context",
    "creation_nonce": "<CLONE_NONCE>",
    "display_name": "Cloned",
    "inference_profile_id": "profile-1",
    "node_did": "<NODE>"
  },
  "agent_did": "<CLONED>",
  "committed": true,
  "host_root_access": {
    "answer": "Saved, but <CLONED> cannot use its host tools yet: a User must grant it reader on the published WorkspaceRoot <ROOT>. Until then the runtime cannot load its tools and reports the agent unavailable.",
    "how_to_proceed": "Keep these Tools and give the User user_command to run from their own client; no config or policy call, and no change to your own Tools, can grant it.",
    "published_root": "<ROOT>",
    "state": "waiting_for_user",
    "user_command": "gents config workspace-root grant-reader --graphql GRAPHQL_URL --path <ROOT> --agent-did <CLONED>"
  },
  "make_default": false,
  "operation": "clone"
}
## create with an unpublished profile
Error: select a published inference profile
Next: gents config agent create --help
## create with an unpublished root
Error: Tools.host.root "/pinned/unpublished" is not admitted by published WorkspaceRoot policy: nonexistent path is outside the allowed roots
Next: gents config agent create --help
## clone with a preset
Error: clone inherits its source tools; edit the cloned Tools to change grants
Next: gents config agent --help
"###;

    #[tokio::test]
    async fn raw_set_rejects_dangling_profile_reference() -> Result<()> {
        use gents_documents::apply_desired_state_plan;
        use gents_documents::{DesiredStateApplyDocument, DesiredStateApplyPlan};
        let client = gents_host::test_support::ClientHome::new().await;
        let binding =
            gents_store::identity::provision_agent_identity(client.home.identity.as_ref())?;
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
                Box::pin(async move {
                    apply_desired_state_plan(
                        txn,
                        plan,
                        &gents_inference::resolved::ProviderProfilePolicy,
                    )
                    .await
                })
            })
            .await
            .unwrap_err();
        assert!(
            format!("{error:#}").contains("missing-profile"),
            "{error:#}"
        );
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
