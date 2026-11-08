use anyhow::{Context, Result};
use defra_node::Node as EmbeddedNode;

use std::sync::Arc;

use crate::store::config_client::ConfigAccess;
use gents_protocol::request_lifecycle::{SessionMessageCause, SessionMessageTarget};

use super::{AgentMessageDelivery, Delivery, RenderedBody};

pub(super) enum PlannedWrite {
    Request {
        create: gents_protocol::request_admission::AgentRequestCreate,
        delivery: Delivery,
    },
    Goal {
        create: gents_protocol::request_admission::AgentRequestCreate,
        objective: String,
        token_budget: Option<i64>,
    },
}

pub(crate) struct Plan {
    pub(super) session_id: String,
    pub(super) hop: u32,
    pub(super) steer_head: Option<Option<String>>,
    pub(super) interrupt_before_publish: bool,
    pub(super) write: PlannedWrite,
}

impl Plan {
    pub(crate) async fn sign_agent_creation(
        &mut self,
        node_did: &str,
        parent: &gents_protocol::request_input::RequestInput,
        signer: &crate::store::identity::ManagedAgentSigner,
    ) -> Result<()> {
        let create = match &mut self.write {
            PlannedWrite::Request { create, .. } | PlannedWrite::Goal { create, .. } => create,
        };
        create.input.session_creation =
            Some(gents_protocol::request_input::SessionCreationIntent::AgentNew);
        create.input.cwd = parent.agent_new_cwd(create.node_did == node_did);
        anyhow::ensure!(
            signer.did() == create.admission.signer_did,
            "session creation signer differs from its admission actor"
        );
        crate::sign_agent_request_create(signer, create).await
    }
    pub(crate) fn hop(&self) -> u32 {
        self.hop
    }

    pub(crate) fn request(
        session_id: String,
        hop: u32,
        create: gents_protocol::request_admission::AgentRequestCreate,
    ) -> Self {
        Self {
            session_id,
            hop,
            steer_head: None,
            interrupt_before_publish: false,
            write: PlannedWrite::Request {
                create,
                delivery: Delivery::Request,
            },
        }
    }

    pub(crate) fn delivery(&self) -> Delivery {
        match self.write {
            PlannedWrite::Request { delivery, .. } => delivery,
            PlannedWrite::Goal { .. } => Delivery::Request,
        }
    }
}

pub(super) async fn target_goal_assignment(
    node: &Arc<EmbeddedNode>,
    cause: &SessionMessageCause,
    target: &SessionMessageTarget,
) -> Result<Option<gents_protocol::request_input::GoalAssignmentInput>> {
    use gents_protocol::request_input::GoalAssignmentInput;
    anyhow::ensure!(
        target.node_did == cause.caller_node_did,
        "cross-runtime agent delivery is not supported; the destination Goal is read only on its own runtime"
    );
    ConfigAccess::transact_local_readonly(
        node,
        Some(identity::Did::new(&cause.caller_agent_did)?),
        "session_message.target_goal",
        |txn| {
            Box::pin(async move {
                let Some(session) =
                    crate::store::config_client::agent_session::load_agent_session_row_in_txn(
                        txn,
                        &target.node_did,
                        &target.session_id,
                        Some(&cause.caller_agent_did),
                    )
                    .await?
                else {
                    return Ok(None);
                };
                anyhow::ensure!(
                    session.session.agent_did == target.agent_did,
                    "session-message target Agent differs from its canonical session"
                );
                let goal = crate::goal::load_canonical_goal_in_txn(
                    txn,
                    &target.node_did,
                    &target.session_id,
                )
                .await?;
                Ok(goal.and_then(|goal| {
                    goal.assignment_root_request_doc_id
                        .map(|root| GoalAssignmentInput {
                            goal_id: goal.goal_id,
                            root_request_doc_id: Some(root),
                        })
                }))
            })
        },
    )
    .await
}

#[cfg(test)]
pub(crate) async fn plan(
    node: &Arc<EmbeddedNode>,
    cause: &SessionMessageCause,
    target: &SessionMessageTarget,
    rendered: RenderedBody,
    title: Option<&str>,
    interrupt: bool,
    signer: &crate::store::identity::ManagedAgentSigner,
) -> Result<Result<Plan, String>> {
    plan_with_delivery(
        node,
        cause,
        target,
        rendered,
        title,
        if interrupt {
            AgentMessageDelivery::Interrupt
        } else {
            AgentMessageDelivery::Queue
        },
        signer,
    )
    .await
}

pub(crate) async fn plan_with_delivery(
    node: &Arc<EmbeddedNode>,
    cause: &SessionMessageCause,
    target: &SessionMessageTarget,
    rendered: RenderedBody,
    title: Option<&str>,
    delivery: AgentMessageDelivery,
    signer: &crate::store::identity::ManagedAgentSigner,
) -> Result<Result<Plan, String>> {
    let request_id = uuid::Uuid::new_v4().to_string();
    if target.node_did != cause.caller_node_did {
        return Ok(Err(
            "cross-runtime agent delivery requires the authenticated HTTP delivery path, which is not available yet".to_owned(),
        ));
    }
    if crate::session::session_is_stopped(
        node,
        &identity::Did::new(&cause.caller_agent_did)?,
        &target.node_did,
        &target.session_id,
        Some(&cause.caller_agent_did),
    )
    .await?
    {
        return Ok(Err(
            "session is stopped; use agent_resume before sending another message".to_owned(),
        ));
    }
    let access = ConfigAccess::local_as(node.clone(), identity::Did::new(&cause.caller_agent_did)?);
    let active = match crate::request::interrupt::active_session_request_with_access(
        &access,
        &target.session_id,
        &target.node_did,
        Some(&cause.caller_agent_did),
    )
    .await?
    {
        Some(active) => {
            let active_doc_id = active
                .doc_id
                .as_deref()
                .context("active session request lacks physical identity")?;
            Some(
                crate::request::binding::load_agent_request_by_doc_id_with_access(
                    &access,
                    active_doc_id,
                )
                .await?
                .context("active session request disappeared")?,
            )
        }
        None => None,
    };
    let steer_head = (delivery == AgentMessageDelivery::Steer)
        .then(|| active.as_ref().map(|request| request.doc_id.clone()));
    let own_hop = match &active {
        Some(active) => active.request_hop.max(
            crate::session::load_session_current_hop(
                node,
                &identity::Did::new(&cause.caller_agent_did)?,
                &target.node_did,
                &target.session_id,
            )
            .await?,
        ),
        None => {
            crate::session::load_session_current_hop(
                node,
                &identity::Did::new(&cause.caller_agent_did)?,
                &target.node_did,
                &target.session_id,
            )
            .await?
        }
    };
    let hop = gents_protocol::request_lifecycle::next_request_hop(
        gents_protocol::request_lifecycle::RequestHopCause::CrossSession {
            cause_hop: cause.caller_hop,
        },
        own_hop,
    );
    let write = if let Some((objective, token_budget)) = rendered.goal {
        if active.is_some() {
            return Ok(Err(
                "a Task that declares a Goal requires an idle session".to_owned()
            ));
        }
        let create = crate::request::lifecycle::build_session_message_request(
            cause,
            target,
            &rendered.content,
            title,
            &request_id,
            Some(format!("session-message:{}", cause.tool_call_doc_id)),
            hop,
            None,
            None,
            signer,
        )
        .await?;
        PlannedWrite::Goal {
            create,
            objective,
            token_budget,
        }
    } else {
        let steering = active.filter(|_| delivery != AgentMessageDelivery::Interrupt);
        let queue = steering
            .as_ref()
            .map(|active| gents_protocol::request_input::RequestQueue {
                delivery: if delivery == AgentMessageDelivery::Steer {
                    gents_protocol::request_input::QueueDelivery::Steer
                } else {
                    gents_protocol::request_input::QueueDelivery::Queue
                },
                position: None,
                source: gents_protocol::request_input::QueueSource::Steering,
                policy: gents_protocol::request_input::QueuePolicy::Append,
                key: None,
                queued_after_request_id: Some(active.request_id.clone()),
                interrupted_request_id: None,
                background_completion_wake_version: None,
            });
        PlannedWrite::Request {
            create:
                crate::request::lifecycle::materialize::build_session_message_request_with_input(
                    cause,
                    target,
                    &rendered.content,
                    title,
                    &request_id,
                    None,
                    hop,
                    gents_protocol::request_input::RequestInput {
                        queue,
                        goal_assignment: target_goal_assignment(node, cause, target).await?,
                        ..Default::default()
                    },
                    signer,
                )
                .await?,
            delivery: match (&steering, delivery) {
                (None, _) => Delivery::Request,
                (Some(_), AgentMessageDelivery::Steer) => Delivery::Steering,
                (Some(_), _) => Delivery::Queued,
            },
        }
    };
    Ok(Ok(Plan {
        session_id: target.session_id.clone(),
        hop,
        steer_head,
        interrupt_before_publish: delivery == AgentMessageDelivery::Interrupt,
        write,
    }))
}
