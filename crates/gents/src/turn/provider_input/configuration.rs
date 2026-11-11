use std::sync::Arc;

#[cfg(test)]
use crate::inference::resolved::SelectedConfigurationDocument;
use crate::inference::resolved::{ResolvedConfiguration, SelectedConfiguration};
use crate::store::config_client::{ConfigAccess, ConfigClientError, TransactionFailure};
use crate::store::graphql::escape_graphql_string;
use gents_loop::loop_stream::{ProviderConfigurationGuard, ProviderConfigurationGuardError};

#[derive(Debug, thiserror::Error)]
pub enum ConfigurationVisibilityError {
    #[error("selected configuration is unavailable")]
    Unavailable,
    #[error("configuration visibility query returned an invalid response")]
    InvalidResponse,
    #[error(transparent)]
    Access(#[from] ConfigClientError),
}

impl TransactionFailure for ConfigurationVisibilityError {
    fn config_client_error(&self) -> Option<&ConfigClientError> {
        match self {
            Self::Access(error) => Some(error),
            _ => None,
        }
    }
}

impl SelectedConfiguration {
    /// Observe the exact resolved generation under its Agent DID. Each check
    /// is a native read; access to another generation or to the runtime's
    /// snapshot does not authorize these documents.
    pub(crate) async fn check(&self) -> Result<(), ConfigurationVisibilityError> {
        if !matches!(self.documents.first(), Some(doc) if doc.collection == crate::Collection::Agent)
            || self.documents.iter().any(|doc| {
                doc.doc_id.is_empty()
                    || !matches!(
                        doc.collection,
                        crate::Collection::Agent
                            | crate::Collection::AgentContext
                            | crate::Collection::Tools
                            | crate::Collection::Compaction
                            | crate::Collection::InferenceProfile
                            | crate::Collection::InferenceBackend
                            | crate::Collection::Skill
                            | crate::Collection::InferenceSampling
                            | crate::Collection::InferenceExecution
                            | crate::Collection::InferenceRetryPolicy
                    )
            })
        {
            return Err(ConfigurationVisibilityError::Unavailable);
        }
        self.check_documents(self.documents.clone()).await
    }

    /// Resolve the cached Skill ID only within this generation, then read its
    /// physical document as the selected Agent before returning instructions.
    pub(crate) async fn check_skill(
        &self,
        skill_id: &str,
    ) -> Result<(), ConfigurationVisibilityError> {
        let documents = self
            .documents
            .iter()
            .filter(|doc| {
                doc.collection == crate::Collection::Skill
                    && doc.logical_id.as_deref() == Some(skill_id)
            })
            .cloned()
            .collect::<Vec<_>>();
        if documents.len() != 1 || documents[0].doc_id.is_empty() {
            return Err(ConfigurationVisibilityError::Unavailable);
        }
        self.check_documents(documents).await
    }

    pub(crate) async fn check_skills(
        &self,
        skill_ids: &[String],
    ) -> Result<(), ConfigurationVisibilityError> {
        let mut documents = Vec::with_capacity(skill_ids.len());
        for skill_id in skill_ids {
            let matching = self
                .documents
                .iter()
                .filter(|doc| {
                    doc.collection == crate::Collection::Skill
                        && doc.logical_id.as_deref() == Some(skill_id.as_str())
                })
                .collect::<Vec<_>>();
            if matching.len() != 1 || matching[0].doc_id.is_empty() {
                return Err(ConfigurationVisibilityError::Unavailable);
            }
            documents.push((*matching[0]).clone());
        }
        self.check_documents(documents).await
    }

    async fn check_documents(
        &self,
        documents: Vec<crate::inference::resolved::SelectedConfigurationDocument>,
    ) -> Result<(), ConfigurationVisibilityError> {
        let access = ConfigAccess::local_as(Arc::clone(&self.node), self.actor.clone());
        access.transact_readonly("provider_input.configuration_visibility", |txn| {
            let documents = documents.clone();
            Box::pin(async move {
                for document in documents {
                    let name = document.collection.graphql_type();
                    let query = format!("{{ {name}(filter: {{_docID: {{_eq: \"{}\"}}}}, limit: 2) {{_docID}} }}", escape_graphql_string(&document.doc_id));
                    let response = txn.execute(&query).await?;
                    let rows = response.get("data").and_then(|data| data.get(name))
                        .and_then(serde_json::Value::as_array)
                        .ok_or(ConfigurationVisibilityError::InvalidResponse)?;
                    if !matches!(rows.as_slice(), [row] if row["_docID"].as_str() == Some(document.doc_id.as_str())) {
                        return Err(ConfigurationVisibilityError::Unavailable);
                    }
                }
                Ok(())
            })
        }).await
    }
}

pub(crate) fn configuration_guard(
    configuration: &ResolvedConfiguration,
) -> Option<ProviderConfigurationGuard> {
    match configuration {
        ResolvedConfiguration::Static => None,
        ResolvedConfiguration::Documents(selection) => {
            let selection = selection.clone();
            Some(Arc::new(move || {
                let selection = selection.clone();
                Box::pin(async move {
                    selection.check().await.map_err(|error| {
                        ProviderConfigurationGuardError::Unavailable(Box::new(error))
                    })
                })
            }))
        }
    }
}

#[cfg(test)]
mod tests;
