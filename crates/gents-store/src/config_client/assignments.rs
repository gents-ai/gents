use super::ConfigAccess;

#[derive(Debug, thiserror::Error)]
pub enum AssignmentReadError {
    #[error("certified assignment reads require an embedded Vera provider")]
    Unsupported,
    #[error("certified assignment read failed")]
    Provider(#[source] defra_node::Error),
}

impl ConfigAccess {
    pub async fn certified_assignments(
        &self,
        query: defra_node::AssignmentQuery,
    ) -> Result<defra_node::CertifiedAssignments, AssignmentReadError> {
        match self {
            Self::Local(local) => local
                .node()
                .certified_assignments_as(&local.request_context(), query)
                .await
                .map_err(AssignmentReadError::Provider),
            Self::Graphql(_) => Err(AssignmentReadError::Unsupported),
        }
    }
}
