use std::sync::Arc;

use tokio::sync::Mutex;

use super::{
    durable, trigger_prompt_header, ActiveRuntimeSnapshot, FireIntent, FireResult, TriggerEngine,
    SERIAL_BUSY,
};
use crate::documents::automation::ResolvedSchedule;
use crate::documents::task_delivery::{validate_task_delivery_options, TriggerKind};
use crate::documents::template::TemplateScope;
use gents_protocol::trigger_delivery::{FireIdentity, TriggerFire};

struct PreparedDispatch {
    trigger_doc_id: Option<String>,
    rendered: String,
    rendered_goal_objective: Option<String>,
    delivery: Option<durable::PreparedFire>,
    prepared_ids: Option<(String, String)>,
}

#[derive(Clone, Copy)]
struct LockedFire<'a> {
    delivery_identity: &'a Option<FireIdentity>,
    node_did: &'a str,
    trigger_id: &'a str,
    delivery: &'a Option<durable::PreparedFire>,
    durable_goal_request_id: &'a Option<String>,
}

#[derive(Clone, Copy)]
struct IntentView<'a> {
    task: &'a crate::documents::automation::ResolvedTask,
    durable_fire_key: &'a str,
    group_vars: &'a Option<serde_json::Value>,
    correlation: &'a Option<String>,
    concurrency: crate::documents::ConcurrencyMode,
}

impl<'a> IntentView<'a> {
    fn of(intent: &'a FireIntent) -> Self {
        Self {
            task: &intent.task,
            durable_fire_key: &intent.durable_fire_key,
            group_vars: &intent.group_vars,
            correlation: &intent.correlation,
            concurrency: intent.concurrency,
        }
    }
}

fn reject(intent: FireIntent, result: FireResult) -> FireResult {
    (intent.on_result)(result.clone());
    result
}

impl TriggerEngine {
    pub(super) async fn dispatch(&self, intent: FireIntent) -> FireResult {
        if let Some(error) = intent.well_formed_error() {
            let result = FireResult::Rejected {
                error: error.to_string(),
            };
            (intent.on_result)(result.clone());
            return result;
        }

        if let Some(request_id) = intent.pre_materialized_request_id.clone() {
            let result = FireResult::Fired { request_id };
            (intent.on_result)(result.clone());
            return result;
        }

        let delivery_identity = self.delivery_identity(&intent);
        if let Err(result) = self
            .recover_prior_fire(IntentView::of(&intent), &delivery_identity)
            .await
        {
            return reject(intent, result);
        }

        let snapshot = self.snapshot_rx.borrow().clone();

        let PreparedDispatch {
            trigger_doc_id,
            rendered,
            rendered_goal_objective,
            delivery,
            prepared_ids,
        } = match Self::prepare(&intent, &snapshot) {
            Ok(prepared) => prepared,
            Err(result) => return reject(intent, result),
        };
        let durable_goal_request_id = match Self::durable_goal_request_id(
            &intent,
            &snapshot,
            &delivery,
            &rendered_goal_objective,
        ) {
            Ok(request_id) => request_id,
            Err(result) => return reject(intent, result),
        };
        let Some(trigger_id) = intent.trigger_id.clone() else {
            return self
                .materialize_after_lock(
                    intent,
                    trigger_doc_id,
                    rendered,
                    rendered_goal_objective,
                    delivery,
                    prepared_ids,
                )
                .await;
        };
        if intent.concurrency == crate::documents::ConcurrencyMode::Parallel
            && intent.group_vars.is_none()
        {
            return self
                .materialize_after_lock(
                    intent,
                    trigger_doc_id,
                    rendered,
                    rendered_goal_objective,
                    delivery,
                    prepared_ids,
                )
                .await;
        }
        let node_did = match Self::concurrency_node_did(&intent, &snapshot) {
            Ok(did) => did,
            Err(reason) => {
                let result = FireResult::Errored {
                    error: format!("concurrency gate: {reason}"),
                };
                return reject(intent, result);
            }
        };
        let lock_key = (
            node_did.clone(),
            trigger_id.clone(),
            intent
                .group_vars
                .as_ref()
                .map(|_| intent.durable_fire_key.clone()),
        );
        let lock = {
            let mut map = self.per_trigger_locks.lock().await;
            map.entry(lock_key.clone())
                .or_insert_with(|| Arc::new(Mutex::new(())))
                .clone()
        };
        let guard = lock.lock().await;

        let gated = self
            .gate_locked_fire(
                IntentView::of(&intent),
                LockedFire {
                    delivery_identity: &delivery_identity,
                    node_did: &node_did,
                    trigger_id: &trigger_id,
                    delivery: &delivery,
                    durable_goal_request_id: &durable_goal_request_id,
                },
            )
            .await;
        if let Err(result) = gated {
            drop(guard);
            self.prune_trigger_lock(&lock_key, &lock).await;
            return reject(intent, result);
        }

        let result = self
            .materialize_after_lock(
                intent,
                trigger_doc_id,
                rendered,
                rendered_goal_objective,
                delivery,
                prepared_ids,
            )
            .await;
        drop(guard);
        self.prune_trigger_lock(&lock_key, &lock).await;
        result
    }

    fn delivery_identity(&self, intent: &FireIntent) -> Option<FireIdentity> {
        let delivery_identity = if intent.trigger_kind == TriggerKind::Schedule {
            let snapshot = self.snapshot_rx.borrow().clone();
            intent.trigger_id.as_ref().map(|trigger| {
                gents_protocol::trigger_delivery::FireIdentity {
                    owner_did: snapshot.local_did.clone(),
                    trigger_id: trigger.clone(),
                    source_collection: "Schedule".into(),
                    source_doc_id: intent.durable_fire_key.clone(),
                }
            })
        } else if intent.trigger_kind == TriggerKind::Event {
            let snapshot = self.snapshot_rx.borrow().clone();
            let collection = intent
                .event_vars
                .get("source_collection")
                .and_then(serde_json::Value::as_str)
                .or_else(|| {
                    snapshot
                        .active_event_triggers()
                        .get(intent.trigger_id.as_deref().unwrap_or_default())
                        .map(|trigger| trigger.source_collection.as_str())
                });
            let (collection, document) = match &intent.group_vars {
                Some(group) => (
                    Some("EventGroupState"),
                    group
                        .get("state_doc_id")
                        .and_then(serde_json::Value::as_str),
                ),
                None => (
                    collection,
                    intent
                        .event_vars
                        .get("source_doc_id")
                        .and_then(serde_json::Value::as_str),
                ),
            };
            match (intent.trigger_id.as_ref(), collection, document) {
                (Some(trigger), Some(collection), Some(document)) => {
                    Some(gents_protocol::trigger_delivery::FireIdentity {
                        owner_did: snapshot.local_did.clone(),
                        trigger_id: trigger.clone(),
                        source_collection: collection.to_owned(),
                        source_doc_id: document.to_owned(),
                    })
                }
                _ => None,
            }
        } else {
            None
        };
        delivery_identity
    }

    async fn recover_prior_fire(
        &self,
        intent: IntentView<'_>,
        delivery_identity: &Option<FireIdentity>,
    ) -> Result<(), FireResult> {
        if let Some(identity) = &delivery_identity {
            match self
                .materializer
                .recover_admitted_fire(identity, intent.task)
                .await
            {
                Ok(Some(request_id)) => {
                    let result = FireResult::Duplicate { request_id };
                    return Err(result);
                }
                Ok(None) => {}
                Err(error) => {
                    let result = FireResult::Errored {
                        error: format!("recover admitted fire: {error}"),
                    };
                    return Err(result);
                }
            }
        }

        match self
            .materializer
            .recover_goal_task_fire(intent.task, intent.durable_fire_key)
            .await
        {
            Ok(Some(request_id)) => {
                let result = FireResult::Fired { request_id };
                return Err(result);
            }
            Ok(None) => {}
            Err(error) => {
                let result = FireResult::Errored {
                    error: format!("recover durable goal Task fire: {error}"),
                };
                return Err(result);
            }
        }
        Ok(())
    }

    fn prepare(
        intent: &FireIntent,
        snapshot: &ActiveRuntimeSnapshot,
    ) -> Result<PreparedDispatch, FireResult> {
        let trigger_doc_id = Self::trigger_doc_id(intent, snapshot)?;

        let now = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true);
        let (node_scope, ctx_scope) = crate::documents::template::task_node_ctx(
            &snapshot.local_did,
            &intent.task.agent_did,
            &now,
        );
        let schedule = (intent.trigger_kind == TriggerKind::Schedule)
            .then(|| {
                intent
                    .trigger_id
                    .as_deref()
                    .and_then(|id| snapshot.active_schedules().get(id))
            })
            .flatten();
        let targeted_schedule = schedule.is_some_and(|s| s.session_id_template.is_some());
        Self::validate_delivery_options(intent, schedule, targeted_schedule)?;
        let prepared_ids = Self::prepared_ids(intent, snapshot, targeted_schedule)?;
        let mut scope = crate::documents::template::TemplateScope {
            session: prepared_ids
                .as_ref()
                .map(|(_, session)| serde_json::json!({"session_id": session})),
            request: prepared_ids
                .as_ref()
                .map(|(request, _)| serde_json::json!({"request_id": request})),
            event: intent.event_vars.clone(),
            doc: intent.doc_vars.clone(),
            args: intent.args_vars.clone(),
            group: intent.group_vars.clone(),
            node: node_scope,
            ctx: ctx_scope,
        };
        if intent.trigger_kind == TriggerKind::Event
            && snapshot.agent(&intent.task.agent_did).is_none()
        {
            let result = FireResult::Errored {
                error: "prepare fire: trigger agent unavailable".into(),
            };
            return Err(result);
        }
        let mut delivery = Self::prepare_delivery(
            intent,
            snapshot,
            schedule,
            targeted_schedule,
            &mut scope,
            &now,
        )?;
        let (rendered, rendered_goal_objective) = match crate::documents::template::render_task(
            &intent.task.prompt_template,
            intent.task.goal_objective_template.as_deref(),
            intent.task.goal_token_budget,
            &scope,
        ) {
            Ok(rendered) => rendered,
            Err(error) => {
                let result = FireResult::Rejected { error };
                return Err(result);
            }
        };
        // An event prompt may interpolate any document its source matched,
        // replicated ones included. Manual prompts keep their existing bytes.
        let rendered = match intent.trigger_kind {
            TriggerKind::Manual => rendered,
            _ => format!("{}\n\n{rendered}", trigger_prompt_header(intent)),
        };
        if let Some(delivery) = &mut delivery {
            delivery.receipt.goal_objective = rendered_goal_objective.clone();
        }
        Ok(PreparedDispatch {
            trigger_doc_id,
            rendered,
            rendered_goal_objective,
            delivery,
            prepared_ids,
        })
    }

    fn trigger_doc_id(
        intent: &FireIntent,
        snapshot: &ActiveRuntimeSnapshot,
    ) -> Result<Option<String>, FireResult> {
        let trigger_doc_id = match intent.trigger_kind {
            TriggerKind::Schedule => {
                let Some(trigger_id) = intent.trigger_id.as_deref() else {
                    let result = FireResult::Rejected {
                        error: "Schedule trigger missing trigger_id".to_string(),
                    };
                    return Err(result);
                };
                let Some(trigger) = snapshot.active_schedules().get(trigger_id) else {
                    let result = FireResult::Skipped {
                        reason: "trigger disabled".to_string(),
                    };
                    return Err(result);
                };
                Some(trigger.trigger_doc_id.clone())
            }
            TriggerKind::Event => {
                let Some(trigger_id) = intent.trigger_id.as_deref() else {
                    let result = FireResult::Rejected {
                        error: "Event trigger missing trigger_id".to_string(),
                    };
                    return Err(result);
                };
                let Some(trigger) = snapshot.active_event_triggers().get(trigger_id) else {
                    let result = FireResult::Skipped {
                        reason: "trigger disabled".to_string(),
                    };
                    return Err(result);
                };
                Some(trigger.trigger_doc_id.clone())
            }
            TriggerKind::Manual => None,
        };
        Ok(trigger_doc_id)
    }

    fn validate_delivery_options(
        intent: &FireIntent,
        schedule: Option<&ResolvedSchedule>,
        targeted_schedule: bool,
    ) -> Result<(), FireResult> {
        if intent.trigger_kind != TriggerKind::Event {
            let validation = if let Some(schedule) = schedule {
                if schedule.task_id != intent.task.task_id {
                    Err(anyhow::anyhow!("schedule Task changed before admission"))
                } else {
                    validate_task_delivery_options(
                        TriggerKind::Schedule,
                        intent.task.emit_outcome,
                        intent.concurrency,
                        targeted_schedule,
                    )
                }
            } else {
                validate_task_delivery_options(
                    intent.trigger_kind,
                    intent.task.emit_outcome,
                    intent.concurrency,
                    false,
                )
            };
            if let Err(error) = validation {
                let result = FireResult::Rejected {
                    error: error.to_string(),
                };
                return Err(result);
            }
        }
        Ok(())
    }

    fn prepared_ids(
        intent: &FireIntent,
        snapshot: &ActiveRuntimeSnapshot,
        targeted_schedule: bool,
    ) -> Result<Option<(String, String)>, FireResult> {
        let prepared_ids = if matches!(
            intent.trigger_kind,
            TriggerKind::Manual | TriggerKind::Schedule
        ) && !targeted_schedule
        {
            Some(if intent.task.goal_objective_template.is_some() {
                let Some(agent) = snapshot.agent(&intent.task.agent_did) else {
                    let result = FireResult::Errored {
                        error: "Task agent unavailable".into(),
                    };
                    return Err(result);
                };
                let identity = crate::goal::task_goal_fire_identity(
                    agent.node_did(),
                    &intent.task.task_id,
                    &intent.durable_fire_key,
                );
                (identity.request_id, identity.session_id)
            } else {
                (
                    uuid::Uuid::new_v4().to_string(),
                    uuid::Uuid::new_v4().to_string(),
                )
            })
        } else {
            None
        };
        Ok(prepared_ids)
    }

    fn prepare_delivery(
        intent: &FireIntent,
        snapshot: &ActiveRuntimeSnapshot,
        schedule: Option<&ResolvedSchedule>,
        targeted_schedule: bool,
        scope: &mut TemplateScope,
        now: &str,
    ) -> Result<Option<durable::PreparedFire>, FireResult> {
        let delivery = if intent.trigger_kind == TriggerKind::Event || targeted_schedule {
            let prepared =
                Self::trigger_fire(intent, snapshot, schedule, targeted_schedule, scope, now);
            match prepared {
                Ok(fire) => {
                    scope.session = Some(serde_json::json!({"session_id": fire.session_id}));
                    scope.request = Some(serde_json::json!({"request_id": fire.request_id}));
                    let event_trigger = snapshot
                        .active_event_triggers()
                        .get(intent.trigger_id.as_deref().unwrap());
                    Some(durable::PreparedFire {
                        replace_prior: intent.concurrency
                            == crate::documents::ConcurrencyMode::LatestOnly,
                        target_existing: targeted_schedule
                            || event_trigger.is_some_and(|t| t.session_id_template.is_some()),
                        create_session: event_trigger.map_or_else(
                            || schedule.is_some_and(|s| s.create_session),
                            |t| t.create_session,
                        ),
                        max_fires_per_hour: event_trigger.map_or_else(
                            || {
                                schedule.map_or(crate::documents::DEFAULT_MAX_FIRES_PER_HOUR, |s| {
                                    s.max_fires_per_hour
                                })
                            },
                            |t| t.max_fires_per_hour,
                        ),
                        receipt: fire,
                        event_source_generation: intent.event_source_generation.clone(),
                    })
                }
                Err(error) => {
                    let result = FireResult::Rejected {
                        error: format!("prepare fire: {error}"),
                    };
                    return Err(result);
                }
            }
        } else {
            None
        };
        Ok(delivery)
    }

    fn trigger_fire(
        intent: &FireIntent,
        snapshot: &ActiveRuntimeSnapshot,
        schedule: Option<&ResolvedSchedule>,
        targeted_schedule: bool,
        scope: &TemplateScope,
        now: &str,
    ) -> anyhow::Result<TriggerFire> {
        let event_trigger = if intent.trigger_kind == TriggerKind::Event {
            Some(
                snapshot
                    .active_event_triggers()
                    .get(intent.trigger_id.as_deref().unwrap_or_default())
                    .ok_or_else(|| anyhow::anyhow!("event trigger disappeared"))?,
            )
        } else {
            None
        };
        let session_template = event_trigger
            .and_then(|t| t.session_id_template.as_deref())
            .or_else(|| schedule.and_then(|s| s.session_id_template.as_deref()));
        let owner = snapshot
            .agent(&intent.task.agent_did)
            .ok_or_else(|| anyhow::anyhow!("trigger agent unavailable"))?
            .node_did()
            .to_string();
        let (source_collection, source_doc_id) = if targeted_schedule {
            ("Schedule", Some(intent.durable_fire_key.as_str()))
        } else if let Some(group) = &intent.group_vars {
            (
                "EventGroupState",
                group
                    .get("state_doc_id")
                    .and_then(serde_json::Value::as_str),
            )
        } else {
            (
                event_trigger
                    .expect("event delivery")
                    .source_collection
                    .as_str(),
                intent
                    .event_vars
                    .get("source_doc_id")
                    .and_then(serde_json::Value::as_str),
            )
        };
        let source_doc_id = source_doc_id
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!("fire lacks source document ID"))?;
        let identity = gents_protocol::trigger_delivery::FireIdentity {
            owner_did: owner,
            trigger_id: intent.trigger_id.clone().expect("configured trigger"),
            source_collection: source_collection.into(),
            source_doc_id: source_doc_id.into(),
        };
        let key = durable::fire_key(&identity);
        let session_id = match session_template {
            Some(template) => {
                let value = crate::documents::template::render_template(template, scope)?;
                anyhow::ensure!(
                    !value.trim().is_empty(),
                    "session template rendered an empty ID"
                );
                value
            }
            None => identity.session_id(),
        };
        let source_string = |field: &str| {
            intent
                .doc_vars
                .as_ref()
                .and_then(|d| d.get(field))
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        };
        let fire = gents_protocol::trigger_delivery::TriggerFire {
            request_id: identity.request_id(),
            fire_key: key,
            identity,
            goal_id: intent.task.goal_objective_template.as_ref().map(|_| {
                gents_protocol::goal::deterministic_goal_id(
                    snapshot.agent(&intent.task.agent_did).unwrap().node_did(),
                    &session_id,
                )
            }),
            session_id,
            task_id: intent.task.task_id.clone(),
            emit_outcome: intent.task.emit_outcome,
            goal_objective: None,
            goal_token_budget: intent.task.goal_token_budget,
            goal_assignment_applied: false,
            queued_serial: intent.concurrency == crate::documents::ConcurrencyMode::QueuedSerial,
            source_handoff_id: source_string("handoff_id"),
            reply_session_id: source_string("reply_session_id"),
            shard_id: source_string("shard_id"),
            attempt: intent
                .doc_vars
                .as_ref()
                .and_then(|d| d.get("attempt"))
                .and_then(serde_json::Value::as_i64),
            created_at: now.to_owned(),
        };
        anyhow::ensure!(
            !fire.emit_outcome
                || fire
                    .source_handoff_id
                    .as_ref()
                    .is_some_and(|s| !s.is_empty()),
            "emit_outcome requires a source handoff_id"
        );
        Ok(fire)
    }

    fn concurrency_node_did(
        intent: &FireIntent,
        snapshot: &ActiveRuntimeSnapshot,
    ) -> Result<String, String> {
        snapshot
            .agent(&intent.task.agent_did)
            .map(|agent| agent.node_did().to_string())
            .ok_or_else(|| {
                snapshot
                    .unavailable_public_message(&intent.task.agent_did)
                    .map(ToOwned::to_owned)
                    .unwrap_or_else(|| format!("agent {} is not loaded", intent.task.agent_did))
            })
    }

    fn durable_goal_request_id(
        intent: &FireIntent,
        snapshot: &ActiveRuntimeSnapshot,
        delivery: &Option<durable::PreparedFire>,
        rendered_goal_objective: &Option<String>,
    ) -> Result<Option<String>, FireResult> {
        let durable_goal_request_id = if let Some(delivery) = &delivery {
            Some(delivery.receipt.request_id.clone())
        } else {
            match rendered_goal_objective.as_ref() {
                Some(_) => match Self::concurrency_node_did(intent, snapshot) {
                    Ok(node_did) => Some(
                        crate::goal::task_goal_fire_identity(
                            &node_did,
                            &intent.task.task_id,
                            &intent.durable_fire_key,
                        )
                        .request_id,
                    ),
                    Err(reason) => {
                        let result = FireResult::Errored {
                            error: format!("goal identity: {reason}"),
                        };
                        return Err(result);
                    }
                },
                None => None,
            }
        };
        Ok(durable_goal_request_id)
    }

    async fn gate_locked_fire(
        &self,
        intent: IntentView<'_>,
        gate: LockedFire<'_>,
    ) -> Result<(), FireResult> {
        let LockedFire {
            delivery_identity,
            node_did,
            trigger_id,
            delivery,
            durable_goal_request_id,
        } = gate;
        if let Some(identity) = &delivery_identity {
            let recovered = self
                .materializer
                .recover_admitted_fire(identity, intent.task)
                .await;
            let result = match recovered {
                Ok(Some(request_id)) => Some(FireResult::Duplicate { request_id }),
                Ok(None) => None,
                Err(error) => Some(FireResult::Errored {
                    error: format!("recover locked admitted fire: {error}"),
                }),
            };
            if let Some(result) = result {
                return Err(result);
            }
        }

        if intent.group_vars.is_some() {
            let Some(_correlation) = intent
                .correlation
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
            else {
                let result = FireResult::Errored {
                    error: "per_group intent requires a non-empty correlation".to_string(),
                };
                return Err(result);
            };
            match self
                .materializer
                .has_materialized_group_request(node_did, trigger_id, intent.durable_fire_key)
                .await
            {
                Ok(true) => {
                    let result = FireResult::Skipped {
                        reason: "per_group: request already materialized".to_string(),
                    };
                    return Err(result);
                }
                Ok(false) => {}
                Err(error) => {
                    let result = FireResult::Errored {
                        error: format!("group marker query: {error}"),
                    };
                    return Err(result);
                }
            }
        }

        use crate::documents::ConcurrencyMode;
        match (intent.group_vars.is_some(), intent.concurrency) {
            (true, _) | (false, ConcurrencyMode::Parallel | ConcurrencyMode::QueuedSerial) => {}
            (false, ConcurrencyMode::Serial) => match self
                .materializer
                .has_active_runtime_request_for_trigger(
                    node_did,
                    trigger_id,
                    durable_goal_request_id.as_deref(),
                )
                .await
            {
                Ok(true) => {
                    let result = FireResult::Skipped {
                        reason: SERIAL_BUSY.to_string(),
                    };
                    return Err(result);
                }
                Ok(false) => {}
                Err(error) => {
                    let result = FireResult::Errored {
                        error: format!("in-flight query: {error}"),
                    };
                    return Err(result);
                }
            },
            (false, ConcurrencyMode::LatestOnly) if delivery.is_some() => {}
            (false, ConcurrencyMode::LatestOnly) => {
                if let Err(error) = self
                    .materializer
                    .supersede_active_runtime_requests_for_trigger(
                        node_did,
                        trigger_id,
                        durable_goal_request_id.as_deref(),
                    )
                    .await
                {
                    let result = FireResult::Errored {
                        error: format!("supersede: {error}"),
                    };
                    return Err(result);
                }
            }
        }

        Ok(())
    }
}
