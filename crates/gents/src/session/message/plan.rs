use anyhow::{Context, Result};
use defra_node::EmbeddedNode;

use std::sync::Arc;

use crate::request::lifecycle::{SessionMessageCause, SessionMessageTarget};
use crate::store::config_client::ConfigAccess;

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
    hop: u32,
    pub(super) steer_head: Option<Option<String>>,
    pub(super) interrupt_before_publish: bool,
    pub(super) write: PlannedWrite,
}

impl Plan {
    pub(crate) async fn sign_agent_creation(&mut self, node_did: &str) -> Result<()> {
        let create = match &mut self.write {
            PlannedWrite::Request { create, .. } | PlannedWrite::Goal { create, .. } => create,
        };
        create.input.session_creation =
            Some(gents_protocol::request_input::SessionCreationIntent::AgentNew);
        let signer =
            crate::store::identity::RegisteredIdentity::from_registered_did(node_did, None)?;
        crate::sign_agent_request_create(&signer, create).await
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

async fn target_goal_assignment(
    node: &Arc<EmbeddedNode>,
    cause: &SessionMessageCause,
    target: &SessionMessageTarget,
) -> Result<Option<gents_protocol::request_input::GoalAssignmentInput>> {
    use gents_protocol::request_input::GoalAssignmentInput;
    if target.node_did == cause.caller_node_did {
        return ConfigAccess::transact_local_readonly(
            node,
            None,
            "session_message.target_goal",
            |txn| {
                Box::pin(async move {
                    let Some(session) = crate::session::load_agent_session_row_in_txn(
                        txn,
                        &target.node_did,
                        &target.session_id,
                        Some(&cause.caller_node_did),
                    )
                    .await?
                    else {
                        return Ok(None);
                    };
                    anyhow::ensure!(
                        session.session.agent_id == target.agent_id,
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
        .await;
    }
    let verifier = crate::store::identity::RegisteredIdentity::from_registered_did(
        cause.caller_node_did.clone(),
        None,
    )?;
    let state = crate::goal::load_session_goal_observation_state(
        &ConfigAccess::local(node.clone()),
        &verifier,
        &target.node_did,
        &target.session_id,
        Some(&cause.caller_node_did),
    )
    .await?;
    match state {
        crate::goal::SessionGoalObservationState::NoReadableSession => Ok(None),
        crate::goal::SessionGoalObservationState::Unknown => {
            anyhow::bail!("The target chat's Goal state is still syncing. Read the chat and retry the message.")
        }
        crate::goal::SessionGoalObservationState::Observed(observation) => {
            Ok(observation.goal.and_then(|goal| {
                goal.assignment_root_request_doc_id
                    .map(|root| GoalAssignmentInput {
                        goal_id: goal.snapshot.goal_id,
                        root_request_doc_id: Some(root),
                    })
            }))
        }
    }
}

#[cfg(test)]
pub(crate) async fn plan(
    node: &Arc<EmbeddedNode>,
    cause: &SessionMessageCause,
    target: &SessionMessageTarget,
    rendered: RenderedBody,
    title: Option<&str>,
    interrupt: bool,
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
) -> Result<Result<Plan, String>> {
    let request_id = uuid::Uuid::new_v4().to_string();
    let local = target.node_did == cause.caller_node_did;
    if !local && delivery == AgentMessageDelivery::Steer {
        return Ok(Err(
            "steering requires a local target session; use delivery=queue".to_owned(),
        ));
    }
    if crate::session::session_is_stopped(
        node,
        &target.node_did,
        &target.session_id,
        Some(&cause.caller_node_did),
    )
    .await?
    {
        return Ok(Err(
            "session is stopped; use agent_resume before sending another message".to_owned(),
        ));
    }
    let active = if local {
        match crate::request::interrupt::active_session_request(
            node,
            &target.session_id,
            &target.node_did,
            Some(&cause.caller_node_did),
        )
        .await?
        {
            Some(active) => {
                let active_doc_id = active
                    .doc_id
                    .as_deref()
                    .context("active session request lacks physical identity")?;
                Some(
                    crate::request::binding::load_agent_request_by_doc_id(node, active_doc_id)
                        .await?
                        .context("active session request disappeared")?,
                )
            }
            None => None,
        }
    } else {
        None
    };
    let steer_head = (delivery == AgentMessageDelivery::Steer)
        .then(|| active.as_ref().map(|request| request.doc_id.clone()));
    let own_hop = match &active {
        Some(active) => active.request_hop.max(
            crate::session::load_session_current_hop(node, &target.node_did, &target.session_id)
                .await?,
        ),
        None => {
            crate::session::load_session_current_hop(node, &target.node_did, &target.session_id)
                .await?
        }
    };
    let hop = crate::request::lifecycle::next_request_hop(
        crate::request::lifecycle::RequestHopCause::CrossSession {
            cause_hop: cause.caller_hop,
        },
        own_hop,
    );
    let write = if let Some((objective, token_budget)) = rendered.goal {
        if !local {
            return Ok(Err(
                "a Task that declares a Goal can only address this principal's own sessions"
                    .to_owned(),
            ));
        }
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
