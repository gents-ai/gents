//! HTTP transport implementation for `RemoteP2pAdmin`.

use std::time::Duration;

use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};

use super::trait_def::{
    RemoteP2pAdmin, RemoteP2pAdminError, RemoteP2pAdminResult, RemoteReplicator,
};

const DEFAULT_RPC_TIMEOUT: Duration = Duration::from_secs(10);

pub struct HttpRemoteP2pAdmin {
    /// `http://host:port/api/v0`, used to compose `/p2p/*` URLs.
    api_base: String,
    client: Client,
}

impl HttpRemoteP2pAdmin {
    pub fn new(graphql_url: &str) -> RemoteP2pAdminResult<Self> {
        let trimmed = graphql_url.trim_end_matches('/');
        let api_base = trimmed
            .strip_suffix("/graphql")
            .ok_or_else(|| {
                RemoteP2pAdminError::LocalError(format!(
                    "expected GraphQL URL ending in /graphql, got {trimmed}"
                ))
            })?
            .to_string();
        let client = Client::builder()
            .timeout(DEFAULT_RPC_TIMEOUT)
            .build()
            .map_err(|e| RemoteP2pAdminError::LocalError(format!("reqwest build: {e}")))?;
        Ok(Self { api_base, client })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.api_base, path)
    }
}

#[derive(Debug, Serialize)]
struct AddReplicatorBody<'a> {
    #[serde(rename = "Collections")]
    collections: &'a [String],
    #[serde(rename = "Addresses")]
    addresses: &'a [String],
}

#[derive(Debug, Serialize)]
struct DeleteReplicatorBody<'a> {
    #[serde(rename = "Collections")]
    collections: &'a [String],
    #[serde(rename = "ID")]
    id: &'a str,
}

#[derive(Debug, Serialize)]
struct SyncDocumentsBody<'a> {
    #[serde(rename = "collectionName")]
    collection_name: &'a str,
    #[serde(rename = "docIDs")]
    doc_ids: &'a [String],
    timeout: String,
}

#[derive(Debug, Serialize)]
struct SyncVersionsBody<'a> {
    #[serde(rename = "versionIDs")]
    version_ids: &'a [String],
    timeout: String,
}

#[derive(Debug, Serialize)]
struct SyncBranchableBody<'a> {
    #[serde(rename = "collectionID")]
    collection_id: &'a str,
    timeout: String,
}

#[async_trait]
impl RemoteP2pAdmin for HttpRemoteP2pAdmin {
    async fn peer_info(&self) -> RemoteP2pAdminResult<Vec<String>> {
        let resp = self
            .client
            .get(self.url("/p2p/info"))
            .send()
            .await
            .map_err(map_reqwest_err)?;
        let resp = check_status(resp).await?;
        resp.json::<Vec<String>>()
            .await
            .map_err(|e| RemoteP2pAdminError::RpcError(format!("decoding peer_info: {e}")))
    }

    async fn active_peers(&self) -> RemoteP2pAdminResult<Vec<String>> {
        let resp = self
            .client
            .get(self.url("/p2p/active-peers"))
            .send()
            .await
            .map_err(map_reqwest_err)?;
        let resp = check_status(resp).await?;
        resp.json::<Vec<String>>()
            .await
            .map_err(|e| RemoteP2pAdminError::RpcError(format!("decoding active_peers: {e}")))
    }

    async fn connect(&self, addresses: &[String]) -> RemoteP2pAdminResult<()> {
        let resp = self
            .client
            .post(self.url("/p2p/connect"))
            .json(addresses)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        check_status(resp).await?;
        Ok(())
    }

    async fn list_replicators(&self) -> RemoteP2pAdminResult<Vec<RemoteReplicator>> {
        #[derive(Deserialize)]
        struct WireReplicator {
            #[serde(default)]
            id: Option<String>,
            #[serde(default)]
            collections: Vec<String>,
            #[serde(default)]
            address: Option<String>,
        }

        let resp = self
            .client
            .get(self.url("/p2p/replicators"))
            .send()
            .await
            .map_err(map_reqwest_err)?;
        let resp = check_status(resp).await?;
        let wire: Vec<WireReplicator> = resp.json().await.map_err(|e| {
            RemoteP2pAdminError::RpcError(format!("decoding list_replicators: {e}"))
        })?;
        Ok(wire
            .into_iter()
            .map(|w| RemoteReplicator {
                id: w.id,
                collections: w.collections,
                address: w.address,
            })
            .collect())
    }

    async fn add_replicator(
        &self,
        addresses: &[String],
        collections: &[String],
    ) -> RemoteP2pAdminResult<()> {
        let body = AddReplicatorBody {
            collections,
            addresses,
        };
        let resp = self
            .client
            .post(self.url("/p2p/replicators"))
            .json(&body)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        check_status(resp).await?;
        Ok(())
    }

    async fn delete_replicator(
        &self,
        id: &str,
        collections: &[String],
    ) -> RemoteP2pAdminResult<()> {
        let body = DeleteReplicatorBody { collections, id };
        let resp = self
            .client
            .delete(self.url("/p2p/replicators"))
            .json(&body)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        check_status(resp).await?;
        Ok(())
    }

    async fn list_p2p_collections(&self) -> RemoteP2pAdminResult<Vec<String>> {
        let resp = self
            .client
            .get(self.url("/p2p/collections"))
            .send()
            .await
            .map_err(map_reqwest_err)?;
        let resp = check_status(resp).await?;
        resp.json::<Vec<String>>().await.map_err(|e| {
            RemoteP2pAdminError::RpcError(format!("decoding list_p2p_collections: {e}"))
        })
    }

    async fn add_p2p_collections(&self, collections: &[String]) -> RemoteP2pAdminResult<()> {
        let resp = self
            .client
            .post(self.url("/p2p/collections"))
            .json(collections)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        check_status(resp).await?;
        Ok(())
    }

    async fn delete_p2p_collections(&self, collections: &[String]) -> RemoteP2pAdminResult<()> {
        let resp = self
            .client
            .delete(self.url("/p2p/collections"))
            .json(collections)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        check_status(resp).await?;
        Ok(())
    }

    async fn list_p2p_documents(&self) -> RemoteP2pAdminResult<Vec<String>> {
        let resp = self
            .client
            .get(self.url("/p2p/documents"))
            .send()
            .await
            .map_err(map_reqwest_err)?;
        let resp = check_status(resp).await?;
        resp.json::<Vec<String>>()
            .await
            .map_err(|e| RemoteP2pAdminError::RpcError(format!("decoding list_p2p_documents: {e}")))
    }

    async fn add_p2p_documents(&self, doc_ids: &[String]) -> RemoteP2pAdminResult<()> {
        let resp = self
            .client
            .post(self.url("/p2p/documents"))
            .json(doc_ids)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        check_status(resp).await?;
        Ok(())
    }

    async fn delete_p2p_documents(&self, doc_ids: &[String]) -> RemoteP2pAdminResult<()> {
        let resp = self
            .client
            .delete(self.url("/p2p/documents"))
            .json(doc_ids)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        check_status(resp).await?;
        Ok(())
    }

    async fn sync_documents(
        &self,
        collection_name: &str,
        doc_ids: &[String],
        timeout: Option<Duration>,
    ) -> RemoteP2pAdminResult<()> {
        let body = SyncDocumentsBody {
            collection_name,
            doc_ids,
            timeout: format_timeout(timeout),
        };
        let resp = self
            .client
            .post(self.url("/p2p/documents/sync"))
            .json(&body)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        check_status(resp).await?;
        Ok(())
    }

    async fn sync_collection_versions(
        &self,
        version_ids: &[String],
        timeout: Option<Duration>,
    ) -> RemoteP2pAdminResult<()> {
        let body = SyncVersionsBody {
            version_ids,
            timeout: format_timeout(timeout),
        };
        let resp = self
            .client
            .post(self.url("/p2p/collections/sync-versions"))
            .json(&body)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        check_status(resp).await?;
        Ok(())
    }

    async fn sync_branchable_collection(
        &self,
        collection_id: &str,
        timeout: Option<Duration>,
    ) -> RemoteP2pAdminResult<()> {
        let body = SyncBranchableBody {
            collection_id,
            timeout: format_timeout(timeout),
        };
        let resp = self
            .client
            .post(self.url("/p2p/collections/sync-branchable"))
            .json(&body)
            .send()
            .await
            .map_err(map_reqwest_err)?;
        check_status(resp).await?;
        Ok(())
    }
}

fn format_timeout(t: Option<Duration>) -> String {
    match t {
        Some(d) => format!("{}s", d.as_secs()),
        None => String::new(),
    }
}

fn map_reqwest_err(e: reqwest::Error) -> RemoteP2pAdminError {
    if e.is_timeout() {
        RemoteP2pAdminError::RpcTimeout
    } else {
        RemoteP2pAdminError::RpcError(e.to_string())
    }
}

async fn check_status(resp: reqwest::Response) -> RemoteP2pAdminResult<reqwest::Response> {
    let status = resp.status();
    if status.is_success() {
        Ok(resp)
    } else if status == reqwest::StatusCode::UNAUTHORIZED
        || status == reqwest::StatusCode::FORBIDDEN
    {
        Err(RemoteP2pAdminError::RemoteUnauthorized)
    } else if status == reqwest::StatusCode::NOT_FOUND {
        let body = resp.text().await.unwrap_or_default();
        Err(RemoteP2pAdminError::RemoteNotFound(body))
    } else {
        let body = resp.text().await.unwrap_or_default();
        Err(RemoteP2pAdminError::RpcError(format!(
            "status {status}: {body}"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{body_json, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn admin_for(server: &MockServer) -> HttpRemoteP2pAdmin {
        let graphql = format!("{}/api/v0/graphql", server.uri());
        HttpRemoteP2pAdmin::new(&graphql).expect("admin constructs")
    }

    #[tokio::test]
    async fn peer_info_returns_addresses() {
        let server = MockServer::start().await;
        let body = serde_json::json!(["/ip4/1.2.3.4/tcp/9000/p2p/peer1"]);
        Mock::given(method("GET"))
            .and(path("/api/v0/p2p/info"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;

        let admin = admin_for(&server);
        assert_eq!(
            admin.peer_info().await.expect("info"),
            vec!["/ip4/1.2.3.4/tcp/9000/p2p/peer1".to_string()]
        );
    }

    #[tokio::test]
    async fn active_peers_returns_list() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v0/p2p/active-peers"))
            .respond_with(ResponseTemplate::new(200).set_body_json(vec!["peer1", "peer2"]))
            .mount(&server)
            .await;

        let admin = admin_for(&server);
        assert_eq!(
            admin.active_peers().await.expect("active"),
            vec!["peer1".to_string(), "peer2".to_string()]
        );
    }

    #[tokio::test]
    async fn connect_posts_addresses() {
        let server = MockServer::start().await;
        let expected = serde_json::json!(["/ip4/1.2.3.4/tcp/9000/p2p/peer1"]);
        Mock::given(method("POST"))
            .and(path("/api/v0/p2p/connect"))
            .and(body_json(expected))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let admin = admin_for(&server);
        admin
            .connect(&["/ip4/1.2.3.4/tcp/9000/p2p/peer1".into()])
            .await
            .expect("connect");
    }

    #[tokio::test]
    async fn list_replicators_returns_remote_replicators() {
        let server = MockServer::start().await;
        let body = serde_json::json!([
            {
                "id": "peer1",
                "collections": ["c1"],
                "address": "/ip4/1.2.3.4/tcp/9000/p2p/peer1",
                "status": 1,
                "lastStatusChange": null
            }
        ]);
        Mock::given(method("GET"))
            .and(path("/api/v0/p2p/replicators"))
            .respond_with(ResponseTemplate::new(200).set_body_json(body))
            .mount(&server)
            .await;

        let admin = admin_for(&server);
        let reps = admin.list_replicators().await.expect("list");
        assert_eq!(reps.len(), 1);
        assert_eq!(reps[0].id.as_deref(), Some("peer1"));
        assert_eq!(reps[0].collections, vec!["c1".to_string()]);
    }

    #[tokio::test]
    async fn add_replicator_posts_addresses_and_collections() {
        let server = MockServer::start().await;
        let expected = serde_json::json!({
            "Collections": ["c1"],
            "Addresses": ["/ip4/1.2.3.4/tcp/9000/p2p/peer1"]
        });
        Mock::given(method("POST"))
            .and(path("/api/v0/p2p/replicators"))
            .and(body_json(expected))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let admin = admin_for(&server);
        admin
            .add_replicator(&["/ip4/1.2.3.4/tcp/9000/p2p/peer1".into()], &["c1".into()])
            .await
            .expect("add_replicator");
    }

    #[tokio::test]
    async fn delete_replicator_deletes_with_id_and_collections() {
        let server = MockServer::start().await;
        let expected = serde_json::json!({
            "Collections": ["c1"],
            "ID": "peer1"
        });
        Mock::given(method("DELETE"))
            .and(path("/api/v0/p2p/replicators"))
            .and(body_json(expected))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let admin = admin_for(&server);
        admin
            .delete_replicator("peer1", &["c1".into()])
            .await
            .expect("delete_replicator");
    }

    #[tokio::test]
    async fn list_p2p_collections_returns_remote_list() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v0/p2p/collections"))
            .respond_with(ResponseTemplate::new(200).set_body_json(vec!["c1", "c2"]))
            .mount(&server)
            .await;

        let admin = admin_for(&server);
        let cols = admin.list_p2p_collections().await.expect("list");
        assert_eq!(cols, vec!["c1".to_string(), "c2".to_string()]);
    }

    #[tokio::test]
    async fn add_p2p_collections_posts_the_body() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v0/p2p/collections"))
            .and(body_json(vec!["c1", "c2"]))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let admin = admin_for(&server);
        admin
            .add_p2p_collections(&["c1".into(), "c2".into()])
            .await
            .expect("add");
    }

    #[tokio::test]
    async fn delete_p2p_collections_deletes_the_body() {
        let server = MockServer::start().await;
        Mock::given(method("DELETE"))
            .and(path("/api/v0/p2p/collections"))
            .and(body_json(vec!["c1"]))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let admin = admin_for(&server);
        admin
            .delete_p2p_collections(&["c1".into()])
            .await
            .expect("delete");
    }

    #[tokio::test]
    async fn list_p2p_documents_returns_list() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/api/v0/p2p/documents"))
            .respond_with(ResponseTemplate::new(200).set_body_json(vec!["doc1", "doc2"]))
            .mount(&server)
            .await;

        let admin = admin_for(&server);
        assert_eq!(
            admin.list_p2p_documents().await.expect("list_docs"),
            vec!["doc1".to_string(), "doc2".to_string()]
        );
    }

    #[tokio::test]
    async fn add_p2p_documents_posts_doc_ids() {
        let server = MockServer::start().await;
        let expected = serde_json::json!(["doc1"]);
        Mock::given(method("POST"))
            .and(path("/api/v0/p2p/documents"))
            .and(body_json(expected))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let admin = admin_for(&server);
        admin
            .add_p2p_documents(&["doc1".into()])
            .await
            .expect("add");
    }

    #[tokio::test]
    async fn delete_p2p_documents_deletes_doc_ids() {
        let server = MockServer::start().await;
        let expected = serde_json::json!(["doc1"]);
        Mock::given(method("DELETE"))
            .and(path("/api/v0/p2p/documents"))
            .and(body_json(expected))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let admin = admin_for(&server);
        admin
            .delete_p2p_documents(&["doc1".into()])
            .await
            .expect("delete");
    }

    #[tokio::test]
    async fn sync_documents_posts_request_body() {
        let server = MockServer::start().await;
        let expected = serde_json::json!({
            "collectionName": "Foo",
            "docIDs": ["doc1"],
            "timeout": "5s"
        });
        Mock::given(method("POST"))
            .and(path("/api/v0/p2p/documents/sync"))
            .and(body_json(expected))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let admin = admin_for(&server);
        admin
            .sync_documents("Foo", &["doc1".into()], Some(Duration::from_secs(5)))
            .await
            .expect("sync_documents");
    }

    #[tokio::test]
    async fn sync_collection_versions_posts_versions() {
        let server = MockServer::start().await;
        let expected = serde_json::json!({
            "versionIDs": ["v1"],
            "timeout": ""
        });
        Mock::given(method("POST"))
            .and(path("/api/v0/p2p/collections/sync-versions"))
            .and(body_json(expected))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let admin = admin_for(&server);
        admin
            .sync_collection_versions(&["v1".into()], None)
            .await
            .expect("sync_collection_versions");
    }

    #[tokio::test]
    async fn sync_branchable_collection_posts_collection_id() {
        let server = MockServer::start().await;
        let expected = serde_json::json!({
            "collectionID": "col1",
            "timeout": ""
        });
        Mock::given(method("POST"))
            .and(path("/api/v0/p2p/collections/sync-branchable"))
            .and(body_json(expected))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let admin = admin_for(&server);
        admin
            .sync_branchable_collection("col1", None)
            .await
            .expect("sync_branchable");
    }
}
