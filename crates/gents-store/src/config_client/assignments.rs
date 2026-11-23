use super::ConfigAccess;

#[derive(Debug, thiserror::Error)]
pub enum AssignmentReadError {
    #[error("certified assignment reads require an embedded Vera provider")]
    Unsupported,
    #[error("certified assignment read failed")]
    Provider(#[source] defra_node::Error),
}

impl ConfigAccess {
    /// Reads every directly assigned object ID for the given policy, resource,
    /// relation, subject and issuer at one certified position. The local node
    /// checks DAC status permission and requires the caller to be the subject.
    ///
    /// Pass the last accepted position in `not_before` after a restart. A
    /// successful empty set establishes absence at the returned position.
    /// Proof limits, unavailable evidence and a position below the requested
    /// floor return errors. Preserve the last accepted view on error.
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
