use super::*;

/// A status address is observation metadata. Only the existing enrollment owner
/// can activate its matching request; this entry supplies no local host or
/// HTTP operator authority.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct EnrollmentEndpoint {
    peer_id: String,
    node_did: String,
    request_digest: String,
    expires_at: String,
    graphql: String,
}

impl EnrollmentEndpoint {
    fn fresh(&self, now: DateTime<Utc>) -> bool {
        DateTime::parse_from_rfc3339(&self.expires_at).is_ok_and(|expires| expires > now)
    }
}

impl PeerDirectory {
    pub(in crate::client) async fn remember_enrollment_endpoint(
        &mut self,
        peer_id: &str,
        node_did: &str,
        request_digest: &str,
        expires_at: &str,
        endpoint: &str,
    ) -> Result<()> {
        let graphql = crate::local_runtime::runtime_graphql_url(endpoint)?;
        let url = reqwest::Url::parse(&graphql)?;
        anyhow::ensure!(
            url.username().is_empty() && url.password().is_none(),
            "enrollment status addresses cannot contain credentials"
        );
        let observation = EnrollmentEndpoint {
            peer_id: normalize_non_empty("peer_id", peer_id)?.to_owned(),
            node_did: normalize_non_empty("node_did", node_did)?.to_owned(),
            request_digest: normalize_non_empty("request_digest", request_digest)?.to_owned(),
            expires_at: expires_at.to_owned(),
            graphql,
        };
        let now = Utc::now();
        anyhow::ensure!(
            observation.fresh(now),
            "enrollment status observation has expired"
        );
        let mut candidate = self.clone();
        candidate.enrollment_endpoints.retain(|previous| {
            previous.fresh(now) && previous.request_digest != observation.request_digest
        });
        candidate.enrollment_endpoints.push(observation);
        self.commit(candidate).await
    }

    pub(super) fn take_enrollment_endpoint(
        &mut self,
        peer_id: &str,
        node_did: &str,
        request_digest: &str,
    ) -> Option<String> {
        let now = Utc::now();
        self.enrollment_endpoints
            .retain(|observation| observation.fresh(now));
        let position = self.enrollment_endpoints.iter().position(|observation| {
            observation.peer_id == peer_id
                && observation.node_did == node_did
                && observation.request_digest == request_digest
        })?;
        Some(self.enrollment_endpoints.remove(position).graphql)
    }
}

#[cfg(test)]
mod tests;
