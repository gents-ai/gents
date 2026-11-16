use gents_protocol::document_query::{fields, Direction, Filter, Query, Selection};
use gents_protocol::views::View;
use gents_store::config_client::ConfigApplyTxn;
use gents_store::views::ViewError;
use serde::{Deserialize, Serialize};

use super::agents::{AgentList, AgentRow};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskRow {
    #[serde(rename = "_docID")]
    pub doc_id: String,
    #[serde(flatten)]
    pub task: crate::Task,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriggerRow {
    #[serde(rename = "_docID")]
    pub doc_id: String,
    #[serde(flatten)]
    pub trigger: crate::Trigger,
    pub next_run_at: Option<String>,
    pub last_attempt_at: Option<String>,
    pub last_fired_source_doc_id: Option<String>,
    pub last_status: Option<String>,
    pub last_error: Option<String>,
    pub fire_count: Option<i64>,
}

impl TriggerRow {
    pub fn into_parts(
        self,
    ) -> (
        crate::Trigger,
        crate::TriggerObservation,
        crate::ScheduleObservation,
    ) {
        let trigger_id = self.trigger.trigger_id.clone();
        (
            self.trigger,
            crate::TriggerObservation {
                trigger_id: trigger_id.clone(),
                last_attempt_at: self.last_attempt_at,
                last_fired_source_doc_id: self.last_fired_source_doc_id,
                last_status: self.last_status,
                last_error: self.last_error,
                fire_count: self.fire_count,
            },
            crate::ScheduleObservation {
                trigger_id,
                next_run_at: self.next_run_at,
            },
        )
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScheduleRow {
    #[serde(rename = "_docID")]
    pub doc_id: String,
    #[serde(flatten)]
    pub schedule: crate::Schedule,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventSourceRow {
    #[serde(rename = "_docID")]
    pub doc_id: String,
    #[serde(flatten)]
    pub event_source: crate::EventSource,
}

pub struct TaskList {
    node_did: String,
    doc_ids: Option<Vec<String>>,
    calling_agent: bool,
    task_id: Option<String>,
}

impl TaskList {
    pub fn new(node_did: impl Into<String>) -> Self {
        Self {
            node_did: node_did.into(),
            doc_ids: None,
            calling_agent: false,
            task_id: None,
        }
    }

    pub fn doc_ids(mut self, doc_ids: Vec<String>) -> Self {
        self.doc_ids = Some(doc_ids);
        self
    }

    pub fn calling_agent(mut self) -> Self {
        self.calling_agent = true;
        self
    }

    pub fn task_id(mut self, task_id: impl Into<String>) -> Self {
        self.task_id = Some(task_id.into());
        self
    }
}

impl View for TaskList {
    type Collection = fields::task::Doc;
    type Row = TaskRow;

    fn query(&self, reader_did: &str) -> Query<Self::Collection> {
        use fields::task as f;
        let mut query = Query::select(
            Selection::doc_id()
                .field(f::TASK_ID)
                .field(f::NODE_DID)
                .field(f::DISPLAY_NAME)
                .field(f::DESCRIPTION)
                .field(f::AGENT_DID)
                .field(f::PROMPT_TEMPLATE)
                .field(f::EMIT_OUTCOME)
                .field(f::OUTCOME_READERS)
                .field(f::GOAL_OBJECTIVE_TEMPLATE)
                .field(f::GOAL_TOKEN_BUDGET)
                .field(f::HOOKS)
                .field(f::ENABLED)
                .field(f::OUTPUT_SCHEMA_REF)
                .field(f::CREATED_AT)
                .field(f::UPDATED_AT)
                .field(f::TAGS),
        )
        .filter(Filter::eq(f::NODE_DID, &self.node_did))
        .order_by(f::TASK_ID, Direction::Asc);
        if self.calling_agent {
            query = query.filter(Filter::eq(f::AGENT_DID, reader_did));
        }
        if let Some(doc_ids) = &self.doc_ids {
            query = query.filter(Filter::any_of(f::DOC_ID, doc_ids));
        }
        if let Some(task_id) = &self.task_id {
            query = query.filter(Filter::eq(f::TASK_ID, task_id));
        }
        query
    }
}

pub struct TriggerList {
    node_did: String,
    doc_ids: Option<Vec<String>>,
    trigger_ids: Option<Vec<String>>,
    task_ids: Option<Vec<String>>,
}

impl TriggerList {
    pub fn new(node_did: impl Into<String>) -> Self {
        Self {
            node_did: node_did.into(),
            doc_ids: None,
            trigger_ids: None,
            task_ids: None,
        }
    }

    pub fn doc_ids(mut self, doc_ids: Vec<String>) -> Self {
        self.doc_ids = Some(doc_ids);
        self
    }

    pub fn task_ids(mut self, task_ids: Vec<String>) -> Self {
        self.task_ids = Some(task_ids);
        self
    }

    pub fn trigger_ids(mut self, trigger_ids: Vec<String>) -> Self {
        self.trigger_ids = Some(trigger_ids);
        self
    }
}

impl View for TriggerList {
    type Collection = fields::trigger::Doc;
    type Row = TriggerRow;

    fn query(&self, _reader_did: &str) -> Query<Self::Collection> {
        use fields::trigger as f;
        let mut query = Query::select(
            Selection::doc_id()
                .field(f::TRIGGER_ID)
                .field(f::NODE_DID)
                .field(f::DISPLAY_NAME)
                .field(f::DESCRIPTION)
                .field(f::TASK_ID)
                .field(f::SOURCE)
                .field(f::SESSION_ID_TEMPLATE)
                .field(f::CREATE_SESSION)
                .field(f::MAX_FIRES_PER_HOUR)
                .field(f::ENABLED)
                .field(f::CONCURRENCY)
                .field(f::NEXT_RUN_AT)
                .field(f::LAST_ATTEMPT_AT)
                .field(f::LAST_FIRED_SOURCE_DOC_ID)
                .field(f::LAST_STATUS)
                .field(f::LAST_ERROR)
                .field(f::FIRE_COUNT)
                .field(f::CREATED_AT)
                .field(f::UPDATED_AT)
                .field(f::TAGS),
        )
        .filter(Filter::eq(f::NODE_DID, &self.node_did))
        .order_by(f::TRIGGER_ID, Direction::Asc);
        if let Some(doc_ids) = &self.doc_ids {
            query = query.filter(Filter::any_of(f::DOC_ID, doc_ids));
        }
        if let Some(task_ids) = &self.task_ids {
            query = query.filter(Filter::any_of(f::TASK_ID, task_ids));
        }
        if let Some(trigger_ids) = &self.trigger_ids {
            query = query.filter(Filter::any_of(f::TRIGGER_ID, trigger_ids));
        }
        query
    }
}

pub struct ScheduleList {
    node_did: String,
    doc_ids: Option<Vec<String>>,
    schedule_ids: Option<Vec<String>>,
}

impl ScheduleList {
    pub fn new(node_did: impl Into<String>) -> Self {
        Self {
            node_did: node_did.into(),
            doc_ids: None,
            schedule_ids: None,
        }
    }

    pub fn doc_ids(mut self, doc_ids: Vec<String>) -> Self {
        self.doc_ids = Some(doc_ids);
        self
    }

    pub fn schedule_ids(mut self, schedule_ids: Vec<String>) -> Self {
        self.schedule_ids = Some(schedule_ids);
        self
    }
}

impl View for ScheduleList {
    type Collection = fields::schedule::Doc;
    type Row = ScheduleRow;

    fn query(&self, _reader_did: &str) -> Query<Self::Collection> {
        use fields::schedule as f;
        let mut query = Query::select(
            Selection::doc_id()
                .field(f::SCHEDULE_ID)
                .field(f::NODE_DID)
                .field(f::DISPLAY_NAME)
                .field(f::CADENCE)
                .field(f::CREATED_AT)
                .field(f::UPDATED_AT)
                .field(f::TAGS),
        )
        .filter(Filter::eq(f::NODE_DID, &self.node_did))
        .order_by(f::SCHEDULE_ID, Direction::Asc);
        if let Some(doc_ids) = &self.doc_ids {
            query = query.filter(Filter::any_of(f::DOC_ID, doc_ids));
        }
        if let Some(schedule_ids) = &self.schedule_ids {
            query = query.filter(Filter::any_of(f::SCHEDULE_ID, schedule_ids));
        }
        query
    }
}

pub struct EventSourceList {
    node_did: String,
    doc_ids: Option<Vec<String>>,
    event_source_ids: Option<Vec<String>>,
}

impl EventSourceList {
    pub fn new(node_did: impl Into<String>) -> Self {
        Self {
            node_did: node_did.into(),
            doc_ids: None,
            event_source_ids: None,
        }
    }

    pub fn doc_ids(mut self, doc_ids: Vec<String>) -> Self {
        self.doc_ids = Some(doc_ids);
        self
    }

    pub fn event_source_ids(mut self, event_source_ids: Vec<String>) -> Self {
        self.event_source_ids = Some(event_source_ids);
        self
    }
}

impl View for EventSourceList {
    type Collection = fields::event_source::Doc;
    type Row = EventSourceRow;

    fn query(&self, _reader_did: &str) -> Query<Self::Collection> {
        use fields::event_source as f;
        let mut query = Query::select(
            Selection::doc_id()
                .field(f::EVENT_SOURCE_ID)
                .field(f::NODE_DID)
                .field(f::DISPLAY_NAME)
                .field(f::SOURCE_COLLECTION)
                .field(f::EVENT_KIND)
                .field(f::FILTER)
                .field(f::CORRELATION_FIELD)
                .field(f::GROUP)
                .field(f::WORKSPACE_AUTHORITY)
                .field(f::CREATED_AT)
                .field(f::UPDATED_AT)
                .field(f::TAGS),
        )
        .filter(Filter::eq(f::NODE_DID, &self.node_did))
        .order_by(f::EVENT_SOURCE_ID, Direction::Asc);
        if let Some(doc_ids) = &self.doc_ids {
            query = query.filter(Filter::any_of(f::DOC_ID, doc_ids));
        }
        if let Some(event_source_ids) = &self.event_source_ids {
            query = query.filter(Filter::any_of(f::EVENT_SOURCE_ID, event_source_ids));
        }
        query
    }
}

#[derive(Debug, Serialize)]
pub struct Inventory {
    pub tasks: Vec<TaskRow>,
    pub agents: Vec<AgentRow>,
    pub triggers: Vec<TriggerRow>,
    pub schedules: Vec<ScheduleRow>,
    pub event_sources: Vec<EventSourceRow>,
}

pub async fn inventory(
    txn: &ConfigApplyTxn<'_>,
    node_did: &str,
    calling_agent: bool,
    task_id: Option<&str>,
) -> Result<Inventory, ViewError> {
    let mut tasks = TaskList::new(node_did);
    if calling_agent {
        tasks = tasks.calling_agent();
    }
    if let Some(task_id) = task_id {
        tasks = tasks.task_id(task_id);
    }
    let tasks = txn.read_view(&tasks).await?;
    let mut triggers = TriggerList::new(node_did);
    if calling_agent || task_id.is_some() {
        triggers = triggers.task_ids(tasks.iter().map(|row| row.task.task_id.clone()).collect());
    }
    let triggers = txn.read_view(&triggers).await?;
    let mut schedules = ScheduleList::new(node_did);
    let mut event_sources = EventSourceList::new(node_did);
    if calling_agent || task_id.is_some() {
        schedules = schedules.schedule_ids(
            triggers
                .iter()
                .filter_map(|row| match &row.trigger.source {
                    crate::TriggerSource::Schedule { schedule_id } => Some(schedule_id.clone()),
                    crate::TriggerSource::Event { .. } => None,
                })
                .collect(),
        );
        event_sources = event_sources.event_source_ids(
            triggers
                .iter()
                .filter_map(|row| match &row.trigger.source {
                    crate::TriggerSource::Event { event_source_id } => {
                        Some(event_source_id.clone())
                    }
                    crate::TriggerSource::Schedule { .. } => None,
                })
                .collect(),
        );
    }
    let schedules = txn.read_view(&schedules).await?;
    let event_sources = txn.read_view(&event_sources).await?;
    let mut agents = AgentList::new(node_did);
    if calling_agent {
        if let Some(reader) = txn.actor_did() {
            agents = agents.agent_did(reader);
        }
    }
    let agents = txn.read_view(&agents).await?;
    Ok(Inventory {
        tasks,
        agents,
        triggers,
        schedules,
        event_sources,
    })
}
