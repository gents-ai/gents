use gents_store::node_scope::ContextCalls as _;
use std::sync::Arc;

use gents_documents::{apply_desired_state_plan, DesiredStateApplyDocument, DesiredStateApplyPlan};
use gents_protocol::agent_identity::AgentIdentityBinding;
use gents_store::collection::Collection;
use gents_store::config_client::ConfigAccess;
use gents_store::local::backend_protection::FreshBackendStore;
use gents_store::local::document_acp::DocumentAcp;
use serde_json::json;

use super::{home, start, Home};

/// A running home whose Node created its own Node document and the Agent
/// documents of the Agents it derived, each administering the Node, as the
/// Engineer does.
pub(super) struct Automated {
    _directory: tempfile::TempDir,
    home: Home,
    pub(super) node: Arc<defra_node::Node>,
    pub(super) node_did: identity::Did,
}

impl Automated {
    pub(super) async fn open(provider: DocumentAcp) -> anyhow::Result<Self> {
        let directory = tempfile::tempdir()?;
        let home = home(directory.path(), "configuration-readers").await?;
        FreshBackendStore::observe(&home.data, &home.lock)
            .await?
            .initialize(&home.key, &home.node, &home.owner, provider)
            .await?;
        let node = start(&home, provider).await?;
        let node_did = identity::Did::new(home.node.did())?;
        Ok(Self {
            _directory: directory,
            home,
            node,
            node_did,
        })
    }

    fn as_node(&self) -> ConfigAccess {
        ConfigAccess::local_as(self.node.clone(), self.node_did.clone())
    }

    async fn node_doc(&self) -> anyhow::Result<String> {
        let node_did = self.node_did.clone();
        let existing = self
            .as_node()
            .execute(&format!(
                r#"{{ Node(filter: {{node_did: {{_eq: "{node_did}"}}}}) {{ _docID }} }}"#
            ))
            .await?;
        if let Some(doc) = existing["data"]["Node"][0]["_docID"].as_str() {
            return Ok(doc.to_owned());
        }
        let created = self
            .as_node()
            .write_with_variables(
                "test.configuration_readers.node",
                "mutation($node: String) { create_Node(input: {node_did: $node}) { _docID } }",
                &json!({ "node": node_did.as_str() }),
            )
            .await?;
        Ok(gents_store::graphql::created_doc_id(&created, "Node")?)
    }

    /// An Agent the runtime derives, under the Node, administering it when
    /// `administers`.
    pub(super) async fn agent(&self, administers: bool) -> anyhow::Result<AgentIdentityBinding> {
        let binding = gents_store::identity::provision_agent_identity(self.home.node.as_ref())?;
        let node_doc = self.node_doc().await?;
        let (runtime, agent) = (
            self.node_did.clone(),
            identity::Did::new(&binding.agent_did)?,
        );
        self.as_node()
            .transact_write("test.configuration_readers.agent", |txn| {
                let (runtime, agent, node_doc, binding) = (&runtime, &agent, &node_doc, &binding);
                Box::pin(async move {
                    let created = txn
                        .execute_with_variables(
                            "mutation($input: AgentMutationInputArg!) { create_Agent(input: $input) { _docID } }",
                            &json!({"input": {
                                "node_did": runtime.as_str(),
                                "agent_did": binding.agent_did,
                                "creation_nonce": binding.creation_nonce,
                                "inference_profile_id": "fixture",
                                "enabled": true,
                            }}),
                        )
                        .await?;
                    let agent_doc = gents_store::graphql::created_doc_id(&created, "Agent")?;
                    txn.add_resource_relationship("Agent", &agent_doc, "parent", "nodes", node_doc)
                        .await?;
                    txn.add_actor_relationship("Agent", &agent_doc, "executor", agent)
                        .await?;
                    if administers {
                        txn.add_actor_relationship("Node", node_doc, "administrator", agent)
                            .await?;
                    }
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await?;
        Ok(binding)
    }

    /// Wait until the provider holds everything the committed transaction
    /// `applied` owes it, so what it committed has taken effect; a local home
    /// owes nothing.
    pub(super) async fn settled(&self, applied: &Applied) -> anyhow::Result<()> {
        if gents_store::local::document_acp::provider_of(&self.node) == DocumentAcp::Local {
            return Ok(());
        }
        let handle = applied
            .0
            .as_ref()
            .expect("an embedded apply has a transaction handle");
        let context =
            defra_core::RequestContext::default().with_identity(Some(self.node_did.to_string()));
        match self
            .node
            .await_owed_acp_as(&context, handle, gents_store::test_support::STALL_CEILING)
            .await?
        {
            acp::OwedWait::Settled => Ok(()),
            unsettled => anyhow::bail!("the apply's owed changes did not settle: {unsettled:?}"),
        }
    }

    pub(super) fn agent_access(
        &self,
        binding: &AgentIdentityBinding,
    ) -> anyhow::Result<ConfigAccess> {
        let agent =
            gents_store::identity::resolve_agent_identity(self.home.node.as_ref(), binding)?;
        Ok(ConfigAccess::local_managed_agent(
            self.node.clone(),
            identity::Did::new(agent.did())?,
            agent.as_ref(),
        )?
        .with_runtime_agent_proof(gents_store::identity::RuntimeAgentProof::new(
            self.home.node.clone(),
        )))
    }

    /// Whether `agent` reads the configuration row of `collection` whose
    /// `field` is `value`.
    pub(super) async fn reads(
        &self,
        binding: &AgentIdentityBinding,
        collection: &str,
        field: &str,
        value: &str,
    ) -> anyhow::Result<bool> {
        let response = self
            .agent_access(binding)?
            .execute(&format!("{{ {collection} {{ {field} }} }}"))
            .await?;
        Ok(response["data"][collection]
            .as_array()
            .into_iter()
            .flatten()
            .any(|row| row[field].as_str() == Some(value)))
    }
}

fn document(collection: Collection, value: serde_json::Value) -> DesiredStateApplyDocument {
    DesiredStateApplyDocument {
        collection,
        add: value.clone(),
        update: value,
    }
}

/// The event automation that runs `agent` when a Task is created.
pub(super) fn automation(node_did: &str, agent: &str) -> Vec<DesiredStateApplyDocument> {
    vec![
        document(
            Collection::Task,
            json!({"node_did": node_did, "task_id": "task", "agent_did": agent, "prompt_template": "hello"}),
        ),
        document(
            Collection::EventSource,
            json!({"node_did": node_did, "event_source_id": "source", "source_collection": "Task", "event_kind": "created"}),
        ),
        document(
            Collection::Trigger,
            json!({"node_did": node_did, "trigger_id": "trigger", "task_id": "task", "source": {"kind": "event", "event_source_id": "source"}}),
        ),
    ]
}

pub(super) async fn apply(
    access: &ConfigAccess,
    documents: Vec<DesiredStateApplyDocument>,
) -> anyhow::Result<Applied> {
    let plan = DesiredStateApplyPlan::new(documents)?;
    access
        .transact_write("test.configuration_readers.apply", |txn| {
            let plan = &plan;
            Box::pin(async move {
                apply_desired_state_plan(
                    txn,
                    plan,
                    &gents_documents::test_support::AdmitAllProfiles,
                )
                .await
                .map(|_| Applied(txn.embedded_handle_for_test()))
            })
        })
        .await
}

/// A committed apply, named by its embedded transaction.
#[derive(Debug)]
pub(super) struct Applied(Option<query::TransactionHandle>);

pub(super) async fn rebind_own_automation(provider: DocumentAcp) -> anyhow::Result<()> {
    let home = Automated::open(provider).await?;
    let (engineer, first, second) = (
        home.agent(true).await?,
        home.agent(false).await?,
        home.agent(false).await?,
    );
    let engineer_access = home.agent_access(&engineer)?;
    let node_did = home.node_did.to_string();
    let applied = apply(&engineer_access, automation(&node_did, &first.agent_did)).await?;
    home.settled(&applied).await?;
    assert!(
        home.reads(&first, "Trigger", "trigger_id", "trigger")
            .await?
    );
    assert!(
        !home
            .reads(&second, "Trigger", "trigger_id", "trigger")
            .await?
    );
    let applied = apply(&engineer_access, automation(&node_did, &second.agent_did)).await?;
    home.settled(&applied).await?;
    for (collection, field, value) in [
        ("Trigger", "trigger_id", "trigger"),
        ("EventSource", "event_source_id", "source"),
    ] {
        assert!(
            home.reads(&second, collection, field, value).await?,
            "the newly selected Agent reads the {collection}"
        );
    }
    home.node.shutdown().await?;
    Ok(())
}

/// Lean `Identity.OwnerStaged.an_owner_grants_its_own_configuration_on_any_home`:
/// an Agent rebinding automation it created grants the newly selected Agent
/// `reader` of its Trigger and EventSource itself.
#[tokio::test]
async fn an_agent_rebinding_its_own_automation_grants_its_readers_on_a_local_home(
) -> anyhow::Result<()> {
    rebind_own_automation(DocumentAcp::Local).await
}

/// Lean `Identity.OwnerStaged.a_home_on_the_node_store_grants_configuration_as_before`:
/// on the node's own store an Agent that did not create the automation
/// changes its readers itself, as before, and native ACP refuses it.
#[tokio::test]
async fn another_agents_automation_is_rebound_directly_and_refused_on_a_local_home(
) -> anyhow::Result<()> {
    let home = Automated::open(DocumentAcp::Local).await?;
    let (creator, engineer, first, second) = (
        home.agent(true).await?,
        home.agent(true).await?,
        home.agent(false).await?,
        home.agent(false).await?,
    );
    let node_did = home.node_did.to_string();
    apply(
        &home.agent_access(&creator)?,
        automation(&node_did, &first.agent_did),
    )
    .await?;
    let rebound = apply(
        &home.agent_access(&engineer)?,
        automation(&node_did, &second.agent_did),
    )
    .await;
    assert!(rebound.is_err(), "{rebound:?}");
    assert!(
        !home
            .reads(&second, "Trigger", "trigger_id", "trigger")
            .await?
    );
    home.node.shutdown().await?;
    Ok(())
}

/// Lean `Identity.Household.mayManage`: an Agent that neither created a
/// Trigger nor manages its readers is refused changing them, on either kind of
/// home, with a refusal that says who may make the change.
#[tokio::test]
async fn a_reader_change_on_another_agents_trigger_is_refused_naming_who_may_change_it(
) -> anyhow::Result<()> {
    for provider in [DocumentAcp::Local, DocumentAcp::Vera] {
        let home = Automated::open(provider).await?;
        let (creator, engineer, first) = (
            home.agent(true).await?,
            home.agent(true).await?,
            home.agent(false).await?,
        );
        let node_did = home.node_did.to_string();
        let applied = apply(
            &home.agent_access(&creator)?,
            automation(&node_did, &first.agent_did),
        )
        .await?;
        home.settled(&applied).await?;
        assert!(
            home.reads(&first, "Trigger", "trigger_id", "trigger")
                .await?
        );
        let trigger = home
            .agent_access(&creator)?
            .execute("{ Trigger { _docID } }")
            .await?["data"]["Trigger"][0]["_docID"]
            .as_str()
            .expect("the creator reads its Trigger")
            .to_owned();
        let first_did = identity::Did::new(&first.agent_did)?;
        let refused = home
            .agent_access(&engineer)?
            .transact_write("test.configuration_readers.unmanaged", |txn| {
                let (trigger, first_did) = (&trigger, &first_did);
                Box::pin(async move {
                    txn.delete_actor_relationship("Trigger", trigger, "reader", first_did)
                        .await
                })
            })
            .await
            .expect_err("an Agent that did not create the Trigger cannot revoke its reader");
        assert!(
            matches!(
                refused,
                gents_store::config_client::ConfigClientError::RelationshipNotManaged(_)
            ),
            "{provider}: {refused:?}"
        );
        assert!(
            refused.to_string().contains(
                "only the identity that created this document, or one that manages this \
                     kind of access to it, can change who it is shared with; make this change \
                     as one of those identities"
            ),
            "{provider}: {refused}"
        );
        assert!(
            home.reads(&first, "Trigger", "trigger_id", "trigger")
                .await?
        );
        home.node.shutdown().await?;
    }
    Ok(())
}
