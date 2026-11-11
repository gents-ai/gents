use serde::{Deserialize, Serialize};

use super::{ConfigClientError, ConfigRelationshipGrant};

/// Durable observations for one configuration transaction's Agent grants.
/// Implementations record attempts before allowing the transaction owner to
/// dispatch; an interrupted attempt must remain unknown after restart.
#[async_trait::async_trait]
pub trait AgentPublicationObserver: Send + Sync {
    async fn before_commit(
        &self,
        creator: &str,
        grants: &[ConfigRelationshipGrant],
    ) -> Result<(), ConfigClientError>;
    async fn committed(&self) -> Result<(), ConfigClientError>;
    async fn commit_failed(&self, definitely_not_committed: bool) -> Result<(), ConfigClientError>;
    async fn before_grant(&self, grant: &ConfigRelationshipGrant) -> Result<(), ConfigClientError>;
    async fn granted(&self, grant: &ConfigRelationshipGrant) -> Result<(), ConfigClientError>;
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicationStage {
    Commit,
    Parent,
    Executor,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicationStatus {
    Fresh,
    Pending,
    Succeeded,
    Failed,
}

/// Publication observations bind exact documents and do not confer ACP authority.
/// A dispatch permit exists only in the process that durably recorded its attempt.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ClientAgentJournal {
    pub client: String,
    pub agent_did: String,
    pub node_did: String,
    pub agent_doc: String,
    pub node_doc: String,
    pub commit: PublicationStatus,
    pub parent: PublicationStatus,
    pub executor: PublicationStatus,
    #[serde(skip)]
    pub permit: Option<PublicationStage>,
}

impl ClientAgentJournal {
    pub fn new(
        client: String,
        agent_did: String,
        node_did: String,
        agent_doc: String,
        node_doc: String,
    ) -> Self {
        Self {
            client,
            agent_did,
            node_did,
            agent_doc,
            node_doc,
            commit: PublicationStatus::Fresh,
            parent: PublicationStatus::Fresh,
            executor: PublicationStatus::Fresh,
            permit: None,
        }
    }

    pub fn status(&self, stage: PublicationStage) -> PublicationStatus {
        match stage {
            PublicationStage::Commit => self.commit,
            PublicationStage::Parent => self.parent,
            PublicationStage::Executor => self.executor,
        }
    }

    fn set_status(&mut self, stage: PublicationStage, status: PublicationStatus) {
        match stage {
            PublicationStage::Commit => self.commit = status,
            PublicationStage::Parent => self.parent = status,
            PublicationStage::Executor => self.executor = status,
        }
    }

    pub fn may_record(&self, stage: PublicationStage) -> bool {
        let bound = [
            &self.client,
            &self.agent_did,
            &self.node_did,
            &self.agent_doc,
            &self.node_doc,
        ]
        .iter()
        .all(|value| !value.is_empty());
        bound
            && self.permit.is_none()
            && matches!(
                self.status(stage),
                PublicationStatus::Fresh | PublicationStatus::Failed
            )
            && match stage {
                PublicationStage::Commit => {
                    self.parent == PublicationStatus::Fresh
                        && self.executor == PublicationStatus::Fresh
                }
                PublicationStage::Parent => self.commit == PublicationStatus::Succeeded,
                PublicationStage::Executor => {
                    self.commit == PublicationStatus::Succeeded
                        && self.parent == PublicationStatus::Succeeded
                }
            }
    }

    pub fn record_attempt(&mut self, stage: PublicationStage, durable: bool) {
        if durable && self.may_record(stage) {
            self.set_status(stage, PublicationStatus::Pending);
            self.permit = Some(stage);
        }
    }

    pub fn may_dispatch(&self, stage: PublicationStage) -> bool {
        self.permit == Some(stage) && self.status(stage) == PublicationStatus::Pending
    }

    pub fn dispatch(&mut self, stage: PublicationStage) {
        if self.may_dispatch(stage) {
            self.permit = None;
        }
    }

    pub fn finish_attempt(
        &mut self,
        stage: PublicationStage,
        success: bool,
        confirmed_failure: bool,
    ) {
        if self.permit.is_none() && self.status(stage) == PublicationStatus::Pending {
            if success {
                self.set_status(stage, PublicationStatus::Succeeded);
            } else if confirmed_failure {
                self.set_status(stage, PublicationStatus::Failed);
            }
        }
    }

    pub fn restart(&mut self) {
        self.permit = None;
    }
}
