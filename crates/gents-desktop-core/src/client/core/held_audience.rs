use gents_protocol::delegation_audience::HeldAudienceChange;

use super::sync_state::ClientSyncStateOwner;
use crate::client::PeerRecord;

/// Read `saved`'s public status and keep the delegation audience it reports
/// with the record (`Identity.HeldAudience.change`). A status that cannot be
/// read leaves the record as it is.
pub(super) async fn refresh_delegation_audience(
    sync_state: &ClientSyncStateOwner,
    saved: &PeerRecord,
) -> (HeldAudienceChange, PeerRecord) {
    let unchanged = || (HeldAudienceChange::Unchanged, saved.clone());
    let reported = match crate::local_runtime::public_delegation_audience(saved).await {
        Ok(Some(reported)) => reported.audience,
        Ok(None) => return unchanged(),
        Err(error) => {
            tracing::debug!(
                directory_peer_id = %saved.peer_id,
                error = %format!("{error:#}"),
                "home status unreadable; keeping the audience held for it"
            );
            return unchanged();
        }
    };
    match sync_state
        .observe_delegation_audience(saved, &reported)
        .await
    {
        Ok(observed) => observed,
        Err(error) => {
            tracing::warn!(
                directory_peer_id = %saved.peer_id,
                error = %format!("{error:#}"),
                "failed to keep the audience a home reports"
            );
            unchanged()
        }
    }
}

impl super::ClientCore {
    fn saved_record(&self, node_did: &str) -> Option<PeerRecord> {
        self.sync_state
            .records()
            .into_iter()
            .find(|record| record.node_did == node_did)
    }

    /// Read the public status of the home `node_did` names again and keep the
    /// audience it reports.
    pub async fn refresh_delegation_audience(&self, node_did: &str) -> HeldAudienceChange {
        match self.saved_record(node_did) {
            Some(saved) => {
                refresh_delegation_audience(&self.sync_state, &saved)
                    .await
                    .0
            }
            None => HeldAudienceChange::Unchanged,
        }
    }

    /// Keep the audience a status the desktop read for `node_did` reports.
    pub async fn observe_runtime_status(
        &self,
        node_did: &str,
        status: &serde_json::Value,
    ) -> HeldAudienceChange {
        let Some(saved) = self.saved_record(node_did) else {
            return HeldAudienceChange::Unchanged;
        };
        let reported =
            gents_protocol::delegation_audience::ReportedHome::of_home(status, node_did).audience;
        match self
            .sync_state
            .observe_delegation_audience(&saved, &reported)
            .await
        {
            Ok((change, _)) => change,
            Err(error) => {
                tracing::warn!(
                    directory_peer_id = %saved.peer_id,
                    error = %format!("{error:#}"),
                    "failed to keep the audience a home reports"
                );
                HeldAudienceChange::Unchanged
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use gents_protocol::delegation_audience::{DelegationAudience, HeldAudienceChange};
    use serde_json::json;
    use wiremock::matchers::{header_exists, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::refresh_delegation_audience;
    use crate::client::core::sync_state::ClientSyncStateOwner;
    use crate::client::PeerRecord;

    fn audience(deployment: u64) -> DelegationAudience {
        DelegationAudience {
            worker: "did:key:z6MkhaXgBZDvotDkL5257faiztiGiC2QtKLGpbnnEGta2doK".into(),
            deployment,
            genesis_id: None,
        }
    }

    async fn status_reports(server: &MockServer, reported: serde_json::Value) {
        server.reset().await;
        Mock::given(method("GET"))
            .and(path("/status"))
            .and(header_exists("authorization"))
            .respond_with(ResponseTemplate::new(500))
            .mount(server)
            .await;
        Mock::given(method("GET"))
            .and(path("/status"))
            .respond_with(ResponseTemplate::new(200).set_body_json(reported))
            .mount(server)
            .await;
    }

    #[tokio::test]
    async fn a_record_follows_the_audience_its_home_reports() {
        let server = MockServer::start().await;
        let mut record = PeerRecord::new("Home", "iroh://peer", "did:key:home");
        record.graphql = Some(format!("{}/api/v0/graphql", server.uri()));
        let (_dir, sync_state) = ClientSyncStateOwner::for_test(vec![record.clone()], vec![]).await;
        let local = json!({ "node_did": "did:key:home" });
        let named = |deployment| json!({ "node_did": "did:key:home", "delegation_audience": audience(deployment) });
        let unavailable = json!({ "node_did": "did:key:home", "delegation_audience": null });

        let steps = [
            (local.clone(), HeldAudienceChange::Unchanged, None),
            (
                named(1),
                HeldAudienceChange::Learned(audience(1)),
                Some(audience(1)),
            ),
            (named(1), HeldAudienceChange::Unchanged, Some(audience(1))),
            (
                unavailable,
                HeldAudienceChange::Unchanged,
                Some(audience(1)),
            ),
            (
                named(2),
                HeldAudienceChange::Changed(audience(2)),
                Some(audience(2)),
            ),
            (
                json!({ "node_did": "did:key:other", "delegation_audience": audience(3) }),
                HeldAudienceChange::Unchanged,
                Some(audience(2)),
            ),
            (local, HeldAudienceChange::Forgotten, None),
        ];
        for (status, change, held) in steps {
            status_reports(&server, status).await;
            let (observed, current) = refresh_delegation_audience(&sync_state, &record).await;
            assert_eq!(observed, change);
            assert_eq!(current.delegation_audience, held);
            assert_eq!(
                sync_state.records(),
                vec![current.clone()],
                "the change is persisted"
            );
            record = current;
        }
    }

    #[tokio::test]
    async fn an_unreadable_status_keeps_the_record() {
        let mut record = PeerRecord::new("Home", "iroh://peer", "did:key:home");
        record.graphql = Some("http://127.0.0.1:9/api/v0/graphql".into());
        record.delegation_audience = Some(audience(1));
        let (_dir, sync_state) = ClientSyncStateOwner::for_test(vec![record.clone()], vec![]).await;
        assert_eq!(
            refresh_delegation_audience(&sync_state, &record).await,
            (HeldAudienceChange::Unchanged, record)
        );
    }
}
