use gents_protocol::document_query::{
    fields::{tool_service_health_state as health, tool_service_registry as service},
    Direction, Filter, Query, Selection,
};
use gents_protocol::views::View;
use serde::{Deserialize, Serialize};

pub struct Services<'a> {
    pub node_did: &'a str,
    pub service_id: Option<&'a str>,
    pub enabled_only: bool,
}

impl View for Services<'_> {
    type Collection = service::Doc;
    type Row = gents_documents::ToolServiceRegistry;

    fn query(&self, _reader_did: &str) -> Query<Self::Collection> {
        service_query(
            service_selection(),
            self.node_did,
            self.service_id,
            self.enabled_only,
        )
    }
}

fn service_query(
    selection: Selection<service::Doc>,
    node_did: &str,
    service_id: Option<&str>,
    enabled_only: bool,
) -> Query<service::Doc> {
    let mut query = Query::select(selection)
        .filter(Filter::eq(service::NODE_DID, node_did))
        .order_by(service::SERVICE_ID, Direction::Asc);
    if enabled_only {
        query = query.filter(Filter::any([
            Filter::eq(service::ENABLED, true),
            Filter::is_null(service::ENABLED),
        ]));
    }
    if let Some(service_id) = service_id {
        query = query.filter(Filter::eq(service::SERVICE_ID, service_id));
    }
    query
}

fn service_selection() -> Selection<service::Doc> {
    Selection::new()
        .field(service::SERVICE_ID)
        .field(service::NODE_DID)
        .field(service::DISPLAY_NAME)
        .field(service::DESCRIPTION)
        .field(service::HOSTNAME)
        .field(service::TAILSCALE_IP)
        .field(service::LAN_IP)
        .field(service::MCP_PORT)
        .field(service::MCP_PATH)
        .field(service::ENDPOINT_URL)
        .field(service::SEND_NODE_DID)
        .field(service::ENABLED)
        .field(service::TAGS)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServiceInventoryRow {
    #[serde(flatten)]
    pub config: gents_documents::ToolServiceRegistry,
    #[serde(rename = "_docID")]
    pub doc_id: String,
    pub status: Option<String>,
    pub version: Option<String>,
    pub updated_at: Option<String>,
}

fn inventory_selection() -> Selection<service::Doc> {
    service_selection()
        .field(service::DOC_ID)
        .field(service::STATUS)
        .field(service::VERSION)
        .field(service::UPDATED_AT)
}

pub struct ServiceInventory<'a> {
    pub node_did: &'a str,
    pub service_id: Option<&'a str>,
    pub enabled_only: bool,
}

impl View for ServiceInventory<'_> {
    type Collection = service::Doc;
    type Row = ServiceInventoryRow;

    fn query(&self, _reader_did: &str) -> Query<Self::Collection> {
        service_query(
            inventory_selection(),
            self.node_did,
            self.service_id,
            self.enabled_only,
        )
    }
}

pub struct ServiceDocuments<'a> {
    pub node_did: &'a str,
    pub doc_ids: &'a [String],
}

impl View for ServiceDocuments<'_> {
    type Collection = service::Doc;
    type Row = ServiceInventoryRow;

    fn query(&self, _reader_did: &str) -> Query<Self::Collection> {
        Query::select(inventory_selection())
            .filter(Filter::eq(service::NODE_DID, self.node_did))
            .filter(Filter::any_of(service::DOC_ID, self.doc_ids.iter()))
            .order_by(service::SERVICE_ID, Direction::Asc)
    }
}

pub struct Health<'a> {
    pub node_did: &'a str,
}

impl View for Health<'_> {
    type Collection = health::Doc;
    type Row = gents_protocol::row::ToolServiceHealthStateRow;

    fn query(&self, _reader_did: &str) -> Query<Self::Collection> {
        Query::select(
            Selection::doc_id()
                .field(health::NODE_DID)
                .field(health::SERVICE_ID)
                .field(health::ENDPOINT)
                .field(health::STATUS)
                .field(health::TOOL_COUNT)
                .field(health::FAILURE_COUNT)
                .field(health::K_MAX)
                .field(health::BACKOFF_UNTIL)
                .field(health::LAST_PROBE_AT)
                .field(health::LAST_SEEN)
                .field(health::LAST_ERROR_CLASS)
                .field(health::LAST_ERROR_MESSAGE)
                .field(health::UPDATED_AT),
        )
        .filter(Filter::eq(health::NODE_DID, self.node_did))
        .order_by(health::SERVICE_ID, Direction::Asc)
        .order_by(health::UPDATED_AT, Direction::Desc)
        .order_by(health::DOC_ID, Direction::Asc)
    }

    fn finish(&self, mut rows: Vec<Self::Row>) -> Vec<Self::Row> {
        let mut seen = rapidhash::RapidHashSet::default();
        rows.retain(|row| seen.insert(row.service_id.clone()));
        rows
    }
}

pub struct ServicePage<'a> {
    pub node_did: &'a str,
    pub cursor: Option<&'a str>,
    pub limit: u64,
}

impl View for ServicePage<'_> {
    type Collection = service::Doc;
    type Row = gents_documents::ToolServiceRegistry;

    fn query(&self, reader_did: &str) -> Query<Self::Collection> {
        let mut query = Services {
            node_did: self.node_did,
            service_id: None,
            enabled_only: false,
        }
        .query(reader_did)
        .limit(self.limit);
        if let Some(cursor) = self.cursor {
            query = query.filter(Filter::gt(service::SERVICE_ID, cursor));
        }
        query
    }
}
