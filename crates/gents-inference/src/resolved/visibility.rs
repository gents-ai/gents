use std::sync::Arc;

use super::{SelectedConfiguration, SelectedConfigurationDocument};
use gents_store::config_client::{ConfigAccess, ConfigClientError, TransactionFailure};
use gents_store::graphql::escape_graphql_string;

#[derive(Debug, thiserror::Error)]
pub enum ConfigurationVisibilityError {
    #[error("selected configuration is unavailable")]
    Unavailable,
    #[error("configuration visibility query returned an invalid response")]
    InvalidResponse,
    #[error(fmt = gents_protocol::error_chain::held)]
    Version(#[from] gents_store::graphql::GraphqlResponseError),
    #[error(fmt = gents_protocol::error_chain::held)]
    Access(#[from] ConfigClientError),
}

impl TransactionFailure for ConfigurationVisibilityError {
    fn config_client_error(&self) -> Option<&ConfigClientError> {
        match self {
            Self::Access(error) => Some(error),
            Self::Version(error) => error.config_client_error(),
            _ => None,
        }
    }
}

impl SelectedConfiguration {
    /// Recheck the admitted selection's visibility under its Agent DID.
    /// Running work retains its resolved values; access to another generation
    /// or to the runtime's snapshot does not authorize these documents.
    pub async fn check(&self) -> Result<(), ConfigurationVisibilityError> {
        self.validate()?;
        self.check_documents(self.documents.clone(), false)
            .await
            .map(|_| ())
    }

    pub async fn check_current(&self) -> Result<bool, ConfigurationVisibilityError> {
        self.validate()?;
        self.check_documents(self.documents.clone(), true).await
    }

    /// Whether the selected documents of `collection` are still at the
    /// versions this generation resolved.
    pub async fn current_in(
        &self,
        collection: gents_store::collection::Collection,
    ) -> Result<bool, ConfigurationVisibilityError> {
        self.validate()?;
        let documents = self
            .documents
            .iter()
            .filter(|document| document.collection == collection)
            .cloned()
            .collect();
        self.check_documents(documents, true).await
    }

    fn validate(&self) -> Result<(), ConfigurationVisibilityError> {
        if !matches!(self.documents.first(), Some(doc) if doc.collection == gents_store::collection::Collection::Agent)
            || self.documents.iter().any(|doc| {
                doc.doc_id.is_empty()
                    || !matches!(
                        doc.collection,
                        gents_store::collection::Collection::Agent
                            | gents_store::collection::Collection::AgentContext
                            | gents_store::collection::Collection::Tools
                            | gents_store::collection::Collection::Compaction
                            | gents_store::collection::Collection::InferenceProfile
                            | gents_store::collection::Collection::InferenceBackend
                            | gents_store::collection::Collection::Skill
                            | gents_store::collection::Collection::InferenceSampling
                            | gents_store::collection::Collection::InferenceExecution
                            | gents_store::collection::Collection::InferenceRetryPolicy
                    )
            })
        {
            return Err(ConfigurationVisibilityError::Unavailable);
        }
        Ok(())
    }

    /// Resolve the cached Skill ID only within this generation, then read its
    /// physical document as the selected Agent before returning instructions.
    pub async fn check_skill(&self, skill_id: &str) -> Result<(), ConfigurationVisibilityError> {
        let documents = self
            .documents
            .iter()
            .filter(|doc| {
                doc.collection == gents_store::collection::Collection::Skill
                    && doc.logical_id.as_deref() == Some(skill_id)
            })
            .cloned()
            .collect::<Vec<_>>();
        if documents.len() != 1 || documents[0].doc_id.is_empty() {
            return Err(ConfigurationVisibilityError::Unavailable);
        }
        self.check_documents(documents, false).await.map(|_| ())
    }

    pub async fn check_skills(
        &self,
        skill_ids: &[String],
    ) -> Result<(), ConfigurationVisibilityError> {
        let mut documents = Vec::with_capacity(skill_ids.len());
        for skill_id in skill_ids {
            let matching = self
                .documents
                .iter()
                .filter(|doc| {
                    doc.collection == gents_store::collection::Collection::Skill
                        && doc.logical_id.as_deref() == Some(skill_id.as_str())
                })
                .collect::<Vec<_>>();
            if matching.len() != 1 || matching[0].doc_id.is_empty() {
                return Err(ConfigurationVisibilityError::Unavailable);
            }
            documents.push((*matching[0]).clone());
        }
        self.check_documents(documents, false).await.map(|_| ())
    }

    async fn check_documents(
        &self,
        documents: Vec<SelectedConfigurationDocument>,
        require_current: bool,
    ) -> Result<bool, ConfigurationVisibilityError> {
        let access = ConfigAccess::local_as(Arc::clone(&self.node), self.actor.clone());
        access.transact_readonly("provider_input.configuration_visibility", |txn| {
            let documents = documents.clone();
            Box::pin(async move {
                let mut current = true;
                for document in documents {
                    let name = document.collection.graphql_type();
                    let version = if require_current { "_version { cid height fieldName }" } else { "" };
                    let query = format!("{{ {name}(filter: {{_docID: {{_eq: \"{}\"}}}}, limit: 2) {{_docID {version}}} }}", escape_graphql_string(&document.doc_id));
                    let response = txn.execute(&query).await?;
                    let rows = response.get("data").and_then(|data| data.get(name))
                        .and_then(serde_json::Value::as_array)
                        .ok_or(ConfigurationVisibilityError::InvalidResponse)?;
                    if !matches!(rows.as_slice(), [row] if row["_docID"].as_str() == Some(document.doc_id.as_str())) {
                        return Err(ConfigurationVisibilityError::Unavailable);
                    }
                    if require_current {
                        let observed = gents_store::graphql::document_composite_version(&rows[0], "configuration admission")?
                            .ok_or(ConfigurationVisibilityError::InvalidResponse)?;
                        current &= !document.version.is_empty() && observed.cid == document.version;
                    }
                }
                Ok(current)
            })
        }).await
    }
}
