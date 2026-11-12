#![cfg(feature = "test-fixtures")]

use gents_documents::views::config::{
    AgentContextList, AgentTargetList, CompactionConfigList, DatastoreToolSurfaceList,
    DefaultAgentSelectionList, NodeList, PageList, PluginSourceList, SkillList, ToolsList,
};
use gents_inference::test_support::signed_test_node_without_agent;
use gents_store::config_client::ConfigAccess;
use gents_store::identity::{GrantSet, NodeIdentity};
use serde_json::{json, Value};

async fn create(
    access: &ConfigAccess,
    collection: &str,
    schema: &str,
    input: Value,
) -> anyhow::Result<String> {
    let document = format!(
        "mutation($input: {collection}MutationInputArg!) {{ create_{collection}(input: $input) {{ _docID }} }}"
    );
    let variables = json!({"input": input});
    let response = access
        .transact_write("test.configuration_view.create", |txn| {
            let document = &document;
            let variables = &variables;
            Box::pin(async move {
                txn.execute_client_signed_create(document, variables, schema)
                    .await
            })
        })
        .await?;
    Ok(gents_store::graphql::created_doc_id(&response, collection)?)
}

#[tokio::test]
async fn configuration_views_match_model_on_served_node() -> anyhow::Result<()> {
    let (db, _, owner) = signed_test_node_without_agent(
        "configuration-views",
        gents_store::node_extension::Extensions::default(),
    )
    .await;
    let (reader, stranger) = tokio::task::spawn_blocking(|| {
        (
            gents_store::test_support::test_identity("configuration-view-reader"),
            gents_store::test_support::test_identity("configuration-view-stranger"),
        )
    })
    .await?;
    let administrator = ConfigAccess::local_as(db.node.clone(), db.node_identity.did().parse()?);
    for actor in [&reader, &stranger] {
        GrantSet::OperatorUser
            .grant(&administrator, &actor.did().parse()?)
            .await?;
    }
    let node = "did:example:configuration-views";
    let fixtures = [
        (
            "node",
            "Node",
            gents_protocol::schemas::NODE,
            json!({"node_did":node,"display_name":"View node","enabled":true}),
        ),
        (
            "default_selection",
            "DefaultAgentSelection",
            gents_protocol::schemas::DEFAULT_AGENT_SELECTION,
            json!({"node_did":node}),
        ),
        (
            "context",
            "AgentContext",
            gents_protocol::schemas::AGENT_CONTEXT,
            json!({"node_did":node,"context_id":"view-context"}),
        ),
        (
            "compaction",
            "CompactionConfig",
            gents_protocol::schemas::COMPACTION_CONFIG,
            json!({"node_did":node,"compaction_id":"view-compaction"}),
        ),
        (
            "skill",
            "Skill",
            gents_protocol::schemas::SKILL,
            json!({"node_did":node,"skill_id":"view-skill"}),
        ),
        (
            "tools",
            "Tools",
            gents_protocol::schemas::TOOLS,
            json!({"node_did":node,"tools_id":"view-tools"}),
        ),
        (
            "target",
            "AgentTarget",
            gents_protocol::schemas::AGENT_TARGET,
            json!({"node_did":node,"target_id":"view-target","target_node_did":node,"agent_did":reader.did(),"name":"View target"}),
        ),
        (
            "surface",
            "DatastoreToolSurface",
            gents_protocol::schemas::DATASTORE_TOOL_SURFACE,
            json!({"node_did":node,"surface_id":"view-surface"}),
        ),
        (
            "page",
            "Page",
            gents_protocol::schemas::PAGE,
            json!({"node_did":node,"page_id":"view-page","module":"export default {}"}),
        ),
        (
            "plugin_source",
            "PluginSource",
            gents_protocol::schemas::PLUGIN_SOURCE,
            json!({"node_did":node,"plugin_id":"view-plugin","language":"rust","source":""}),
        ),
    ];
    for (_, collection, schema, input) in fixtures {
        let document = create(&owner, collection, schema, input).await?;
        owner
            .add_actor_relationship(collection, &document, "reader", &reader.did().parse()?)
            .await?;
    }
    let cases = gents_lean_vocab::families::configuration_views_inventory::cases::<String>();
    assert_eq!(cases.len(), 40);
    for case in cases {
        let access = db.client_access(if case.input.readable {
            &reader
        } else {
            &stranger
        });
        let node = if case.input.same_node {
            node
        } else {
            "did:example:other-node"
        };
        let visible = match case.input.collection.as_str() {
            "node" => !access.read_view(&NodeList::new(node)).await?.is_empty(),
            "default_selection" => !access
                .read_view(&DefaultAgentSelectionList::new(node))
                .await?
                .is_empty(),
            "context" => !access
                .read_view(&AgentContextList::new(node))
                .await?
                .is_empty(),
            "compaction" => !access
                .read_view(&CompactionConfigList::new(node))
                .await?
                .is_empty(),
            "skill" => !access.read_view(&SkillList::new(node)).await?.is_empty(),
            "tools" => !access.read_view(&ToolsList::new(node)).await?.is_empty(),
            "target" => !access
                .read_view(&AgentTargetList::new(node))
                .await?
                .is_empty(),
            "surface" => !access
                .read_view(&DatastoreToolSurfaceList::new(node))
                .await?
                .is_empty(),
            "page" => !access.read_view(&PageList::new(node)).await?.is_empty(),
            "plugin_source" => !access
                .read_view(&PluginSourceList::new(node))
                .await?
                .is_empty(),
            other => anyhow::bail!("unknown configuration view {other}"),
        };
        case.check(visible);
    }
    db.node.shutdown().await?;
    Ok(())
}
