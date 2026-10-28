use serde_json::{json, Value};

use super::{graphql_api_base, ConfigAccess, ConfigClientError};

type Result<T, E = ConfigClientError> = std::result::Result<T, E>;

impl ConfigAccess {
    pub async fn collection_versions(&self) -> Result<Vec<Value>> {
        match self {
            Self::Local(local) => {
                local
                    .run_owned(|node| async move {
                        node.get_all_collection_versions()
                            .await?
                            .into_iter()
                            .map(|v| serde_json::to_value(v).map_err(Into::into))
                            .collect::<Result<_>>()
                    })
                    .await
            }
            Self::Graphql(_) => serde_json::from_value(
                self.schema_request(reqwest::Method::GET, "collections?get_inactive=true", None)
                    .await?,
            )
            .map_err(Into::into),
        }
    }

    /// Native DefraDB patch semantics; DefraDB owns version creation, activation, validation and node authorization.
    pub async fn patch_collection_schema(&self, collection: &str, patch: &Value) -> Result<Value> {
        match self {
            Self::Local(local) => {
                let collection = collection.to_owned();
                let patch = serde_json::to_string(patch)?;
                local
                    .run_owned(move |node| async move {
                        Ok(serde_json::to_value(
                            node.patch_collection(&collection, &patch).await?,
                        )?)
                    })
                    .await
            }
            Self::Graphql(_) => {
                self.schema_request(
                    reqwest::Method::PATCH,
                    "collections",
                    Some(json!({"Patch":patch})),
                )
                .await?;
                // The HTTP endpoint returns no version receipt; reading the active
                // version would misidentify a patch that created an inactive one.
                Ok(
                    json!({"committed":true,"collection":collection,"VersionID":null,
                    "note":"HTTP publication returns no version ID; inspect collection versions for the resulting definition"}),
                )
            }
        }
    }

    pub async fn activate_collection_version(&self, version_id: &str) -> Result<()> {
        match self {
            Self::Local(local) => {
                let version_id = version_id.to_owned();
                local
                    .run_owned(move |node| async move {
                        node.set_active_collection_version(&version_id)
                            .await
                            .map_err(Into::into)
                    })
                    .await
            }
            Self::Graphql(endpoint) => {
                let url = format!("{}/collections/default", graphql_api_base(endpoint.url())?);
                let client = super::graphql::http_client().await?;
                endpoint
                    .authorize(client.post(&url))?
                    .timeout(std::time::Duration::from_secs(120))
                    .body(version_id.to_owned())
                    .send()
                    .await?
                    .error_for_status()?;
                Ok(())
            }
        }
    }

    /// A schema grant never grants host filesystem access.
    pub async fn set_schema_migration(
        &self,
        config: crate::defra_node::LensConfig,
    ) -> Result<Value> {
        config.validate_for_http()?;
        match self {
            Self::Local(local) => Ok(local
                .run_owned(move |node| async move {
                    Ok::<_, ConfigClientError>(
                        json!({"lensId":node.set_migration(config).await?.to_string()}),
                    )
                })
                .await?),
            Self::Graphql(_) => Ok(self
                .schema_request(
                    reqwest::Method::POST,
                    "collections/migrations",
                    Some(serde_json::to_value(config)?),
                )
                .await?),
        }
    }

    pub async fn add_schema_view(&self, query: &str, sdl: &str) -> Result<()> {
        match self {
            Self::Local(local) => {
                let query = query.to_owned();
                let sdl = sdl.to_owned();
                local
                    .run_owned(move |node| async move {
                        node.add_view(&query, &sdl).await.map_err(Into::into)
                    })
                    .await
            }
            Self::Graphql(_) => {
                self.schema_request(
                    reqwest::Method::POST,
                    "view",
                    Some(json!({"Query":query,"SDL":sdl})),
                )
                .await?;
                Ok(())
            }
        }
    }

    pub async fn materialize_schema_collection(&self, collection: &str) -> Result<usize> {
        match self {
            Self::Local(local) => {
                let collection = collection.to_owned();
                local
                    .run_owned(move |node| async move {
                        node.materialize_collection(&collection)
                            .await
                            .map_err(Into::into)
                    })
                    .await
            }
            Self::Graphql(_) => Err(ConfigClientError::MaterializeUnsupported),
        }
    }

    async fn schema_request(
        &self,
        method: reqwest::Method,
        route: &str,
        body: Option<Value>,
    ) -> Result<Value> {
        let Self::Graphql(endpoint) = self else {
            return Err(ConfigClientError::SchemaEndpointRequired);
        };
        let url = format!("{}/{}", graphql_api_base(endpoint.url())?, route);
        let client = super::graphql::http_client().await?;
        let mut request = endpoint
            .authorize(client.request(method, &url))?
            .timeout(std::time::Duration::from_secs(120));
        if let Some(body) = body {
            request = request.json(&body);
        }
        let response =
            request
                .send()
                .await
                .map_err(|source| ConfigClientError::SchemaOperation {
                    url: url.clone(),
                    source,
                })?;
        let status = response.status();
        let bytes = response.bytes().await?;
        if !status.is_success() {
            return Err(ConfigClientError::SchemaOperationStatus {
                status,
                body: String::from_utf8_lossy(&bytes).into_owned(),
            });
        }
        if bytes.is_empty() {
            Ok(Value::Null)
        } else {
            Ok(serde_json::from_slice(&bytes)?)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn http_patch_does_not_substitute_the_active_version_for_an_inactive_publication() {
        use axum::{routing::patch, Json, Router};
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/api/v0/graphql", listener.local_addr().unwrap());
        let app = Router::new().route(
            "/api/v0/collections",
            patch(|Json(body): Json<Value>| async move {
                assert_eq!(body["Patch"][0]["value"], false);
                axum::http::StatusCode::OK
            }),
        );
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let access = ConfigAccess::Graphql(super::super::GraphqlEndpoint::anonymous(endpoint));
        let result = access
            .patch_collection_schema(
                "Example",
                &json!([{"op":"replace","path":"/Example/IsActive","value":false}]),
            )
            .await;
        server.abort();
        let receipt = result.unwrap();
        assert_eq!(receipt["committed"], true);
        assert!(receipt["VersionID"].is_null());
    }
}
