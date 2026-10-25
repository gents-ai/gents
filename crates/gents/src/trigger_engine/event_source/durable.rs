use super::*;
use crate::config_client::ConfigAccess;
use crate::graphql::escape_graphql_string;
use gents_protocol::event_delivery::EventConsumer;

pub(super) struct PendingCheckpoint {
    arrival: CheckpointArrival,
    result: tokio::sync::oneshot::Receiver<super::super::FireResult>,
}

struct CheckpointArrival {
    owner: String,
    trigger_id: String,
    collection: String,
    position: String,
    source_doc_id: String,
    source_document: Option<serde_json::Value>,
    generation: u64,
}

const MAX_RETRY_BACKOFF_INTERVALS: u32 = 64;

pub(super) struct ParkedArrival {
    position: String,
    collection: String,
    source_doc_id: String,
    source_document: Option<serde_json::Value>,
    generation: u64,
    retry_at: Option<Instant>,
    attempts: u32,
}

impl EventSource {
    pub(super) async fn release_parked_document(&mut self, collection: &str, doc_id: &str) {
        if collection == crate::Collection::Trigger.graphql_type() {
            return;
        }
        let parked = self
            .parked_arrivals
            .iter()
            .filter(|(_, parked)| parked.collection == collection && parked.source_doc_id == doc_id)
            .map(|(trigger_id, _)| trigger_id.clone())
            .collect::<Vec<_>>();
        if parked.is_empty() {
            return;
        }
        let Ok(current) = self.read_source_doc(collection, doc_id).await else {
            return;
        };
        for trigger_id in parked {
            self.release_if_changed(&trigger_id, current.as_ref());
        }
    }

    fn release_if_changed(&mut self, trigger_id: &str, current: Option<&serde_json::Value>) {
        if self
            .parked_arrivals
            .get(trigger_id)
            .is_some_and(|parked| current.is_none() || parked.source_document.as_ref() != current)
        {
            tracing::debug!(%trigger_id, deleted = current.is_none(),
                "refused source document changed; retrying its fire");
            self.parked_arrivals.remove(trigger_id);
        }
    }
}

impl EventSource {
    async fn release_changed_refusals(&mut self) {
        let now = Instant::now();
        if self
            .last_refusal_scan
            .is_some_and(|last| now.duration_since(last) < self.rescan_interval)
        {
            return;
        }
        self.last_refusal_scan = Some(now);
        let refused = self
            .parked_arrivals
            .iter()
            .filter(|(_, parked)| parked.retry_at.is_none())
            .map(|(trigger_id, parked)| {
                (
                    trigger_id.clone(),
                    parked.collection.clone(),
                    parked.source_doc_id.clone(),
                )
            })
            .collect::<Vec<_>>();
        for (trigger_id, collection, doc_id) in refused {
            let Ok(current) = self.read_source_doc(&collection, &doc_id).await else {
                continue;
            };
            self.release_if_changed(&trigger_id, current.as_ref());
        }
    }
}

impl ParkedArrival {
    fn blocks(&self, generation: u64, now: Instant) -> bool {
        generation == self.generation && self.retry_at.is_none_or(|retry_at| now < retry_at)
    }
}

impl EventSource {
    pub(super) async fn finish_durable_checkpoint(&mut self) {
        let Some(PendingCheckpoint {
            arrival: pending,
            result,
        }) = self.durable_checkpoint.take()
        else {
            return;
        };
        let result = tokio::select! {
            _ = self.cancel.cancelled() => return,
            result = result => result,
        };
        let refusal = match &result {
            Ok(super::super::FireResult::Rejected { error }) => Some(error.clone()),
            Ok(super::super::FireResult::Skipped { reason })
                if reason != super::super::SERIAL_BUSY =>
            {
                Some(reason.clone())
            }
            _ => None,
        };
        let acknowledged = matches!(
            &result,
            Ok(super::super::FireResult::Fired { .. } | super::super::FireResult::Duplicate { .. })
        ) || matches!(&result, Ok(super::super::FireResult::Skipped { reason }) if reason == super::super::SERIAL_BUSY);
        if acknowledged {
            let advanced = ConfigAccess::transact_local_write(
                &self.node,
                None,
                "trigger.advance_arrival",
                |txn| {
                    Box::pin(async {
                        crate::config_client::event_source_cursor::checkpoint_prefix(
                            txn,
                            &pending.owner,
                            &EventConsumer::Trigger {
                                trigger_id: pending.trigger_id.clone(),
                            },
                            &pending.collection,
                            &pending.position,
                            matches!(&result, Ok(super::super::FireResult::Skipped { reason })
                                if reason == super::super::SERIAL_BUSY),
                        )
                        .await
                    })
                },
            )
            .await;
            self.durable_ready = advanced.as_ref().copied().unwrap_or(false);
            match advanced {
                Ok(true) => {
                    self.parked_arrivals.remove(&pending.trigger_id);
                    return;
                }
                Ok(false) => {}
                Err(error) => {
                    tracing::warn!(%error, trigger_id = %pending.trigger_id, "arrival checkpoint remains pending");
                }
            }
        } else {
            self.durable_ready = false;
            match &result {
                Ok(super::super::FireResult::Errored { error }) => {
                    tracing::warn!(trigger_id = %pending.trigger_id, source_doc_id = %pending.source_doc_id,
                        %error, "event trigger materialization failed; preserving arrival for retry");
                }
                Err(error) => {
                    tracing::warn!(trigger_id = %pending.trigger_id, source_doc_id = %pending.source_doc_id,
                        %error, "event trigger acknowledgment channel closed; preserving arrival for retry");
                }
                _ => {}
            }
        }
        self.park_arrival(&pending, refusal);
    }

    fn park_arrival(&mut self, pending: &CheckpointArrival, refusal: Option<String>) {
        let attempts = self
            .parked_arrivals
            .get(&pending.trigger_id)
            .filter(|parked| {
                parked.position == pending.position && parked.generation == pending.generation
            })
            .map_or(1, |parked| parked.attempts.saturating_add(1));
        let retry_at = match &refusal {
            Some(reason) => {
                tracing::warn!(
                    trigger_id = %pending.trigger_id,
                    source_collection = %pending.collection,
                    source_doc_id = %pending.source_doc_id,
                    %reason,
                    "event trigger fire refused; the document stays pending until the configuration changes",
                );
                None
            }
            None => {
                let delay = self.rescan_interval.saturating_mul(
                    2u32.saturating_pow(attempts - 1)
                        .min(MAX_RETRY_BACKOFF_INTERVALS),
                );
                tracing::warn!(
                    trigger_id = %pending.trigger_id,
                    source_collection = %pending.collection,
                    source_doc_id = %pending.source_doc_id,
                    attempts,
                    retry_in_ms = delay.as_millis() as u64,
                    "event trigger fire was not acknowledged; retrying after backoff",
                );
                Some(Instant::now() + delay)
            }
        };
        self.parked_arrivals.insert(
            pending.trigger_id.clone(),
            ParkedArrival {
                position: pending.position.clone(),
                collection: pending.collection.clone(),
                source_doc_id: pending.source_doc_id.clone(),
                source_document: pending.source_document.clone(),
                generation: pending.generation,
                retry_at,
                attempts,
            },
        );
    }

    pub(super) async fn next_durable_fire(&mut self) -> Option<FireIntent> {
        self.durable_ready = false;
        let snapshot = self.snapshot_rx.borrow().clone();
        let mut triggers = snapshot
            .active_event_triggers()
            .values()
            .filter(|t| {
                t.enabled
                    && t.event_kind == "created"
                    && t.fire_mode == crate::runtime_snapshot::EventTriggerFireMode::PerDocument
            })
            .cloned()
            .collect::<Vec<_>>();
        triggers.sort_by(|a, b| a.trigger_id.cmp(&b.trigger_id));
        let now = Instant::now();
        self.parked_arrivals.retain(|trigger_id, parked| {
            parked.generation == snapshot.generation
                && triggers.iter().any(|t| &t.trigger_id == trigger_id)
        });
        self.release_changed_refusals().await;
        triggers.retain(|t| {
            self.parked_arrivals
                .get(&t.trigger_id)
                .is_none_or(|parked| !parked.blocks(snapshot.generation, now))
        });
        if let Some(last) = &self.durable_after_trigger {
            let offset = triggers
                .iter()
                .position(|t| &t.trigger_id > last)
                .unwrap_or(0);
            triggers.rotate_left(offset);
        }
        for trigger in triggers {
            match self.next_trigger_arrival(&snapshot, &trigger).await {
                Ok(Some(intent)) => {
                    self.durable_after_trigger = Some(trigger.trigger_id.clone());
                    return Some(intent);
                }
                Ok(None) => {}
                Err(error) => tracing::warn!(%error, trigger_id = %trigger.trigger_id,
                    "durable trigger delivery remains pending"),
            }
        }
        None
    }

    async fn next_trigger_arrival(
        &mut self,
        snapshot: &ActiveRuntimeSnapshot,
        trigger: &crate::runtime_snapshot::ResolvedEventTrigger,
    ) -> Result<Option<FireIntent>> {
        let owner = Self::delivery(snapshot, trigger)?.owner().to_owned();
        let consumer = EventConsumer::Trigger {
            trigger_id: trigger.trigger_id.clone(),
        };
        let record = ConfigAccess::transact_local_readonly(
            &self.node,
            None,
            "trigger.read_arrival_cursor",
            |txn| {
                Box::pin(async {
                    crate::config_client::event_source_cursor::load_for_source(
                        txn,
                        &owner,
                        &consumer,
                        &trigger.source_collection,
                    )
                    .await
                })
            },
        )
        .await?;
        let record = match record {
            Some(record) => record,
            None => {
                ConfigAccess::transact_local_write(
                    &self.node,
                    None,
                    "trigger.seed_arrival_cursor",
                    |txn| {
                        Box::pin(async {
                            crate::config_client::event_source_cursor::load_or_seed_for_source(
                                txn,
                                &owner,
                                &consumer,
                                &trigger.source_collection,
                            )
                            .await
                        })
                    },
                )
                .await?
            }
        };
        let response = crate::graphql::graphql_with_transaction_retry(&self.node, &format!(
            "{{ _documentArrivals(collection: \"{}\", after: \"{}\", limit: 128) {{ head next entries {{ cursor docID }} }} }}",
            escape_graphql_string(&record.cursor.source_collection), escape_graphql_string(&record.cursor.after)), "trigger.read_arrivals").await?;
        let data = response.data.ok_or(EventSourceError::ArrivalDataMissing)?;
        let page = &data["_documentArrivals"];
        let entries = page["entries"]
            .as_array()
            .ok_or(EventSourceError::ArrivalEntriesMissing)?;
        for entry in entries {
            let doc_id = entry["docID"]
                .as_str()
                .ok_or(EventSourceError::ArrivalDocIdMissing)?;
            let position = entry["cursor"]
                .as_str()
                .ok_or(EventSourceError::ArrivalCursorMissing)?;
            if !self.probe_filter(doc_id, trigger).await? {
                self.advance_arrival(&owner, trigger, position).await?;
                continue;
            }
            let mut build = self
                .build_intents_for_candidates(
                    snapshot,
                    &trigger.source_collection,
                    doc_id,
                    vec![trigger.clone()],
                    false,
                )
                .await;
            let Some(mut intent) = build.intents.pop() else {
                if build.correlation_pending {
                    self.commit_delivery_seen_state(&trigger.source_collection, doc_id, &build);
                }
                return Err(EventSourceError::SourceDocNotRendered {
                    doc_id: doc_id.to_owned(),
                });
            };
            let source_document = intent.doc_vars.clone();
            let (tx, rx) = tokio::sync::oneshot::channel();
            let observe = intent.on_result;
            let unacknowledged = UnacknowledgedGuard::new(
                self.node.clone(),
                owner.clone(),
                trigger.trigger_id.clone(),
                doc_id.to_string(),
            );
            intent.on_result = Box::new(move |result| {
                unacknowledged.acknowledge();
                let _ = tx.send(result.clone());
                observe(result);
            });
            self.durable_checkpoint = Some(PendingCheckpoint {
                arrival: CheckpointArrival {
                    owner,
                    trigger_id: trigger.trigger_id.clone(),
                    collection: trigger.source_collection.clone(),
                    position: position.into(),
                    source_doc_id: doc_id.into(),
                    source_document,
                    generation: snapshot.generation,
                },
                result: rx,
            });
            return Ok(Some(intent));
        }
        let next = page["next"]
            .as_str()
            .ok_or(EventSourceError::ArrivalNextCursorMissing)?;
        if next != record.cursor.after {
            self.advance_arrival(&owner, trigger, next).await?;
        }
        Ok(None)
    }

    async fn advance_arrival(
        &self,
        owner: &str,
        trigger: &crate::runtime_snapshot::ResolvedEventTrigger,
        position: &str,
    ) -> Result<()> {
        ConfigAccess::transact_local_write(&self.node, None, "trigger.exclude_arrival", |txn| {
            Box::pin(async {
                crate::config_client::event_source_cursor::exclude_arrival(
                    txn,
                    owner,
                    &EventConsumer::Trigger {
                        trigger_id: trigger.trigger_id.clone(),
                    },
                    &trigger.source_collection,
                    position,
                )
                .await
            })
        })
        .await?;
        Ok(())
    }
}
