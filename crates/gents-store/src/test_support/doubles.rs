use std::collections::{BTreeSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use futures::future::BoxFuture;
use gents_protocol::delegation_audience::{DelegationAudience, ReportedDelegationAudience};
use gents_protocol::document_query::Collection;
use gents_protocol::home_attestation::{
    AttestedAudience, HomeAttestation, HomeNonce, HomeStatement, HOME_ATTESTATION_VERSION,
    STATUS_HOME_ATTESTATION_FIELD,
};
use tokio::sync::Notify;

use crate::config_client::assignment_receiver::AssignmentRuntime;
use crate::config_client::{
    AgentPublicationObserver, ConfigClientError, ConfigRelationshipGrant, ResolveAudience,
};
use crate::identity::{
    EnrollmentGrantTuple, KeyIdentity, NativeTuples, NodeIdentity, TupleChanges,
};

pub(crate) struct PanicPublicationObserver;

#[async_trait::async_trait]
impl AgentPublicationObserver for PanicPublicationObserver {
    async fn before_commit(
        &self,
        _: &str,
        _: &[ConfigRelationshipGrant],
    ) -> Result<(), ConfigClientError> {
        panic!("observer must not run")
    }

    async fn committed(&self) -> Result<(), ConfigClientError> {
        panic!("observer must not run")
    }

    async fn commit_failed(&self, _: bool) -> Result<(), ConfigClientError> {
        panic!("observer must not run")
    }

    async fn before_grant(&self, _: &ConfigRelationshipGrant) -> Result<(), ConfigClientError> {
        panic!("observer must not run")
    }

    async fn granted(&self, _: &ConfigRelationshipGrant) -> Result<(), ConfigClientError> {
        panic!("observer must not run")
    }
}

pub(crate) fn test_delegation_audience(deployment: u64) -> DelegationAudience {
    DelegationAudience {
        worker: "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK".into(),
        deployment,
        genesis_id: None,
    }
}

pub(crate) struct ScriptedAudienceAnswer {
    pub(crate) audience: u64,
    pub(crate) provider_time: Option<u64>,
    pub(crate) gate: Option<Arc<Notify>>,
}

pub(crate) struct ScriptedAudienceResolver {
    pub(crate) node: KeyIdentity,
    pub(crate) answers: Mutex<VecDeque<ScriptedAudienceAnswer>>,
    pub(crate) entered: Notify,
    pub(crate) reads: AtomicUsize,
}

impl ResolveAudience for ScriptedAudienceResolver {
    fn resolve<'a>(&'a self, nonce: &'a HomeNonce) -> BoxFuture<'a, Option<serde_json::Value>> {
        let answer = self
            .answers
            .lock()
            .unwrap()
            .pop_front()
            .expect("a scripted answer for every read");
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.entered.notify_one();
        Box::pin(async move {
            if let Some(gate) = &answer.gate {
                gate.notified().await;
            }
            Some(
                super::signed_home_status(
                    &self.node,
                    &ReportedDelegationAudience::Named(test_delegation_audience(answer.audience)),
                    answer.provider_time,
                    nonce,
                )
                .await,
            )
        })
    }
}

pub(crate) struct SignedStatusResolver {
    pub(crate) node: KeyIdentity,
    pub(crate) reports: ReportedDelegationAudience,
    pub(crate) provider_time: Option<u64>,
    pub(crate) reads: AtomicUsize,
}

impl ResolveAudience for SignedStatusResolver {
    fn resolve<'a>(&'a self, nonce: &'a HomeNonce) -> BoxFuture<'a, Option<serde_json::Value>> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            Some(
                super::signed_home_status(&self.node, &self.reports, self.provider_time, nonce)
                    .await,
            )
        })
    }
}

pub(crate) struct ContradictoryStatusResolver {
    pub(crate) node: KeyIdentity,
    pub(crate) reads: AtomicUsize,
}

impl ResolveAudience for ContradictoryStatusResolver {
    fn resolve<'a>(&'a self, nonce: &'a HomeNonce) -> BoxFuture<'a, Option<serde_json::Value>> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            let statement = HomeStatement {
                version: HOME_ATTESTATION_VERSION,
                nonce: nonce.as_str().to_owned(),
                node_did: self.node.did().to_owned(),
                client_authentication:
                    gents_protocol::client_authentication::ClientAuthentication::Vera {
                        publisher_node: None,
                        worker: test_delegation_audience(2).worker,
                        deployment: 2,
                    },
                delegation_audience: AttestedAudience::Named {
                    audience: DelegationAudience {
                        genesis_id: Some("0x03".into()),
                        ..test_delegation_audience(3)
                    },
                },
                provider_time: Some(1_000),
            };
            let signature = self.node.sign(&statement.signing_payload()).await.unwrap();
            Some(serde_json::json!({
                "node_did": self.node.did(),
                STATUS_HOME_ATTESTATION_FIELD: HomeAttestation::signed(statement, &signature),
            }))
        })
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) enum AudienceForgery {
    Unsigned,
    OtherKey,
    Replayed,
}

pub(crate) struct ImpostorStatusResolver {
    pub(crate) home: KeyIdentity,
    pub(crate) other: KeyIdentity,
    pub(crate) forgery: AudienceForgery,
    pub(crate) reports: DelegationAudience,
    pub(crate) reads: AtomicUsize,
}

impl ResolveAudience for ImpostorStatusResolver {
    fn resolve<'a>(&'a self, nonce: &'a HomeNonce) -> BoxFuture<'a, Option<serde_json::Value>> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        Box::pin(async move {
            let mut status = serde_json::json!({
                "node_did": self.home.did(),
                "client_authentication": {
                    "kind": "vera",
                    "worker": self.reports.worker,
                    "deployment": self.reports.deployment,
                },
                "delegation_audience": self.reports,
            });
            match self.forgery {
                AudienceForgery::Unsigned => {}
                AudienceForgery::OtherKey => {
                    crate::identity::home_status::attest(Some(&self.other), nonce, &mut status)
                        .await;
                }
                AudienceForgery::Replayed => {
                    let earlier = HomeNonce::from_bytes([9; 32]);
                    crate::identity::home_status::attest(Some(&self.home), &earlier, &mut status)
                        .await;
                }
            }
            Some(status)
        })
    }
}

pub(crate) enum FilterVariableProbe {}

impl Collection for FilterVariableProbe {
    const NAME: &'static str = "FilterVariableProbe";
}

pub(crate) struct FailingTupleChanges<'a> {
    pub(crate) inner: NativeTuples<'a>,
    pub(crate) fail_at: usize,
    pub(crate) issues: AtomicUsize,
}

impl TupleChanges for FailingTupleChanges<'_> {
    async fn issue(&self, tuple: EnrollmentGrantTuple) -> Result<(), ConfigClientError> {
        if self.issues.fetch_add(1, Ordering::SeqCst) == self.fail_at {
            return Err(ConfigClientError::RelationshipActorRequired);
        }
        self.inner.issue(tuple).await
    }

    async fn remove(&self, tuple: EnrollmentGrantTuple) -> Result<(), ConfigClientError> {
        self.inner.remove(tuple).await
    }
}

pub(crate) struct InMemoryTuples {
    held: Mutex<Vec<EnrollmentGrantTuple>>,
    issues: AtomicUsize,
    fail_at: Option<usize>,
    removals_fail: bool,
}

impl InMemoryTuples {
    pub(crate) fn holding(
        held: &[EnrollmentGrantTuple],
        fail_at: Option<usize>,
        removals_fail: bool,
    ) -> Self {
        Self {
            held: Mutex::new(held.to_vec()),
            issues: AtomicUsize::new(0),
            fail_at,
            removals_fail,
        }
    }

    pub(crate) fn held(&self) -> Vec<EnrollmentGrantTuple> {
        let held = self.held.lock().unwrap();
        EnrollmentGrantTuple::derived()
            .into_iter()
            .filter(|tuple| held.contains(tuple))
            .collect()
    }
}

impl TupleChanges for InMemoryTuples {
    async fn issue(&self, tuple: EnrollmentGrantTuple) -> Result<(), ConfigClientError> {
        let call = self.issues.fetch_add(1, Ordering::SeqCst);
        if self.fail_at == Some(call) {
            return Err(ConfigClientError::RelationshipActorRequired);
        }
        let mut held = self.held.lock().unwrap();
        if !held.contains(&tuple) {
            held.push(tuple);
        }
        Ok(())
    }

    async fn remove(&self, tuple: EnrollmentGrantTuple) -> Result<(), ConfigClientError> {
        if self.removals_fail {
            return Err(ConfigClientError::RelationshipActorRequired);
        }
        self.held.lock().unwrap().retain(|held| *held != tuple);
        Ok(())
    }
}

#[derive(Default)]
pub(crate) struct AssignmentTestRuntime {
    pub(crate) active: tokio::sync::Mutex<BTreeSet<String>>,
    pub(crate) bodies: tokio::sync::Mutex<BTreeSet<String>>,
    pub(crate) fail_stop: AtomicBool,
    pub(crate) block_after_stop: AtomicBool,
    pub(crate) stopped: Notify,
    pub(crate) block_active: AtomicBool,
    pub(crate) active_entered: Notify,
    pub(crate) changed: Notify,
}

#[async_trait::async_trait]
impl AssignmentRuntime for AssignmentTestRuntime {
    async fn active(&self) -> Result<Vec<String>, Box<dyn std::error::Error + Send + Sync>> {
        if self.block_active.load(Ordering::SeqCst) {
            self.active_entered.notify_one();
            std::future::pending::<()>().await;
        }
        Ok(self.active.lock().await.iter().cloned().collect())
    }

    async fn stop(&self, id: &str) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
        if self.fail_stop.load(Ordering::SeqCst) {
            return Err(std::io::Error::other("stop failed").into());
        }
        self.active.lock().await.remove(id);
        if self.block_after_stop.load(Ordering::SeqCst) {
            self.stopped.notify_one();
            std::future::pending::<()>().await;
        }
        self.changed.notify_one();
        Ok(())
    }

    async fn has_verified_body(
        &self,
        id: &str,
    ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
        Ok(self.bodies.lock().await.contains(id))
    }

    async fn start_if_verified(
        &self,
        id: &str,
    ) -> Result<bool, Box<dyn std::error::Error + Send + Sync>> {
        if !self.bodies.lock().await.contains(id) {
            return Ok(false);
        }
        self.active.lock().await.insert(id.to_owned());
        self.changed.notify_one();
        Ok(true)
    }
}
