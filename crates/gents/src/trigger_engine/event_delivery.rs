//! Shared event selection and durable group observation for task triggers and
//! callback bindings. Publication stays with each existing request/invocation owner.
use anyhow::{Context, Result};
use chrono::{DateTime, SecondsFormat, Utc};
use gents_protocol::event_delivery::{EventConsumer, EventGroupState};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::event_source::SourceSchemaCache;
use crate::document_config::{CallbackBinding, EventGroupCount, EventSource};
use crate::graphql::escape_graphql_string;
use crate::runtime_snapshot::{ResolvedEventTrigger, MAX_EVENT_TRIGGER_GROUP_DOCS};

pub(crate) const GROUP_RECOVERY_PAGE_SIZE: usize = 256;

/// Borrow existing resolved/configuration owners; no second writable config.
#[derive(Clone, Copy)]
pub(crate) enum Delivery<'a> {
    Trigger {
        agent_did: &'a str,
        trigger: &'a ResolvedEventTrigger,
    },
    Callback {
        binding: &'a CallbackBinding,
        source: &'a EventSource,
    },
}
impl Delivery<'_> {
    pub(crate) fn owner(&self) -> &str {
        match self {
            Self::Trigger { agent_did, .. } => agent_did,
            Self::Callback { binding, .. } => &binding.agent_did,
        }
    }
    pub(crate) fn consumer(&self) -> EventConsumer {
        match self {
            Self::Trigger { trigger, .. } => EventConsumer::Trigger {
                trigger_id: trigger.trigger_id.clone(),
            },
            Self::Callback { binding, .. } => EventConsumer::CallbackBinding {
                binding_id: binding.binding_id.clone(),
            },
        }
    }
    pub(crate) fn collection(&self) -> &str {
        match self {
            Self::Trigger { trigger, .. } => &trigger.source_collection,
            Self::Callback { source, .. } => &source.source_collection,
        }
    }
    pub(crate) fn filter(&self) -> Option<&str> {
        match self {
            Self::Trigger { trigger, .. } => trigger.filter.as_deref(),
            Self::Callback { source, .. } => source.filter.as_deref(),
        }
    }
    pub(crate) fn correlation_field(&self) -> Option<&str> {
        match self {
            Self::Trigger { trigger, .. } => trigger.correlation_field.as_deref(),
            Self::Callback { source, .. } => source.correlation_field.as_deref(),
        }
    }
    pub(crate) fn timeout_secs(&self) -> Result<Option<u64>> {
        match self {
            Self::Trigger { trigger, .. } => Ok(trigger.group_timeout_secs),
            Self::Callback { source, .. } => source
                .group
                .as_ref()
                .and_then(|group| group.timeout_secs)
                .map(|n| {
                    anyhow::ensure!(n > 0, "group timeout must be positive");
                    Ok(n as u64)
                })
                .transpose(),
        }
    }
    fn minimum_count(&self) -> Result<usize> {
        let count = match self {
            Self::Trigger { trigger, .. } => trigger.group_min_count,
            Self::Callback { source, .. } => {
                let n = source
                    .group
                    .as_ref()
                    .context("callback source is not grouped")?
                    .min_count
                    .unwrap_or(1);
                anyhow::ensure!(n > 0, "minimum group count must be positive");
                usize::try_from(n)?
            }
        };
        anyhow::ensure!(
            count > 0 && count <= MAX_EVENT_TRIGGER_GROUP_DOCS,
            "minimum group count exceeds bounds"
        );
        Ok(count)
    }
    fn expected(&self) -> Result<Option<EventGroupCount>> {
        match self {
            Self::Callback { source, .. } => Ok(source
                .group
                .as_ref()
                .context("callback source is not grouped")?
                .expected_count
                .clone()),
            Self::Trigger { trigger, .. } => {
                anyhow::ensure!(
                    trigger.expected_count.is_none() || trigger.expected_count_field.is_none(),
                    "group has two expected count sources"
                );
                Ok(
                    match (trigger.expected_count, &trigger.expected_count_field) {
                        (Some(n), None) => Some(EventGroupCount::Fixed(i64::try_from(n)?)),
                        (None, Some(field)) => Some(EventGroupCount::SourceField {
                            source_field: field.clone(),
                        }),
                        _ => None,
                    },
                )
            }
        }
    }
    pub(crate) fn validate_group(&self) -> Result<()> {
        anyhow::ensure!(
            !self.owner().trim().is_empty(),
            "event delivery owner is missing"
        );
        if let Self::Callback { binding, source } = self {
            anyhow::ensure!(
                source.event_kind.as_deref().unwrap_or("created") == "created",
                "callback event source requires created events"
            );
            anyhow::ensure!(
                binding.agent_did == source.agent_did
                    && binding.event_source_id == source.event_source_id,
                "callback source owner/reference mismatch"
            );
        }
        let field = self
            .correlation_field()
            .context("group requires correlation field")?;
        crate::graphql::validate_graphql_name(field)?;
        let minimum = self.minimum_count()?;
        let expected = self.expected()?;
        anyhow::ensure!(
            expected.is_some() || self.timeout_secs()?.is_some(),
            "group requires expected count or timeout"
        );
        match expected {
            Some(EventGroupCount::Fixed(n)) => anyhow::ensure!(
                n > 0 && n <= MAX_EVENT_TRIGGER_GROUP_DOCS as i64 && minimum <= n as usize,
                "fixed expected count is outside group bounds"
            ),
            Some(EventGroupCount::SourceField { source_field }) => {
                crate::graphql::validate_graphql_name(&source_field)?
            }
            None => {}
        }
        Ok(())
    }
    pub(crate) fn config_key(&self) -> String {
        // Membership changes start another clock; prompt/count/deadline changes
        // retain the observed first-seen time, as the existing event contract requires.
        digest(&json!([
            self.collection(),
            self.filter(),
            self.correlation_field()
        ]))
    }
    pub(crate) fn consumer_key(&self) -> String {
        digest(&json!([self.owner(), self.consumer(), self.config_key()]))
    }
    pub(crate) fn group_key(&self, correlation: &str) -> String {
        let (kind, id) = match self.consumer() {
            EventConsumer::Trigger { trigger_id } => ("trigger", trigger_id),
            EventConsumer::CallbackBinding { binding_id } => ("callback_binding", binding_id),
        };
        digest(&json!([
            self.owner(),
            kind,
            id,
            self.config_key(),
            correlation
        ]))
    }
}
fn digest(value: &Value) -> String {
    format!(
        "{:x}",
        Sha256::digest(serde_json::to_vec(value).expect("JSON value is serializable"))
    )
}

pub(crate) fn selection_filter(
    filter: Option<&str>,
    equality: Option<(&str, &str)>,
) -> Result<String> {
    let mut clauses = Vec::new();
    if let Some(filter) = filter.map(str::trim).filter(|s| !s.is_empty()) {
        crate::graphql::validate_graphql_filter_fragment(filter)?;
        clauses.push(filter.to_owned());
    }
    if let Some((field, value)) = equality {
        crate::graphql::validate_graphql_name(field)?;
        clauses.push(format!(
            "{{ {field}: {{_eq:\"{}\"}} }}",
            escape_graphql_string(value)
        ));
    }
    Ok(match clauses.as_slice() {
        [] => "{}".into(),
        [only] => only.clone(),
        _ => format!("{{_and:[{}]}}", clauses.join(",")),
    })
}

pub(crate) async fn probe_document(
    node: &defra_node::EmbeddedNode,
    collection: &str,
    id: &str,
    filter: Option<&str>,
) -> Result<Option<String>> {
    crate::graphql::validate_collection_identifier(collection)?;
    let filter = selection_filter(filter, Some(("_docID", id)))?;
    let response = node
        .execute(&format!(
            "{{{collection}(filter:{filter},limit:1){{_docID _version{{cid height fieldName}}}}}}"
        ))
        .await;
    anyhow::ensure!(
        !response.has_errors(),
        "event selection probe failed: {:?}",
        response.errors
    );
    let rows = crate::graphql::rows::<Value>(&response, collection)?;
    rows.first()
        .map(|row| {
            crate::graphql::document_composite_version(row, "event selection probe")?
                .context("event source lacks composite version")
                .map(|v| v.cid)
        })
        .transpose()
}

pub(crate) struct GroupRecord {
    pub(crate) doc_id: String,
    pub(crate) state: EventGroupState,
}
const GROUP_FIELDS:&str="group_key agent_did consumer correlation consumer_config_key first_seen_at quiesced_at quiesced_reason";
fn decode_group(
    response: &Value,
    delivery: Delivery<'_>,
    correlation: &str,
) -> Result<Option<GroupRecord>> {
    let rows = gents_protocol::graphql::graphql_rows_from_response(response, "EventGroupState");
    anyhow::ensure!(rows.len() <= 1, "ambiguous durable event group");
    rows.into_iter()
        .next()
        .map(|mut row| {
            let doc_id = row
                .as_object_mut()
                .and_then(|row| row.remove("_docID"))
                .and_then(|value| value.as_str().map(str::to_owned))
                .context("group record missing physical ID")?;
            let state: EventGroupState = serde_json::from_value(row)?;
            anyhow::ensure!(
                state.agent_did == delivery.owner()
                    && state.consumer == delivery.consumer()
                    && state.consumer_config_key == delivery.config_key()
                    && state.correlation == correlation
                    && state.group_key == delivery.group_key(correlation),
                "group ownership/identity mismatch"
            );
            Ok(GroupRecord { doc_id, state })
        })
        .transpose()
}
fn group_query(delivery: Delivery<'_>, correlation: &str) -> String {
    format!("{{EventGroupState(filter:{{agent_did:{{_eq:\"{}\"}},group_key:{{_eq:\"{}\"}}}},limit:2){{_docID {GROUP_FIELDS}}}}}",escape_graphql_string(delivery.owner()),escape_graphql_string(&delivery.group_key(correlation)))
}

pub(crate) async fn load_or_create_group(
    node: &defra_node::EmbeddedNode,
    delivery: Delivery<'_>,
    correlation: &str,
) -> Result<GroupRecord> {
    delivery.validate_group()?;
    anyhow::ensure!(
        !correlation.trim().is_empty(),
        "group correlation is missing"
    );
    crate::config_client::ConfigAccess::transact_local(node,None,"event.group_clock",|txn| Box::pin(async move {
        let query=group_query(delivery,correlation);
        if let Some(row)=decode_group(&txn.execute(&query).await?,delivery,correlation)? {return Ok(row);}
        let state=EventGroupState {group_key:delivery.group_key(correlation),agent_did:delivery.owner().into(),consumer:delivery.consumer(),correlation:correlation.into(),consumer_config_key:delivery.config_key(),first_seen_at:Utc::now().to_rfc3339_opts(SecondsFormat::Millis,true),quiesced_at:None,quiesced_reason:None};
        txn.execute_with_variables("mutation($input:EventGroupStateMutationInputArg!){create_EventGroupState(input:$input){_docID}}",&json!({"input":state})).await?;
        decode_group(&txn.execute(&query).await?,delivery,correlation)?.context("created event group disappeared")
    })).await
}

pub(crate) async fn quiesce_group(
    node: &defra_node::EmbeddedNode,
    record: &GroupRecord,
    reason: &str,
) -> Result<()> {
    if record.state.quiesced_at.is_some() {
        return Ok(());
    }
    crate::config_client::ConfigAccess::write_local(node,"event.quiesce_group",&format!("mutation{{update_EventGroupState(filter:{{_docID:{{_eq:\"{}\"}},agent_did:{{_eq:\"{}\"}}}},input:{{quiesced_at:\"{}\",quiesced_reason:\"{}\"}}){{_docID}}}}",escape_graphql_string(&record.doc_id),escape_graphql_string(&record.state.agent_did),escape_graphql_string(&Utc::now().to_rfc3339_opts(SecondsFormat::Millis,true)),escape_graphql_string(reason))).await.map(|_| ())
}

pub(crate) enum GroupOutcome {
    Empty,
    Pending {
        first_seen: DateTime<Utc>,
        dormant: bool,
    },
    Quiesced,
    Ready {
        first_seen: DateTime<Utc>,
        docs: Vec<Value>,
        complete: bool,
    },
}

pub(crate) async fn evaluate_group(
    node: &defra_node::EmbeddedNode,
    cache: &SourceSchemaCache,
    delivery: Delivery<'_>,
    correlation: &str,
) -> Result<GroupOutcome> {
    delivery.validate_group()?;
    crate::graphql::validate_collection_identifier(delivery.collection())?;
    let fields = cache.fields_for(delivery.collection(), node).await?;
    let filter = selection_filter(
        delivery.filter(),
        Some((
            delivery
                .correlation_field()
                .context("group correlation field missing")?,
            correlation,
        )),
    )?;
    let query = format!(
        "{{{}(filter:{filter},order:{{_docID:ASC}},limit:{}){{_docID {}}}}}",
        delivery.collection(),
        MAX_EVENT_TRIGGER_GROUP_DOCS + 1,
        fields.join(" ")
    );
    let response = node.execute(&query).await;
    anyhow::ensure!(
        !response.has_errors(),
        "group membership query failed: {:?}",
        response.errors
    );
    let docs = crate::graphql::rows::<Value>(&response, delivery.collection())?;
    if docs.is_empty() {
        return Ok(GroupOutcome::Empty);
    }
    let record = load_or_create_group(node, delivery, correlation).await?;
    if record.state.quiesced_at.is_some() {
        return Ok(GroupOutcome::Quiesced);
    }
    let expected = expected_count(delivery, &docs);
    let reason = match &expected {
        Err(error) => Some(format!("invalid expected cardinality: {error}")),
        Ok(expected)
            if docs.len() > MAX_EVENT_TRIGGER_GROUP_DOCS
                || expected.is_some_and(|n| docs.len() > n) =>
        {
            Some("group exceeds expected count or hard document cap".into())
        }
        _ => None,
    };
    if let Some(reason) = reason {
        quiesce_group(node, &record, &reason).await?;
        return Ok(GroupOutcome::Quiesced);
    }
    let expected = expected?;
    let minimum = delivery.minimum_count()?;
    if expected.is_some_and(|n| minimum > n) {
        quiesce_group(
            node,
            &record,
            "minimum count exceeds dynamic expected count",
        )
        .await?;
        return Ok(GroupOutcome::Quiesced);
    }
    let first_seen = DateTime::parse_from_rfc3339(&record.state.first_seen_at)?.with_timezone(&Utc);
    let timed_out = delivery.timeout_secs()?.is_some_and(|seconds| {
        Utc::now()
            .signed_duration_since(first_seen)
            .to_std()
            .is_ok_and(|elapsed| elapsed >= std::time::Duration::from_secs(seconds))
    });
    if !group_candidate_eligible(docs.len(), expected, minimum, timed_out, true) {
        return Ok(GroupOutcome::Pending {
            first_seen,
            dormant: timed_out && docs.len() < minimum,
        });
    }
    let complete = expected == Some(docs.len());
    Ok(GroupOutcome::Ready {
        first_seen,
        docs,
        complete,
    })
}
fn expected_count(delivery: Delivery<'_>, docs: &[Value]) -> Result<Option<usize>> {
    let Some(expected) = delivery.expected()? else {
        return Ok(None);
    };
    match expected {
        EventGroupCount::Fixed(count) => Ok(Some(usize::try_from(count)?)),
        EventGroupCount::SourceField { source_field } => {
            let mut resolved = None;
            for doc in docs {
                let value = doc
                    .get(&source_field)
                    .context("group member missing expected-count field")?;
                let n =
                    crate::graphql::canonical_positive_count(value, MAX_EVENT_TRIGGER_GROUP_DOCS)
                        .context("expected count must be a canonical positive bounded integer")?;
                anyhow::ensure!(
                    resolved.is_none_or(|prior| prior == n),
                    "group members disagree on expected count"
                );
                resolved = Some(n);
            }
            Ok(resolved)
        }
    }
}

pub(crate) async fn group_correlation_page(
    node: &defra_node::EmbeddedNode,
    delivery: Delivery<'_>,
    cursor: Option<&str>,
) -> Result<(Vec<String>, Option<String>, bool)> {
    delivery.validate_group()?;
    let field = delivery
        .correlation_field()
        .context("group correlation field missing")?;
    crate::graphql::validate_collection_identifier(delivery.collection())?;
    let base = selection_filter(delivery.filter(), None)?;
    let filter = cursor.map_or(base.clone(), |cursor| {
        format!(
            "{{_and:[{base},{{_docID:{{_gt:\"{}\"}}}}]}}",
            escape_graphql_string(cursor)
        )
    });
    let response=node.execute(&format!("{{{}(filter:{filter},order:{{_docID:ASC}},limit:{GROUP_RECOVERY_PAGE_SIZE}){{_docID {field}}}}}",delivery.collection())).await;
    anyhow::ensure!(
        !response.has_errors(),
        "event group recovery page failed: {:?}",
        response.errors
    );
    let rows = crate::graphql::rows::<Value>(&response, delivery.collection())?;
    let cursor = rows
        .last()
        .and_then(|row| row.get("_docID"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let correlations = rows
        .iter()
        .filter_map(|row| row.get(field).and_then(Value::as_str))
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect();
    Ok((correlations, cursor, rows.len() < GROUP_RECOVERY_PAGE_SIZE))
}

pub(crate) fn group_candidate_eligible(
    actual_count: usize,
    expected_count: Option<usize>,
    minimum_count: usize,
    timed_out: bool,
    well_formed: bool,
) -> bool {
    well_formed
        && actual_count > 0
        && actual_count <= crate::runtime_snapshot::MAX_EVENT_TRIGGER_GROUP_DOCS
        && match expected_count {
            Some(expected) => {
                expected > 0
                    && expected <= crate::runtime_snapshot::MAX_EVENT_TRIGGER_GROUP_DOCS
                    && actual_count <= expected
                    && (actual_count == expected || (timed_out && minimum_count <= actual_count))
            }
            None => timed_out && minimum_count <= actual_count,
        }
}

/// Requests themselves are the publication marker. Ordinary grouped requests
/// retain the fire key as their ID; goal requests retain it through the existing
/// goal identity owner. Task or goal-mode edits cannot erase a prior delivery.
pub(crate) fn request_matches_fire_key(owner: &str, request_id: &str, fire_key: &str) -> bool {
    request_id == fire_key || crate::goal::task_goal_fire_key(request_id, owner) == Some(fire_key)
}

pub(crate) fn is_group_fire_key(key: &str) -> bool {
    key.starts_with("11:event-group:")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn config(owner: &str) -> (CallbackBinding, EventSource) {
        let binding = serde_json::from_value(json!({"agent_did":owner,"binding_id":"same-id","callback_id":"callback","event_source_id":"source","input_fields":["value"]})).unwrap();
        let source = serde_json::from_value(json!({"agent_did":owner,"event_source_id":"source","source_collection":"GroupMember","correlation_field":"batch","group":{"expected_count":{"source_field":"expected"},"timeout_secs":1}})).unwrap();
        (binding, source)
    }

    #[test]
    fn consumer_owner_and_membership_separate_group_identity() {
        let (binding, source) = config("owner-a");
        let delivery = Delivery::Callback {
            binding: &binding,
            source: &source,
        };
        let initial = delivery.group_key("batch-a");
        let (other, other_source) = config("owner-b");
        assert_ne!(
            initial,
            Delivery::Callback {
                binding: &other,
                source: &other_source
            }
            .group_key("batch-a")
        );
        let mut changed = source.clone();
        changed.group.as_mut().unwrap().timeout_secs = Some(100);
        assert_eq!(
            initial,
            Delivery::Callback {
                binding: &binding,
                source: &changed
            }
            .group_key("batch-a")
        );
        changed.filter = Some("{value:{_eq:\"included\"}}".into());
        assert_ne!(
            initial,
            Delivery::Callback {
                binding: &binding,
                source: &changed
            }
            .group_key("batch-a")
        );
        let trigger = crate::runtime_snapshot::ResolvedEventTrigger {
            trigger_doc_id: "physical".into(),
            trigger_id: binding.binding_id.clone(),
            task_id: "task".into(),
            task: crate::runtime_snapshot::ResolvedTask {
                task_id: "task".into(),
                name: None,
                behavior_id: "behavior".into(),
                prompt_template: String::new(),
                goal_objective_template: None,
                goal_token_budget: None,
                output_schema_ref: None,
                hooks: Default::default(),
            },
            source_collection: source.source_collection.clone(),
            event_kind: "created".into(),
            filter: None,
            enabled: true,
            concurrency: crate::document_config::ConcurrencyMode::Parallel,
            fire_mode: crate::runtime_snapshot::EventTriggerFireMode::PerGroup,
            correlation_field: source.correlation_field.clone(),
            expected_count: None,
            expected_count_field: Some("expected".into()),
            group_timeout_secs: Some(1),
            group_min_count: 1,
            workspace_authority: None,
        };
        assert_ne!(
            initial,
            Delivery::Trigger {
                agent_did: "owner-a",
                trigger: &trigger
            }
            .group_key("batch-a")
        );
    }

    #[test]
    fn dynamic_count_disagreement_and_foreign_source_reject() {
        let (binding, mut source) = config("owner-a");
        let delivery = Delivery::Callback {
            binding: &binding,
            source: &source,
        };
        assert_eq!(
            expected_count(delivery, &[json!({"expected":2}), json!({"expected":2})]).unwrap(),
            Some(2)
        );
        assert!(expected_count(delivery, &[json!({"expected":2}), json!({"expected":3})]).is_err());
        assert!(expected_count(delivery, &[json!({"expected":0})]).is_err());
        source.agent_did = "owner-b".into();
        assert!(Delivery::Callback {
            binding: &binding,
            source: &source
        }
        .validate_group()
        .is_err());
    }

    #[tokio::test]
    async fn shared_clock_survives_restart_and_quiescence_is_owner_scoped() {
        let node = defra_node::EmbeddedNode::builder().build().await.unwrap();
        node.add_schema(gents_protocol::schemas::EVENT_GROUP_STATE)
            .await
            .unwrap();
        node.add_schema("type GroupMember { batch:String expected:Int value:String }")
            .await
            .unwrap();
        let response=node.execute("mutation {create_GroupMember(input:{batch:\"one\",expected:2,value:\"first\"}){_docID}}").await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        let (binding, mut source) = config("owner-a");
        source.group.as_mut().unwrap().timeout_secs = Some(3600);
        let delivery = Delivery::Callback {
            binding: &binding,
            source: &source,
        };
        let first = load_or_create_group(&node, delivery, "one").await.unwrap();
        let restarted = load_or_create_group(&node, delivery, "one").await.unwrap();
        assert_eq!(first.doc_id, restarted.doc_id);
        assert_eq!(first.state.first_seen_at, restarted.state.first_seen_at);
        assert!(matches!(
            evaluate_group(&node, &SourceSchemaCache::default(), delivery, "one")
                .await
                .unwrap(),
            GroupOutcome::Pending { .. }
        ));
        let response=node.execute("mutation {create_GroupMember(input:{batch:\"one\",expected:3,value:\"inconsistent\"}){_docID}}").await;
        assert!(!response.has_errors(), "{:?}", response.errors);
        assert!(matches!(
            evaluate_group(&node, &SourceSchemaCache::default(), delivery, "one")
                .await
                .unwrap(),
            GroupOutcome::Quiesced
        ));
        let stored = load_or_create_group(&node, delivery, "one").await.unwrap();
        assert!(stored
            .state
            .quiesced_reason
            .as_deref()
            .unwrap()
            .contains("disagree"));
        let (other, other_source) = config("owner-b");
        let independent = load_or_create_group(
            &node,
            Delivery::Callback {
                binding: &other,
                source: &other_source,
            },
            "one",
        )
        .await
        .unwrap();
        assert_ne!(stored.doc_id, independent.doc_id);
        assert!(independent.state.quiesced_at.is_none());
        node.shutdown().await;
    }
}
