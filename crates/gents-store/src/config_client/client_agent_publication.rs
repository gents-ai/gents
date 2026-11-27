use serde::{Deserialize, Serialize};

use super::host_grant::HostCredential;
use super::{ConfigClientError, ConfigRelationshipGrant};

/// Durable observations for one configuration transaction's Agent grants.
/// Implementations record attempts before allowing the transaction owner to
/// dispatch; an interrupted attempt must remain unknown after restart.
#[async_trait::async_trait]
pub trait AgentPublicationObserver: Send + Sync {
    /// `host_credentials` pairs each `host` grant that will be sent under a
    /// request-bound delegation with that delegation.
    async fn before_commit(
        &self,
        creator: &str,
        grants: &[ConfigRelationshipGrant],
        host_credentials: &[(ConfigRelationshipGrant, HostCredential)],
    ) -> Result<(), ConfigClientError>;
    /// The delegation `grant` is sent under, retained by `before_commit`.
    async fn host_credential(&self, grant: &ConfigRelationshipGrant) -> Option<HostCredential>;
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
    Host,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicationStatus {
    Fresh,
    Pending,
    Succeeded,
    Failed,
    /// A `host` grant the owner replaced by placing the Agent on another Node
    /// before the client learned its own grant's outcome.
    Superseded,
}

/// What a read of an Agent's `host` relationships shows
/// (`Identity.ClientAgentJournal.HostReading`).
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum HostReading {
    ThisNode,
    OtherNode,
    Absent,
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
    pub host: PublicationStatus,
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
            host: PublicationStatus::Fresh,
            permit: None,
        }
    }

    pub fn status(&self, stage: PublicationStage) -> PublicationStatus {
        match stage {
            PublicationStage::Commit => self.commit,
            PublicationStage::Parent => self.parent,
            PublicationStage::Executor => self.executor,
            PublicationStage::Host => self.host,
        }
    }

    fn set_status(&mut self, stage: PublicationStage, status: PublicationStatus) {
        match stage {
            PublicationStage::Commit => self.commit = status,
            PublicationStage::Parent => self.parent = status,
            PublicationStage::Executor => self.executor = status,
            PublicationStage::Host => self.host = status,
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
                        && self.host == PublicationStatus::Fresh
                }
                PublicationStage::Parent => self.commit == PublicationStatus::Succeeded,
                PublicationStage::Executor => {
                    self.commit == PublicationStatus::Succeeded
                        && self.parent == PublicationStatus::Succeeded
                }
                PublicationStage::Host => {
                    self.commit == PublicationStatus::Succeeded
                        && self.parent == PublicationStatus::Succeeded
                        && self.executor == PublicationStatus::Succeeded
                }
            }
    }

    /// `Identity.ClientAgentJournal.hostOwed`.
    pub fn host_owed(&self) -> bool {
        [self.commit, self.parent, self.executor]
            .iter()
            .all(|status| *status == PublicationStatus::Succeeded)
    }

    /// `Identity.ClientAgentJournal.complete`: every stage, `host` included,
    /// succeeded.
    pub fn complete(&self) -> bool {
        self.host_owed() && self.host == PublicationStatus::Succeeded
    }

    /// `Identity.ClientAgentJournal.settled`.
    pub fn settled(&self) -> bool {
        self.complete() || (self.host_owed() && self.host == PublicationStatus::Superseded)
    }

    /// `Identity.ClientAgentJournal.readsHost`.
    pub fn reads_host(&self, token_valid: bool) -> bool {
        self.host_owed() && self.host == PublicationStatus::Pending && !token_valid
    }

    /// `Identity.ClientAgentJournal.resumeAttempt`.
    pub fn resume_attempt(&mut self, token_valid: bool) {
        if token_valid
            && self.host_owed()
            && self.permit.is_none()
            && self.host == PublicationStatus::Pending
        {
            self.permit = Some(PublicationStage::Host);
        }
    }

    /// `Identity.ClientAgentJournal.observeHost`.
    pub fn observe_host(&mut self, reading: HostReading) {
        if self.host_owed() && self.permit.is_none() && self.host == PublicationStatus::Pending {
            match reading {
                HostReading::ThisNode => self.host = PublicationStatus::Succeeded,
                HostReading::OtherNode => self.host = PublicationStatus::Superseded,
                HostReading::Absent => {}
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

/// What a client may do with a retained publication before it publishes again
/// (`Identity.ClientAgentJournal.recoveryDisposition`). An unfinished `parent`
/// or `executor` is never resumed automatically; an unfinished `host` is sent,
/// or settled by reading the Agent's `host` relationships.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryDisposition {
    Proceed,
    BindingMismatch,
    CommitUnknown,
    ExplicitRepair,
    ResendHost,
    ObservedHost,
    SupersededHost,
    AbsentHost,
}

pub fn recovery_disposition(
    actor_matches: bool,
    retained: Option<&ClientAgentJournal>,
    binding_matches: bool,
    token_valid: bool,
    reading: HostReading,
) -> RecoveryDisposition {
    if !actor_matches {
        return RecoveryDisposition::BindingMismatch;
    }
    let Some(journal) = retained else {
        return RecoveryDisposition::Proceed;
    };
    if !binding_matches {
        return RecoveryDisposition::BindingMismatch;
    }
    if journal.commit == PublicationStatus::Failed || journal.settled() {
        return RecoveryDisposition::Proceed;
    }
    if journal.commit != PublicationStatus::Succeeded {
        return RecoveryDisposition::CommitUnknown;
    }
    if !journal.host_owed() {
        return RecoveryDisposition::ExplicitRepair;
    }
    if journal.reads_host(token_valid) {
        return match reading {
            HostReading::ThisNode => RecoveryDisposition::ObservedHost,
            HostReading::OtherNode => RecoveryDisposition::SupersededHost,
            HostReading::Absent => RecoveryDisposition::AbsentHost,
        };
    }
    RecoveryDisposition::ResendHost
}
