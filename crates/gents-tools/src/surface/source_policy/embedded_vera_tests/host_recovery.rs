//! Recovery of a client-signed Agent creation interrupted around its `host`
//! grant on a Vera home (`Identity.ClientAgentJournal.recoveryDisposition`),
//! driven through the client's real publication journal, its retained
//! request-bound delegation and the runtime's host grant route.

use std::sync::Arc;

use anyhow::Context as _;
use gents_store::client_journal::records::publication::{ensure_recovered, Observer};
use gents_store::config_client::host_grant::HostCredential;
use gents_store::config_client::{AgentPublicationObserver, ConfigAccess, ConfigRelationshipGrant};
use gents_store::local::document_acp::DocumentAcp;

use crate::test_support::vera_test_home::Household;

/// Where the client stopped.
#[derive(Clone, Copy)]
enum Stop {
    /// After the `executor` grant, before the `host` attempt was recorded.
    AfterExecutor,
    /// After the `host` grant was recorded and sent, before its outcome was.
    AfterHostApplied,
    /// After the `host` attempt was recorded, before it reached the runtime.
    BeforeHostSent,
}

struct Interrupted {
    household: Household,
    journal: Arc<defra_node::Node>,
    user: ConfigAccess,
    agent_did: String,
    agent_doc: String,
}

impl Interrupted {
    /// A User's Agent creation whose client stopped at `stop`, its `host`
    /// grant's delegation lapsed when `lapsed`.
    async fn at(stop: Stop, lapsed: bool) -> anyhow::Result<Self> {
        let household = Household::open(DocumentAcp::Vera).await?;
        let user = household.as_user()?;
        let user_did = household.user_did.to_string();
        let node_did = household.node_did.to_string();
        let agent = household.derive_agent()?;
        let input = serde_json::json!({"input": {
            "node_did": node_did, "agent_did": agent.agent_did,
            "creation_nonce": agent.creation_nonce, "inference_profile_id": "fixture",
            "enabled": true,
        }});
        let agent_doc = user
            .transact_write("test.host_recovery.agent", |txn| {
                let input = &input;
                Box::pin(async move {
                    let created = txn
                        .execute_client_signed_create(
                            "mutation($input: AgentMutationInputArg!) { create_Agent(input: $input) { _docID } }",
                            input,
                            gents_protocol::schemas::AGENT,
                        )
                        .await?;
                    Ok::<_, anyhow::Error>(gents_store::graphql::created_doc_id(&created, "Agent")?)
                })
            })
            .await?;
        let journal = Arc::new(
            defra_node::Node::builder()
                .with_node_identity_did(household.user.did())
                .build()
                .await?,
        );
        gents_store::schema::ensure_runtime_schemas(&journal).await?;
        let observer = Observer::new(
            journal.clone(),
            user_did.clone(),
            vec![(agent.agent_did.clone(), node_did.clone())],
        );
        let grants = [
            ConfigRelationshipGrant::Resource {
                collection: "Agent".into(),
                doc_id: agent_doc.clone(),
                relation: "parent".into(),
                target_resource: "nodes".into(),
                target_doc_id: household.node_doc.clone(),
            },
            ConfigRelationshipGrant::Actor {
                collection: "Agent".into(),
                doc_id: agent_doc.clone(),
                relation: "executor".into(),
                target: agent.agent_did.clone(),
            },
            ConfigRelationshipGrant::Actor {
                collection: "Agent".into(),
                doc_id: agent_doc.clone(),
                relation: "host".into(),
                target: node_did.clone(),
            },
        ];
        let credential = user
            .host_credential(&agent_doc, &node_did)
            .await?
            .context("a Vera home mints a request-bound host delegation")?;
        let credential = if lapsed {
            credential.lapsed_for_test()
        } else {
            credential
        };
        observer
            .before_commit(
                &user_did,
                &grants,
                &[(grants[2].clone(), credential.clone())],
            )
            .await?;
        observer.committed().await?;
        for grant in &grants[..2] {
            observer.before_grant(grant).await?;
            user.complete_relationship_grants(&user_did, std::slice::from_ref(grant))
                .await?;
            observer.granted(grant).await?;
        }
        match stop {
            Stop::AfterExecutor => {}
            Stop::BeforeHostSent => observer.before_grant(&grants[2]).await?,
            Stop::AfterHostApplied => {
                observer.before_grant(&grants[2]).await?;
                user.grant_host_bound(&agent_doc, &node_did, &credential)
                    .await?;
            }
        }
        household
            .settled(&household.user_did, "the interrupted creation's grants")
            .await?;
        Ok(Self {
            household,
            journal,
            user,
            agent_did: agent.agent_did,
            agent_doc,
        })
    }

    async fn recover(&self) -> anyhow::Result<()> {
        ensure_recovered(
            &self.journal,
            &self.user,
            self.household.user_did.as_str(),
            self.household.node_did.as_str(),
            &self.agent_did,
        )
        .await
    }

    /// The Nodes the Agent's `host` names, once nothing is owed.
    async fn hosts(&self) -> anyhow::Result<Vec<String>> {
        self.household
            .settled(&self.household.user_did, "the host grant")
            .await?;
        Ok(self
            .user
            .list_document_relationships("Agent", &self.agent_doc)
            .await?
            .into_iter()
            .filter(|relationship| relationship.relation == "host")
            .map(|relationship| relationship.subject)
            .collect())
    }

    async fn shutdown(self) -> anyhow::Result<()> {
        self.journal.shutdown().await?;
        self.household.shutdown().await
    }
}

#[tokio::test]
async fn a_host_grant_never_sent_is_sent_on_recovery() -> anyhow::Result<()> {
    let interrupted = Interrupted::at(Stop::AfterExecutor, false).await?;
    assert!(interrupted.hosts().await?.is_empty());
    interrupted.recover().await?;
    let node = interrupted.household.node_did.to_string();
    assert_eq!(interrupted.hosts().await?, [node.clone()]);
    interrupted.recover().await?;
    assert_eq!(interrupted.hosts().await?, [node]);
    interrupted.shutdown().await
}

#[tokio::test]
async fn an_applied_host_grant_resent_under_its_delegation_takes_effect_once() -> anyhow::Result<()>
{
    let interrupted = Interrupted::at(Stop::AfterHostApplied, false).await?;
    let node = interrupted.household.node_did.to_string();
    assert_eq!(interrupted.hosts().await?, [node.clone()]);
    interrupted.recover().await?;
    assert_eq!(interrupted.hosts().await?, [node]);
    interrupted.shutdown().await
}

#[tokio::test]
async fn a_host_grant_withdrawn_before_its_resend_is_not_revived() -> anyhow::Result<()> {
    let interrupted = Interrupted::at(Stop::AfterHostApplied, false).await?;
    interrupted
        .user
        .delete_actor_relationship(
            "Agent",
            &interrupted.agent_doc,
            "host",
            &interrupted.household.node_did,
        )
        .await?;
    assert!(interrupted.hosts().await?.is_empty());
    interrupted.recover().await?;
    assert!(interrupted.hosts().await?.is_empty());
    interrupted.shutdown().await
}

#[tokio::test]
async fn a_lapsed_unconfirmed_host_grant_is_reported_and_not_granted() -> anyhow::Result<()> {
    let interrupted = Interrupted::at(Stop::BeforeHostSent, true).await?;
    let error = interrupted
        .recover()
        .await
        .expect_err("an Agent placed nowhere is reported");
    let message = error.to_string();
    assert!(
        message.contains("is not placed on any device") && message.contains(&interrupted.agent_doc),
        "{message}"
    );
    assert!(interrupted.hosts().await?.is_empty());
    interrupted.shutdown().await
}

#[tokio::test]
async fn a_lapsed_host_grant_found_on_this_node_succeeded() -> anyhow::Result<()> {
    let interrupted = Interrupted::at(Stop::AfterHostApplied, true).await?;
    interrupted.recover().await?;
    interrupted.recover().await?;
    assert_eq!(
        interrupted.hosts().await?,
        [interrupted.household.node_did.to_string()]
    );
    interrupted.shutdown().await
}

#[tokio::test]
async fn a_lapsed_host_grant_the_owner_moved_is_superseded() -> anyhow::Result<()> {
    let interrupted = Interrupted::at(Stop::BeforeHostSent, true).await?;
    let elsewhere = interrupted.household.held_key("elsewhere.key").await?;
    let elsewhere =
        identity::Did::new(gents_store::identity::NodeIdentity::did(elsewhere.as_ref()))?;
    interrupted
        .user
        .add_actor_relationship("Agent", &interrupted.agent_doc, "host", &elsewhere)
        .await?;
    interrupted.recover().await?;
    interrupted.recover().await?;
    assert_eq!(interrupted.hosts().await?, [elsewhere.to_string()]);
    interrupted.shutdown().await
}

#[test]
fn a_retained_host_credential_lapses_at_its_expiry() {
    let credential: HostCredential =
        serde_json::from_value(serde_json::json!({"token": "bound", "expires_at": 10}))
            .expect("credential");
    assert!(credential.holds_at(9));
    assert!(!credential.lapsed_for_test().holds_at(9));
}
