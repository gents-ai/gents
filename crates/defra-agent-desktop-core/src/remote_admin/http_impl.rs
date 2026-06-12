//! HTTP transport implementation for `RemoteP2pAdmin`.

use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use reqwest::{header::CONTENT_TYPE, Client, Method, RequestBuilder};
use serde::{Deserialize, Serialize};

use defra_agent::agent::p2p_reconcile::{
    RemoteP2pAdmin, RemoteP2pAdminError, RemoteP2pAdminResult, RemoteReplicator,
};

use crate::client::PrincipalIdentity;

const DEFAULT_RPC_TIMEOUT: Duration = Duration::from_secs(10);
pub const ACTOR_DID_HEADER: &str = "x-defra-actor-did";
pub const ACTOR_SIGNATURE_HEADER: &str = "x-defra-actor-signature";
pub const ACTOR_SIGNATURE_VERSION_HEADER: &str = "x-defra-actor-signature-version";
pub const ACTOR_SIGNATURE_VERSION: &str = "p2p-admin-v1";

pub struct HttpRemoteP2pAdmin {
    /// `http://host:port/api/v0`, used to compose `/p2p/*` URLs.
    api_base: String,
    api_base_path: String,
    client: Client,
    actor: Option<Arc<PrincipalIdentity>>,
}

impl HttpRemoteP2pAdmin {
    pub fn new(graphql_url: &str) -> RemoteP2pAdminResult<Self> {
        Self::new_inner(graphql_url, None)
    }

    pub fn new_with_actor(
        graphql_url: &str,
        actor: Arc<PrincipalIdentity>,
    ) -> RemoteP2pAdminResult<Self> {
        Self::new_inner(graphql_url, Some(actor))
    }

    fn new_inner(
        graphql_url: &str,
        actor: Option<Arc<PrincipalIdentity>>,
    ) -> RemoteP2pAdminResult<Self> {
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
        let api_base_path = reqwest::Url::parse(&api_base)
            .map_err(|e| RemoteP2pAdminError::LocalError(format!("invalid API base URL: {e}")))?
            .path()
            .trim_end_matches('/')
            .to_string();
        Ok(Self {
            api_base,
            api_base_path,
            client,
            actor,
        })
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.api_base, path)
    }

    fn request(
        &self,
        method: Method,
        path: &str,
        body: Option<Vec<u8>>,
    ) -> RemoteP2pAdminResult<RequestBuilder> {
        let mut request = self.client.request(method.clone(), self.url(path));
        let body_bytes = body.as_deref().unwrap_or_default();

        if let Some(actor) = self.actor.as_ref() {
            let signed_path = signed_admin_path(&self.api_base_path, path);
            let payload = signing_payload(method.as_str(), &signed_path, body_bytes);
            let signature = actor.sign(&payload).map_err(|error| {
                RemoteP2pAdminError::LocalError(format!("signing remote admin request: {error:#}"))
            })?;
            request = request
                .header(ACTOR_DID_HEADER, actor.did())
                .header(ACTOR_SIGNATURE_HEADER, hex_encode(&signature))
                .header(ACTOR_SIGNATURE_VERSION_HEADER, ACTOR_SIGNATURE_VERSION);
        }

        if let Some(body) = body {
            request = request.header(CONTENT_TYPE, "application/json").body(body);
        }

        Ok(request)
    }

    fn json_request<T: Serialize + ?Sized>(
        &self,
        method: Method,
        path: &str,
        body: &T,
    ) -> RemoteP2pAdminResult<RequestBuilder> {
        let body = serde_json::to_vec(body).map_err(|error| {
            RemoteP2pAdminError::LocalError(format!("encoding remote admin request body: {error}"))
        })?;
        self.request(method, path, Some(body))
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
            .request(Method::GET, "/p2p/info", None)?
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
            .request(Method::GET, "/p2p/active-peers", None)?
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
            .json_request(Method::POST, "/p2p/connect", addresses)?
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
            .request(Method::GET, "/p2p/replicators", None)?
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
            .json_request(Method::POST, "/p2p/replicators", &body)?
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
            .json_request(Method::DELETE, "/p2p/replicators", &body)?
            .send()
            .await
            .map_err(map_reqwest_err)?;
        check_status(resp).await?;
        Ok(())
    }

    async fn list_p2p_collections(&self) -> RemoteP2pAdminResult<Vec<String>> {
        let resp = self
            .request(Method::GET, "/p2p/collections", None)?
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
            .json_request(Method::POST, "/p2p/collections", collections)?
            .send()
            .await
            .map_err(map_reqwest_err)?;
        check_status(resp).await?;
        Ok(())
    }

    async fn delete_p2p_collections(&self, collections: &[String]) -> RemoteP2pAdminResult<()> {
        let resp = self
            .json_request(Method::DELETE, "/p2p/collections", collections)?
            .send()
            .await
            .map_err(map_reqwest_err)?;
        check_status(resp).await?;
        Ok(())
    }

    async fn list_p2p_documents(&self) -> RemoteP2pAdminResult<Vec<String>> {
        let resp = self
            .request(Method::GET, "/p2p/documents", None)?
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
            .json_request(Method::POST, "/p2p/documents", doc_ids)?
            .send()
            .await
            .map_err(map_reqwest_err)?;
        check_status(resp).await?;
        Ok(())
    }

    async fn delete_p2p_documents(&self, doc_ids: &[String]) -> RemoteP2pAdminResult<()> {
        let resp = self
            .json_request(Method::DELETE, "/p2p/documents", doc_ids)?
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
            .json_request(Method::POST, "/p2p/documents/sync", &body)?
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
            .json_request(Method::POST, "/p2p/collections/sync-versions", &body)?
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
            .json_request(Method::POST, "/p2p/collections/sync-branchable", &body)?
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

pub(crate) fn signing_payload(method: &str, path: &str, body: &[u8]) -> Vec<u8> {
    let mut payload = Vec::with_capacity(
        ACTOR_SIGNATURE_VERSION.len() + method.len() + path.len() + body.len() + 3,
    );
    payload.extend_from_slice(ACTOR_SIGNATURE_VERSION.as_bytes());
    payload.push(b'\n');
    payload.extend_from_slice(method.as_bytes());
    payload.push(b'\n');
    payload.extend_from_slice(path.as_bytes());
    payload.push(b'\n');
    payload.extend_from_slice(body);
    payload
}

pub(crate) fn signed_admin_path(api_base_path: &str, path: &str) -> String {
    let api_base_path = api_base_path.trim_end_matches('/');
    if api_base_path.is_empty() {
        path.to_string()
    } else {
        format!("{api_base_path}{path}")
    }
}

pub(crate) fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
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
    use crate::client::{DesktopPaths, PrincipalIdentity};
    use wiremock::matchers::{body_bytes, body_json, header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    fn admin_for(server: &MockServer) -> HttpRemoteP2pAdmin {
        let graphql = format!("{}/api/v0/graphql", server.uri());
        HttpRemoteP2pAdmin::new(&graphql).expect("admin constructs")
    }

    async fn signed_admin_for(server: &MockServer) -> (HttpRemoteP2pAdmin, Arc<PrincipalIdentity>) {
        let tempdir = tempfile::tempdir().unwrap();
        let paths = DesktopPaths::from_root(tempdir.path());
        let actor = Arc::new(PrincipalIdentity::load_or_create(&paths).await.unwrap());
        let graphql = format!("{}/api/v0/graphql", server.uri());
        let admin =
            HttpRemoteP2pAdmin::new_with_actor(&graphql, Arc::clone(&actor)).expect("admin signs");
        (admin, actor)
    }

    async fn mount_signed_request(
        server: &MockServer,
        actor: &PrincipalIdentity,
        method_name: &str,
        request_path: &str,
        body: Option<Vec<u8>>,
        response: ResponseTemplate,
    ) {
        let body_for_signature = body.as_deref().unwrap_or_default();
        let expected_signature = hex_encode(
            &actor
                .sign(&signing_payload(
                    method_name,
                    &signed_admin_path("/api/v0", request_path),
                    body_for_signature,
                ))
                .expect("signature"),
        );

        let mut mock = Mock::given(method(method_name))
            .and(path(format!("/api/v0{request_path}")))
            .and(header(ACTOR_DID_HEADER, actor.did()))
            .and(header(
                ACTOR_SIGNATURE_VERSION_HEADER,
                ACTOR_SIGNATURE_VERSION,
            ))
            .and(header(ACTOR_SIGNATURE_HEADER, expected_signature));
        if let Some(body) = body {
            mock = mock.and(body_bytes(body));
        }
        mock.respond_with(response).mount(server).await;
    }

    fn json_body<T: Serialize>(body: &T) -> Vec<u8> {
        serde_json::to_vec(body).expect("body")
    }

    #[tokio::test]
    async fn signed_admin_attaches_actor_headers_to_every_admin_method() {
        let server = MockServer::start().await;
        let (admin, actor) = signed_admin_for(&server).await;

        mount_signed_request(
            &server,
            &actor,
            "GET",
            "/p2p/info",
            None,
            ResponseTemplate::new(200).set_body_json(Vec::<String>::new()),
        )
        .await;
        admin.peer_info().await.expect("signed peer_info");

        mount_signed_request(
            &server,
            &actor,
            "GET",
            "/p2p/active-peers",
            None,
            ResponseTemplate::new(200).set_body_json(Vec::<String>::new()),
        )
        .await;
        admin.active_peers().await.expect("signed active_peers");

        let addresses = vec!["/ip4/1.2.3.4/tcp/9000/p2p/peer1".to_string()];
        mount_signed_request(
            &server,
            &actor,
            "POST",
            "/p2p/connect",
            Some(json_body(&addresses)),
            ResponseTemplate::new(200),
        )
        .await;
        admin.connect(&addresses).await.expect("signed connect");

        mount_signed_request(
            &server,
            &actor,
            "GET",
            "/p2p/replicators",
            None,
            ResponseTemplate::new(200).set_body_json(Vec::<serde_json::Value>::new()),
        )
        .await;
        admin
            .list_replicators()
            .await
            .expect("signed list_replicators");

        let collections = vec!["c1".to_string()];
        let add_replicator = AddReplicatorBody {
            collections: &collections,
            addresses: &addresses,
        };
        mount_signed_request(
            &server,
            &actor,
            "POST",
            "/p2p/replicators",
            Some(json_body(&add_replicator)),
            ResponseTemplate::new(200),
        )
        .await;
        admin
            .add_replicator(&addresses, &collections)
            .await
            .expect("signed add_replicator");

        let delete_replicator = DeleteReplicatorBody {
            collections: &collections,
            id: "peer1",
        };
        mount_signed_request(
            &server,
            &actor,
            "DELETE",
            "/p2p/replicators",
            Some(json_body(&delete_replicator)),
            ResponseTemplate::new(200),
        )
        .await;
        admin
            .delete_replicator("peer1", &collections)
            .await
            .expect("signed delete_replicator");

        mount_signed_request(
            &server,
            &actor,
            "GET",
            "/p2p/collections",
            None,
            ResponseTemplate::new(200).set_body_json(Vec::<String>::new()),
        )
        .await;
        admin
            .list_p2p_collections()
            .await
            .expect("signed list_p2p_collections");

        mount_signed_request(
            &server,
            &actor,
            "POST",
            "/p2p/collections",
            Some(json_body(&collections)),
            ResponseTemplate::new(200),
        )
        .await;
        admin
            .add_p2p_collections(&collections)
            .await
            .expect("signed add_p2p_collections");

        mount_signed_request(
            &server,
            &actor,
            "DELETE",
            "/p2p/collections",
            Some(json_body(&collections)),
            ResponseTemplate::new(200),
        )
        .await;
        admin
            .delete_p2p_collections(&collections)
            .await
            .expect("signed delete_p2p_collections");

        mount_signed_request(
            &server,
            &actor,
            "GET",
            "/p2p/documents",
            None,
            ResponseTemplate::new(200).set_body_json(Vec::<String>::new()),
        )
        .await;
        admin
            .list_p2p_documents()
            .await
            .expect("signed list_p2p_documents");

        let doc_ids = vec!["doc1".to_string()];
        mount_signed_request(
            &server,
            &actor,
            "POST",
            "/p2p/documents",
            Some(json_body(&doc_ids)),
            ResponseTemplate::new(200),
        )
        .await;
        admin
            .add_p2p_documents(&doc_ids)
            .await
            .expect("signed add_p2p_documents");

        mount_signed_request(
            &server,
            &actor,
            "DELETE",
            "/p2p/documents",
            Some(json_body(&doc_ids)),
            ResponseTemplate::new(200),
        )
        .await;
        admin
            .delete_p2p_documents(&doc_ids)
            .await
            .expect("signed delete_p2p_documents");

        let sync_documents = SyncDocumentsBody {
            collection_name: "Foo",
            doc_ids: &doc_ids,
            timeout: "5s".to_string(),
        };
        mount_signed_request(
            &server,
            &actor,
            "POST",
            "/p2p/documents/sync",
            Some(json_body(&sync_documents)),
            ResponseTemplate::new(200),
        )
        .await;
        admin
            .sync_documents("Foo", &doc_ids, Some(Duration::from_secs(5)))
            .await
            .expect("signed sync_documents");

        let version_ids = vec!["v1".to_string()];
        let sync_versions = SyncVersionsBody {
            version_ids: &version_ids,
            timeout: String::new(),
        };
        mount_signed_request(
            &server,
            &actor,
            "POST",
            "/p2p/collections/sync-versions",
            Some(json_body(&sync_versions)),
            ResponseTemplate::new(200),
        )
        .await;
        admin
            .sync_collection_versions(&version_ids, None)
            .await
            .expect("signed sync_collection_versions");

        let sync_branchable = SyncBranchableBody {
            collection_id: "col1",
            timeout: String::new(),
        };
        mount_signed_request(
            &server,
            &actor,
            "POST",
            "/p2p/collections/sync-branchable",
            Some(json_body(&sync_branchable)),
            ResponseTemplate::new(200),
        )
        .await;
        admin
            .sync_branchable_collection("col1", None)
            .await
            .expect("signed sync_branchable_collection");
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
