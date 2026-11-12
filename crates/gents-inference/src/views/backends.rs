use gents_documents::inference_backend::observation_binding::{
    bind, ObservationBindingError, ObservationKey,
};
use gents_documents::{InferenceBackendMetadata, InferenceBackendObservation};
use gents_protocol::document_query::{
    fields::inference_backend_metadata, fields::inference_backend_observation, Direction, Filter,
    Query, Selection,
};
use gents_protocol::views::View;
use gents_store::config_client::{ConfigAccess, ConfigApplyTxn};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct BackendRow {
    #[serde(flatten)]
    pub metadata: InferenceBackendMetadata,
}

#[derive(Debug, Clone)]
pub struct BackendList {
    node_did: String,
    backend_id: Option<String>,
    after: Option<String>,
    doc_ids: Option<Vec<String>>,
    limit: Option<u64>,
}

impl BackendList {
    pub fn new(node_did: impl Into<String>) -> Self {
        Self {
            node_did: node_did.into(),
            backend_id: None,
            after: None,
            doc_ids: None,
            limit: None,
        }
    }

    pub fn backend_id(mut self, backend_id: impl Into<String>) -> Self {
        self.backend_id = Some(backend_id.into());
        self
    }

    pub fn after(mut self, after: impl Into<String>) -> Self {
        self.after = Some(after.into());
        self
    }

    pub fn doc_ids(mut self, doc_ids: Vec<String>) -> Self {
        self.doc_ids = Some(doc_ids);
        self
    }

    pub fn limit(mut self, limit: u64) -> Self {
        self.limit = Some(limit);
        self
    }
}

impl View for BackendList {
    type Collection = inference_backend_metadata::Doc;
    type Row = BackendRow;

    fn query(&self, _reader_did: &str) -> Query<Self::Collection> {
        let selection = Selection::new()
            .field(inference_backend_metadata::DOC_ID)
            .field(inference_backend_metadata::NODE_DID)
            .field(inference_backend_metadata::BACKEND_ID)
            .field(inference_backend_metadata::NAME)
            .field(inference_backend_metadata::PROVIDER_KIND)
            .field(inference_backend_metadata::OPENAI_WIRE_API)
            .field(inference_backend_metadata::ENDPOINT)
            .field(inference_backend_metadata::AUTH_KIND)
            .field(inference_backend_metadata::CREDENTIAL_ID)
            .field(inference_backend_metadata::CREDENTIAL_VERSION)
            .field(inference_backend_metadata::ENVIRONMENT_VARIABLE)
            .field(inference_backend_metadata::OAUTH_ACCOUNT_REF)
            .field(inference_backend_metadata::OAUTH_ALLOW_OVERAGE)
            .field(inference_backend_metadata::CLAUDE_PLATFORM)
            .field(inference_backend_metadata::CONNECT_TIMEOUT_SECS)
            .field(inference_backend_metadata::DISCOVERY_TIMEOUT_SECS)
            .field(inference_backend_metadata::MAX_CONCURRENT)
            .field(inference_backend_metadata::MAX_QUEUE_DEPTH)
            .field(inference_backend_metadata::ENABLED)
            .field(inference_backend_metadata::TAGS);
        let mut query = Query::select(selection)
            .filter(Filter::eq(
                inference_backend_metadata::NODE_DID,
                &self.node_did,
            ))
            .order_by(inference_backend_metadata::BACKEND_ID, Direction::Asc);
        if let Some(backend_id) = &self.backend_id {
            query = query.filter(Filter::eq(
                inference_backend_metadata::BACKEND_ID,
                backend_id,
            ));
        }
        if let Some(after) = &self.after {
            query = query.filter(Filter::gt(inference_backend_metadata::BACKEND_ID, after));
        }
        if let Some(doc_ids) = &self.doc_ids {
            query = query.filter(Filter::any_of(
                inference_backend_metadata::DOC_ID,
                doc_ids.iter(),
            ));
        }
        if let Some(limit) = self.limit {
            query = query.limit(limit);
        }
        query
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ObservationRow {
    #[serde(rename = "_docID")]
    pub doc_id: String,
    pub node_did: String,
    #[serde(flatten)]
    pub observation: InferenceBackendObservation,
}

impl<'de> Deserialize<'de> for ObservationRow {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Identity {
            #[serde(rename = "_docID")]
            doc_id: String,
            node_did: String,
        }
        let value = serde_json::Value::deserialize(deserializer)?;
        let identity: Identity =
            serde_json::from_value(value.clone()).map_err(serde::de::Error::custom)?;
        let observation = crate::backends::registry::decode_backend_observation(value)
            .map_err(serde::de::Error::custom)?;
        Ok(Self {
            doc_id: identity.doc_id,
            node_did: identity.node_did,
            observation,
        })
    }
}

pub struct ObservationList {
    node_did: String,
    backend_id: Option<String>,
    doc_ids: Option<Vec<String>>,
}

impl ObservationList {
    pub fn new(node_did: impl Into<String>) -> Self {
        Self {
            node_did: node_did.into(),
            backend_id: None,
            doc_ids: None,
        }
    }

    pub fn backend_id(mut self, backend_id: impl Into<String>) -> Self {
        self.backend_id = Some(backend_id.into());
        self
    }

    pub fn doc_ids(mut self, doc_ids: Vec<String>) -> Self {
        self.doc_ids = Some(doc_ids);
        self
    }
}

impl View for ObservationList {
    type Collection = inference_backend_observation::Doc;
    type Row = ObservationRow;

    fn query(&self, _reader_did: &str) -> Query<Self::Collection> {
        let mut query = Query::select(
            Selection::new()
                .field(inference_backend_observation::DOC_ID)
                .field(inference_backend_observation::NODE_DID)
                .field(inference_backend_observation::BACKEND_ID)
                .field(inference_backend_observation::METADATA_DOC_ID)
                .field(inference_backend_observation::CATALOGS)
                .field(inference_backend_observation::CATALOG_FRESHNESS)
                .field(inference_backend_observation::PROBE_STATUS)
                .field(inference_backend_observation::LAST_PROBE),
        )
        .filter(Filter::eq(
            inference_backend_observation::NODE_DID,
            &self.node_did,
        ))
        .order_by(inference_backend_observation::BACKEND_ID, Direction::Asc);
        if let Some(backend_id) = &self.backend_id {
            query = query.filter(Filter::eq(
                inference_backend_observation::BACKEND_ID,
                backend_id,
            ));
        }
        if let Some(doc_ids) = &self.doc_ids {
            query = query.filter(Filter::any_of(
                inference_backend_observation::DOC_ID,
                doc_ids.iter(),
            ));
        }
        query
    }
}

#[derive(Debug, Clone)]
pub struct InventoryRow {
    pub metadata: InferenceBackendMetadata,
    pub observation: Option<InferenceBackendObservation>,
}

#[derive(Debug, thiserror::Error)]
pub enum InventoryError {
    #[error(transparent)]
    View(#[from] gents_store::views::ViewError),
    #[error(transparent)]
    Binding(#[from] ObservationBindingError),
    #[error("inference backend {0:?} has no physical document ID")]
    MissingDocumentId(String),
    #[error("backend catalog scope is ambiguous")]
    Catalog(#[source] anyhow::Error),
    #[error(transparent)]
    Store(#[from] gents_store::config_client::ConfigClientError),
}

impl gents_store::config_client::TransactionFailure for InventoryError {
    fn config_client_error(&self) -> Option<&gents_store::config_client::ConfigClientError> {
        match self {
            Self::Store(error) => Some(error),
            Self::View(gents_store::views::ViewError::Execute(error)) => Some(error),
            _ => None,
        }
    }
}

pub async fn read_inventory(
    access: &ConfigAccess,
    view: &BackendList,
) -> Result<Vec<InventoryRow>, InventoryError> {
    access
        .transact_readonly("views.inference.backends", |txn| {
            Box::pin(async move { read_inventory_in_txn(txn, view).await })
        })
        .await
}

pub async fn read_inventory_in_txn(
    txn: &ConfigApplyTxn<'_>,
    view: &BackendList,
) -> Result<Vec<InventoryRow>, InventoryError> {
    let metadata = txn.read_view(view).await?;
    let observations = txn.read_view(&ObservationList::new(&view.node_did)).await?;
    metadata
        .into_iter()
        .map(|row| {
            let metadata = row.metadata;
            let doc_id = metadata
                .physical_doc_id
                .as_deref()
                .ok_or_else(|| InventoryError::MissingDocumentId(metadata.backend_id.clone()))?;
            let mut observation = bind(
                &metadata.node_did,
                &metadata.backend_id,
                doc_id,
                observations.iter().map(|row| {
                    (
                        ObservationKey {
                            node_did: Some(&row.node_did),
                            backend_id: &row.observation.backend_id,
                            metadata_doc_id: row.observation.metadata_doc_id.as_deref(),
                        },
                        &row.observation,
                    )
                }),
            )?
            .cloned();
            if let Some(observation) = observation.as_mut() {
                let scope = metadata
                    .provider_kind
                    .is_node_scoped_oauth()
                    .then_some(metadata.node_did.as_str());
                observation.catalogs = observation
                    .catalog_for(scope)
                    .map_err(InventoryError::Catalog)?
                    .cloned()
                    .into_iter()
                    .collect();
            }
            Ok(InventoryRow {
                metadata,
                observation,
            })
        })
        .collect()
}
