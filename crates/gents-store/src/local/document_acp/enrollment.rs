use super::{publication, DocumentAcp, Publication, Result};

/// Whether serving creates the runtime's signed `Network` row
/// (`Identity.NodePublication.enrollmentRoot`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnrollmentRoot {
    Created,
    Absent,
}

pub fn enrollment_root(provider: DocumentAcp) -> EnrollmentRoot {
    match publication(provider) {
        Publication::Proceeds => EnrollmentRoot::Created,
        Publication::AwaitsTransportGrants => EnrollmentRoot::Absent,
    }
}

/// A request that enrolls or pairs a device through the enrollment root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnrollmentRequest {
    Offer,
    Decision,
    Pending,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnrollmentAnswer {
    Served,
    PeerNetworkingUnavailable,
}

/// `Identity.NodePublication.enrollment`.
pub fn enrollment(provider: DocumentAcp, _: EnrollmentRequest) -> EnrollmentAnswer {
    match enrollment_root(provider) {
        EnrollmentRoot::Created => EnrollmentAnswer::Served,
        EnrollmentRoot::Absent => EnrollmentAnswer::PeerNetworkingUnavailable,
    }
}

/// Refuses `request` on `node` before anything is read or written when the
/// home serves without an enrollment root.
pub async fn ensure_enrollment(node: &defra_node::Node, _: EnrollmentRequest) -> Result<()> {
    super::ensure_node_publication(node).await
}
