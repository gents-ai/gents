//! Native end-to-end tests for the `policy` resource. The Engineer's
//! managed-Agent signer drives a signed node, and DefraDB's held-bounded NAC
//! rule decides every change; these tests assert the receipts and refusals
//! the command reports around those decisions.

use super::*;

use std::sync::Arc;

use crate::inference::llm::tool::{ToolDyn, ToolError};
use crate::store::config_client::{ConfigAccess, ConfigurationNodeBinding, NodePermission};
use crate::tools::builtin::self_config::{build_self_config_tools_with_runtime, CONFIG_TOOL_NAME};
use crate::tools::surface::SelfConfigToolConfig;
use crate::NodeIdentity;

/// A signed node whose Engineer is the fixture Agent, reached through the
/// managed-Agent signer the config tool uses. The Agent's own baseline grants
/// come from its published document; every other node permission is granted
/// here by the runtime owner, as a User's init would.
struct PolicyFixture {
    db: crate::support::TestDb,
    user_access: ConfigAccess,
    agent: Arc<dyn NodeIdentity>,
    agent_did: String,
    node_binding: ConfigurationNodeBinding,
}

impl PolicyFixture {
    async fn new(name: &str) -> Self {
        let (db, _user, user_access, binding) = crate::test_support::signed_test_node(name).await;
        let agent =
            crate::store::identity::resolve_agent_identity(db.node_identity.as_ref(), &binding)
                .expect("fixture Agent is derived from the held runtime");
        let response = user_access.execute("{ Node { _docID } }").await.unwrap();
        let node_binding = ConfigurationNodeBinding {
            node_did: ::identity::Did::new(db.node_identity.did()).unwrap(),
            doc_id: response["data"]["Node"][0]["_docID"]
                .as_str()
                .unwrap()
                .to_owned(),
            enabled: true,
            default_agent_did: Some(binding.agent_did.clone()),
        };
        Self {
            db,
            user_access,
            agent,
            agent_did: binding.agent_did,
            node_binding,
        }
    }

    fn admin(&self) -> ConfigAccess {
        ConfigAccess::local_as(
            self.db.node.clone(),
            ::identity::Did::new(self.db.node_identity.did()).unwrap(),
        )
    }

    /// Grants the Engineer each permission as the runtime owner.
    async fn grant_engineer(&self, permissions: &[NodePermission]) {
        let engineer = ::identity::Did::new(self.agent_did.clone()).unwrap();
        for permission in permissions {
            self.admin()
                .grant_node_permission(&engineer, *permission)
                .await
                .unwrap();
        }
    }

    /// Publishes a second Agent through the User, as `install_sibling` does.
    async fn install_sibling(&self, display_name: &str) -> String {
        let user = ::identity::Did::new(self.user_access.actor_did().unwrap()).unwrap();
        let binding = crate::store::identity::provision_agent_identity_authorized(
            &self.db.node,
            self.db.node_identity.as_ref(),
            &user,
        )
        .await
        .unwrap();
        crate::test_support::install_test_agent_binding_with_access(
            self.db.node_identity.did(),
            display_name,
            &binding,
            &self.user_access,
        )
        .await;
        binding.agent_did
    }

    async fn agent_doc_id(&self, agent_did: &str) -> String {
        let query = format!(
            r#"{{ Agent(filter: {{agent_did: {{_eq: "{}"}}}}) {{ _docID }} }}"#,
            crate::store::graphql::escape_graphql_string(agent_did),
        );
        let response = self.user_access.execute(&query).await.unwrap();
        response["data"]["Agent"][0]["_docID"]
            .as_str()
            .expect("the Agent document exists")
            .to_owned()
    }

    /// The config tool as an Agent with `categories`, the policy grant
    /// `no_lockout` setting, and the managed-Agent signer.
    fn tools(&self, categories: &[&str], no_lockout: bool) -> Vec<Box<dyn ToolDyn>> {
        let config = SelfConfigToolConfig {
            enabled: true,
            agent_did: self.agent_did.clone(),
            categories: categories.iter().map(|c| c.to_string()).collect(),
            no_lockout,
            preview: false,
            enable_pack_install: false,
            process_ceiling: Default::default(),
        };
        build_self_config_tools_with_runtime(
            Arc::default(),
            self.db.node.clone(),
            self.db.node_identity.did().to_owned(),
            Some(self.agent.clone()),
            Some(self.db.node_identity.clone()),
            &config,
            Arc::new(crate::plugin::executor::PluginExecutor::default()),
            Some(self.node_binding.clone()),
        )
    }
}

fn config(tools: &[Box<dyn ToolDyn>]) -> &dyn ToolDyn {
    tools
        .iter()
        .find(|tool| tool.name() == CONFIG_TOOL_NAME)
        .expect("config tool is registered")
        .as_ref()
}

/// `Ok` is the receipt object; `Err` is the typed failure object, whose
/// `recovery.next_call` names the next command.
async fn run(tool: &dyn ToolDyn, argv: Value) -> std::result::Result<Value, Value> {
    match tool.call(argv.to_string()).await {
        Ok(text) => Ok(serde_json::from_str(&text).expect("policy success is JSON")),
        Err(ToolError::ToolCallError(error)) => {
            Err(serde_json::from_str(&error.to_string()).expect("policy failure is typed JSON"))
        }
        Err(error) => panic!("unexpected tool error: {error}"),
    }
}

fn policy_get(subject: &str) -> Value {
    json!({"argv": ["policy", "get", "--subject", subject]})
}

fn grant_node(subject: &str, permission: &str) -> Value {
    json!({"argv": ["policy", "grant", "--subject", subject, "--permission", permission]})
}

fn revoke_node(subject: &str, permission: &str) -> Value {
    json!({"argv": ["policy", "revoke", "--subject", subject, "--permission", permission]})
}

fn permission_names(view: &Value) -> Vec<String> {
    view["node_permissions"]
        .as_array()
        .expect("node_permissions list")
        .iter()
        .map(|entry| entry["permission"].as_str().unwrap().to_owned())
        .collect()
}

fn assert_refused_with_policy_get(failure: &Value, verb: &str, permission: &str) {
    let message = failure["error"].as_str().expect("error message");
    assert!(
        message.contains(&format!("policy {verb} refused for node permission {permission}")),
        "{message}"
    );
    assert_eq!(
        failure["recovery"]["next_call"],
        json!({"argv": ["policy", "get"]}),
        "{failure}"
    );
}

#[tokio::test]
async fn engineer_grants_held_permission_to_sibling_and_policy_get_shows_it() {
    let fixture = PolicyFixture::new("policy-grant-held").await;
    let sibling = fixture.install_sibling("Reviewer").await;
    fixture
        .grant_engineer(&[
            NodePermission::NacRelationAdd,
            NodePermission::NacRelationDelete,
            NodePermission::NacStatus,
            NodePermission::CollectionGet,
        ])
        .await;
    let tools = fixture.tools(&["policy"], false);
    let tool = config(&tools);

    let receipt = run(tool, grant_node(&sibling, "get-collection"))
        .await
        .expect("a held permission under add-nac-relation is admitted");
    assert_eq!(
        receipt["answer"].as_str().unwrap(),
        format!("Granted node permission get-collection to {sibling}.")
    );
    let access = receipt["effective_access"].as_str().unwrap();
    assert!(access.contains("still holds it through: direct grant"), "{access}");
    assert_eq!(receipt["metadata"]["acting_agent_did"], fixture.agent_did);
    assert_eq!(receipt["metadata"]["subject_did"], sibling);
    assert!(receipt["config_execution"].is_object(), "{receipt}");

    let view = run(tool, policy_get(&sibling)).await.unwrap();
    let granted = view["node_permissions"]
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["permission"] == "get-collection" && entry["via"] == "direct grant");
    assert!(granted, "{view}");
}

#[tokio::test]
async fn engineer_grant_of_non_held_permission_is_refused_naming_policy_get() {
    let fixture = PolicyFixture::new("policy-grant-not-held").await;
    let sibling = fixture.install_sibling("Reviewer").await;
    fixture
        .grant_engineer(&[
            NodePermission::NacRelationAdd,
            NodePermission::NacRelationDelete,
            NodePermission::NacStatus,
        ])
        .await;
    let tools = fixture.tools(&["policy"], false);
    let tool = config(&tools);

    let failure = run(tool, grant_node(&sibling, "delete-document"))
        .await
        .expect_err("the Engineer does not hold delete-document");
    assert_refused_with_policy_get(&failure, "grant", "delete-document");
    let message = failure["error"].as_str().unwrap();
    assert!(message.contains("You hold:"), "{message}");
    assert!(message.contains("add-nac-relation"), "{message}");

    let view = run(tool, policy_get(&sibling)).await.unwrap();
    assert!(
        !permission_names(&view).contains(&"delete-document".to_owned()),
        "{view}"
    );
}

#[tokio::test]
async fn engineer_grant_of_admin_class_permission_is_refused_even_when_held() {
    let fixture = PolicyFixture::new("policy-grant-admin-class").await;
    let sibling = fixture.install_sibling("Reviewer").await;
    // The Engineer holds the admin-class permissions it tries to delegate, so
    // only the admin-class rule can refuse these grants.
    fixture
        .grant_engineer(&[
            NodePermission::NacRelationAdd,
            NodePermission::NacRelationDelete,
            NodePermission::NacStatus,
            NodePermission::DacBypass,
        ])
        .await;
    let tools = fixture.tools(&["policy"], false);
    let tool = config(&tools);

    for permission in ["bypass-dac", "add-nac-relation"] {
        let failure = run(tool, grant_node(&sibling, permission))
            .await
            .expect_err("admin-class permissions are never delegated");
        assert_refused_with_policy_get(&failure, "grant", permission);
    }

    let view = run(tool, policy_get(&sibling)).await.unwrap();
    let names = permission_names(&view);
    assert!(!names.contains(&"bypass-dac".to_owned()), "{view}");
    assert!(!names.contains(&"add-nac-relation".to_owned()), "{view}");
}

#[tokio::test]
async fn engineer_revokes_held_permission_and_receipt_states_access_honestly() {
    let fixture = PolicyFixture::new("policy-revoke-held").await;
    let sibling = fixture.install_sibling("Reviewer").await;
    fixture
        .grant_engineer(&[
            NodePermission::NacRelationAdd,
            NodePermission::NacRelationDelete,
            NodePermission::NacStatus,
            NodePermission::CollectionGet,
        ])
        .await;
    let tools = fixture.tools(&["policy"], false);
    let tool = config(&tools);
    run(tool, grant_node(&sibling, "get-collection"))
        .await
        .expect("grant succeeds before the revoke");

    let receipt = run(tool, revoke_node(&sibling, "get-collection"))
        .await
        .expect("a held permission under delete-nac-relation is revoked");
    assert_eq!(
        receipt["answer"].as_str().unwrap(),
        format!("Revoked node permission get-collection from {sibling}.")
    );
    // The sibling's baseline does not include get-collection, so no path remains.
    assert_eq!(
        receipt["effective_access"].as_str().unwrap(),
        "The subject no longer holds it through any path."
    );
    let assumptions = receipt["assumptions"].as_array().unwrap();
    assert!(
        assumptions.iter().any(|note| note
            .as_str()
            .unwrap()
            .contains("Grants carry no provenance")),
        "{receipt}"
    );

    let view = run(tool, policy_get(&sibling)).await.unwrap();
    assert!(
        !permission_names(&view).contains(&"get-collection".to_owned()),
        "{view}"
    );
}

#[tokio::test]
async fn policy_explain_names_the_real_denial_and_the_grant_that_would_allow_it() {
    let fixture = PolicyFixture::new("policy-explain-denial").await;
    let sibling = fixture.install_sibling("Reviewer").await;
    fixture
        .grant_engineer(&[
            NodePermission::NacRelationAdd,
            NodePermission::NacRelationDelete,
            NodePermission::NacStatus,
        ])
        .await;
    let tools = fixture.tools(&["policy"], false);
    let tool = config(&tools);

    // Self: the Engineer does not hold get-collection, so the denial is real.
    let denial = run(
        tool,
        json!({"argv": ["policy", "explain", "--permission", "get-collection"]}),
    )
    .await
    .unwrap();
    assert_eq!(
        denial["answer"].as_str().unwrap(),
        "no: you do not hold node permission get-collection."
    );
    let how = denial["how_to_proceed"].as_str().unwrap();
    assert!(
        how.contains("You do not hold get-collection, so you cannot delegate it."),
        "{how}"
    );

    // Admin-class: the explanation says delegation never applies.
    let admin_class = run(
        tool,
        json!({"argv": ["policy", "explain", "--permission", "bypass-dac"]}),
    )
    .await
    .unwrap();
    let how = admin_class["how_to_proceed"].as_str().unwrap();
    assert!(how.contains("admin-class"), "{how}");

    // A held permission with add-nac-relation names the grant that allows it.
    fixture
        .grant_engineer(&[NodePermission::CollectionGet])
        .await;
    let allowed_path = run(
        tool,
        json!({"argv": ["policy", "explain", "--subject", sibling, "--permission", "get-collection"]}),
    )
    .await
    .unwrap();
    let how = allowed_path["how_to_proceed"].as_str().unwrap();
    assert!(
        how.contains(&format!(
            "policy grant --subject {sibling} --permission get-collection"
        )),
        "{how}"
    );
}

#[tokio::test]
async fn explain_reports_missing_add_nac_relation_when_permission_is_held() {
    let fixture = PolicyFixture::new("policy-explain-no-operation").await;
    let sibling = fixture.install_sibling("Reviewer").await;
    fixture
        .grant_engineer(&[
            NodePermission::NacStatus,
            NodePermission::CollectionGet,
        ])
        .await;
    let tools = fixture.tools(&["policy"], false);
    let tool = config(&tools);

    let explained = run(
        tool,
        json!({"argv": ["policy", "explain", "--subject", sibling, "--permission", "get-collection"]}),
    )
    .await
    .unwrap();
    let how = explained["how_to_proceed"].as_str().unwrap();
    assert!(
        how.contains("You hold the permission but not add-nac-relation"),
        "{how}"
    );
}

#[tokio::test]
async fn agent_without_policy_category_is_refused_even_with_native_operation() {
    let fixture = PolicyFixture::new("policy-no-category").await;
    let sibling = fixture.install_sibling("Reviewer").await;
    fixture
        .grant_engineer(&[
            NodePermission::NacRelationAdd,
            NodePermission::NacRelationDelete,
            NodePermission::NacStatus,
        ])
        .await;
    // Native grants alone do not open the resource; the category gate applies first.
    let tools = fixture.tools(&["tools"], false);
    let tool = config(&tools);

    let grant = run(tool, grant_node(&sibling, "get-collection"))
        .await
        .expect_err("policy is not granted to this Agent");
    assert!(
        grant["error"]
            .as_str()
            .unwrap()
            .contains("policy configuration is not granted"),
        "{grant}"
    );
    let get = run(tool, policy_get(&sibling))
        .await
        .expect_err("policy get is gated by the same category");
    assert!(
        get["error"]
            .as_str()
            .unwrap()
            .contains("policy configuration is not granted"),
        "{get}"
    );
}

#[tokio::test]
async fn policy_get_on_another_subject_is_refused_without_nac_status() {
    let fixture = PolicyFixture::new("policy-read-refusal").await;
    let sibling = fixture.install_sibling("Reviewer").await;
    let tools = fixture.tools(&["policy"], false);
    let tool = config(&tools);

    let failure = run(tool, policy_get(&sibling))
        .await
        .expect_err("another subject's grants need NacStatus");
    let message = failure["error"].as_str().unwrap();
    assert!(
        message.contains("cannot read the node permissions of"),
        "{message}"
    );
    assert_eq!(
        failure["recovery"]["next_call"],
        json!({"argv": ["policy", "get"]})
    );
    // Reading your own permissions needs no NacStatus.
    run(tool, json!({"argv": ["policy", "get"]}))
        .await
        .expect("policy get on self is always readable");
}

#[tokio::test]
async fn engineer_grants_document_relation_through_manager_relation() {
    let fixture = PolicyFixture::new("policy-document-relation").await;
    let reviewer = fixture.install_sibling("Reviewer").await;
    let helper = fixture.install_sibling("Helper").await;
    fixture
        .grant_engineer(&[
            NodePermission::NacRelationAdd,
            NodePermission::NacRelationDelete,
            NodePermission::NacStatus,
        ])
        .await;
    // `administrator` on an Agent manages `config_writer`, so the Engineer
    // may add or remove that relation on this document and nothing else.
    let doc = fixture.agent_doc_id(&reviewer).await;
    fixture
        .user_access
        .add_actor_relationship(
            "Agent",
            &doc,
            "administrator",
            &::identity::Did::new(fixture.agent_did.clone()).unwrap(),
        )
        .await
        .unwrap();
    let tools = fixture.tools(&["policy"], false);
    let tool = config(&tools);
    let argv = |verb: &str, subject: &str| {
        json!({"argv": [
            "policy", verb,
            "--collection", "Agent",
            "--doc", doc.as_str(),
            "--relation", "config_writer",
            "--subject", subject,
        ]})
    };

    let receipt = run(tool, argv("grant", &helper))
        .await
        .expect("a manager of config_writer may grant it on the document");
    assert_eq!(
        receipt["answer"].as_str().unwrap(),
        format!("Granted relation config_writer on Agent document {doc} to {helper}.")
    );
    assert_eq!(
        receipt["effective_access"].as_str().unwrap(),
        "A direct relationship of this kind remains for the subject on this document."
    );

    let check = run(tool, argv("check", &helper)).await.unwrap();
    assert!(
        check["answer"].as_str().unwrap().starts_with("yes:"),
        "{check}"
    );

    let revoked = run(tool, argv("revoke", &helper))
        .await
        .expect("the same manager may revoke the direct relation");
    let access = revoked["effective_access"].as_str().unwrap();
    assert!(
        access.contains("was not evaluated"),
        "a removed direct relation must not claim the subject lost every path: {access}"
    );
}

#[tokio::test]
async fn policy_revoke_of_own_lockout_permissions_is_refused_under_no_lockout() {
    let fixture = PolicyFixture::new("policy-self-lockout").await;
    fixture
        .grant_engineer(&[
            NodePermission::NacRelationAdd,
            NodePermission::NacRelationDelete,
            NodePermission::NacStatus,
            NodePermission::CollectionGet,
        ])
        .await;
    let tools = fixture.tools(&["policy"], true);
    let tool = config(&tools);
    let self_revoke = |permission: &str| {
        json!({"argv": ["policy", "revoke", "--permission", permission]})
    };

    // The Agent's baseline read grant is kept against its own revoke.
    let baseline = run(tool, self_revoke("read-document"))
        .await
        .expect_err("self_config_no_lockout keeps the baseline read grant");
    assert!(
        baseline["error"]
            .as_str()
            .unwrap()
            .contains("self_config_no_lockout keeps node permission read-document on this agent"),
        "{baseline}"
    );

    // The policy delegation grant is also kept against its own revoke.
    let delegation = run(tool, self_revoke("add-nac-relation"))
        .await
        .expect_err("self_config_no_lockout keeps the delegation grant");
    assert!(
        delegation["error"]
            .as_str()
            .unwrap()
            .contains("self_config_no_lockout keeps node permission add-nac-relation"),
        "{delegation}"
    );

    // A permission outside the protected sets may still be revoked from self.
    let receipt = run(tool, self_revoke("get-collection"))
        .await
        .expect("an unprotected permission may be revoked from self");
    assert_eq!(
        receipt["answer"].as_str().unwrap(),
        "Revoked node permission get-collection from you."
    );
}
