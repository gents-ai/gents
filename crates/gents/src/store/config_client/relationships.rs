use super::{graphql, graphql_api_base, ConfigAccess, ConfigClientError};

/// A detached ACP1 grant for a document committed by the authenticated creator.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ConfigRelationshipGrant {
    Actor {
        collection: String,
        doc_id: String,
        relation: String,
        target: String,
    },
    Resource {
        collection: String,
        doc_id: String,
        relation: String,
        target_resource: String,
        target_doc_id: String,
    },
}

impl ConfigAccess {
    /// Complete grants returned by a committed-but-incomplete transaction.
    /// The original creator must authenticate again; successful grants are not
    /// replayed, since an owner may have revoked them subsequently.
    pub async fn complete_relationship_grants(
        &self,
        creator: &str,
        grants: &[ConfigRelationshipGrant],
    ) -> Result<(), ConfigClientError> {
        if self.actor_did().as_deref() != Some(creator) {
            return Err(ConfigClientError::TransactionActorMismatch);
        }
        for (index, grant) in grants.iter().enumerate() {
            let result = match grant {
                ConfigRelationshipGrant::Actor {
                    collection,
                    doc_id,
                    relation,
                    target,
                } => match identity::Did::new(target) {
                    Ok(target) => {
                        self.add_actor_relationship(collection, doc_id, relation, &target)
                            .await
                    }
                    Err(error) => Err(ConfigClientError::RelationshipTargetIdentity(error)),
                },
                ConfigRelationshipGrant::Resource {
                    collection,
                    doc_id,
                    relation,
                    target_resource,
                    target_doc_id,
                } => {
                    self.add_resource_relationship(
                        collection,
                        doc_id,
                        relation,
                        target_resource,
                        target_doc_id,
                    )
                    .await
                }
            };
            if let Err(source) = result {
                return Err(ConfigClientError::CommittedRelationshipsIncomplete {
                    creator: creator.to_owned(),
                    pending: grants[index..].to_vec(),
                    source: Box::new(source),
                });
            }
        }
        Ok(())
    }
    /// Grant an actor a document relationship as this access's authenticated actor.
    /// A relationship that already exists is successful.
    pub async fn add_actor_relationship(
        &self,
        collection: &str,
        doc_id: &str,
        relation: &str,
        target: &identity::Did,
    ) -> Result<(), ConfigClientError> {
        self.change_document_relationship(collection, doc_id, relation, target.as_str(), false)
            .await
    }

    /// Revoke an actor's document relationship as this access's authenticated actor.
    /// A relationship that is already absent is successful.
    pub async fn delete_actor_relationship(
        &self,
        collection: &str,
        doc_id: &str,
        relation: &str,
        target: &identity::Did,
    ) -> Result<(), ConfigClientError> {
        self.change_document_relationship(collection, doc_id, relation, target.as_str(), true)
            .await
    }

    /// Grant a relationship to another policy resource and document.
    pub async fn add_resource_relationship(
        &self,
        collection: &str,
        doc_id: &str,
        relation: &str,
        target_resource: &str,
        target_doc_id: &str,
    ) -> Result<(), ConfigClientError> {
        let target = resource_target(target_resource, target_doc_id)?;
        self.change_document_relationship(collection, doc_id, relation, &target, false)
            .await
    }

    /// Revoke a relationship to another policy resource and document.
    pub async fn delete_resource_relationship(
        &self,
        collection: &str,
        doc_id: &str,
        relation: &str,
        target_resource: &str,
        target_doc_id: &str,
    ) -> Result<(), ConfigClientError> {
        let target = resource_target(target_resource, target_doc_id)?;
        self.change_document_relationship(collection, doc_id, relation, &target, true)
            .await
    }

    async fn change_document_relationship(
        &self,
        collection: &str,
        doc_id: &str,
        relation: &str,
        target: &str,
        delete: bool,
    ) -> Result<(), ConfigClientError> {
        crate::store::graphql::validate_collection_identifier(collection)?;
        match self {
            Self::Local(local) => {
                let actor = local
                    .identity()
                    .cloned()
                    .ok_or(ConfigClientError::RelationshipActorRequired)?;
                let (collection, doc_id, relation, target) = (
                    collection.to_owned(),
                    doc_id.to_owned(),
                    relation.to_owned(),
                    target.to_owned(),
                );
                local
                    .run_owned(move |node| async move {
                        if delete {
                            node.delete_dac_actor_relationship(
                                actor.as_str(),
                                &collection,
                                &doc_id,
                                &relation,
                                &target,
                            )
                            .await?;
                        } else {
                            node.add_dac_actor_relationship(
                                actor.as_str(),
                                &collection,
                                &doc_id,
                                &relation,
                                &target,
                            )
                            .await?;
                        }
                        Ok::<_, ConfigClientError>(())
                    })
                    .await
            }
            Self::Graphql(endpoint) => {
                if endpoint.is_anonymous() {
                    return Err(ConfigClientError::RelationshipActorRequired);
                }
                let url = format!(
                    "{}/acp/document/relationship",
                    graphql_api_base(endpoint.url())?
                );
                let client = graphql::http_client().await?;
                let method = if delete {
                    reqwest::Method::DELETE
                } else {
                    reqwest::Method::POST
                };
                let response = endpoint.authorize(client.request(method, url).json(&serde_json::json!({
                    "collection": collection, "doc_id": doc_id, "relation": relation, "target_actor": target,
                })))?.send().await?;
                let status = response.status();
                if !status.is_success() {
                    return Err(ConfigClientError::DocumentRelationshipStatus {
                        status,
                        body: response.text().await?,
                    });
                }
                Ok(())
            }
        }
    }
}

pub(super) fn resource_target(resource: &str, doc_id: &str) -> Result<String, ConfigClientError> {
    let target = format!("{resource}:\"{doc_id}\"");
    match acp::parse_target_subject(&target) {
        Ok(acp::Subject::EntitySet {
            resource: parsed_resource,
            object_id,
            relation,
        }) if parsed_resource == resource && object_id == doc_id && relation.is_empty() => {
            Ok(target)
        }
        _ => Err(ConfigClientError::InvalidResourceRelationshipTarget),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn incomplete_grants_retain_creator_and_only_retry_the_remaining_suffix() {
        use axum::{extract::State, http::StatusCode, routing::post, Json, Router};
        use std::sync::{Arc, Mutex};

        type Requests = Arc<Mutex<Vec<serde_json::Value>>>;
        async fn record(
            State(requests): State<Requests>,
            Json(body): Json<serde_json::Value>,
        ) -> StatusCode {
            let mut requests = requests.lock().unwrap();
            requests.push(body);
            if requests.len() == 2 {
                StatusCode::FORBIDDEN
            } else {
                StatusCode::OK
            }
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/api/v0/graphql", listener.local_addr().unwrap());
        let requests: Requests = Arc::new(Mutex::new(Vec::new()));
        let app = Router::new()
            .route("/api/v0/acp/document/relationship", post(record))
            .with_state(requests.clone());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let actor = identity::Did::new("did:key:creator").unwrap();
        let access = ConfigAccess::Graphql(
            super::super::GraphqlEndpoint::with_authenticated_delegation(
                endpoint.clone(),
                "Bearer signed-token",
                actor.clone(),
            )
            .unwrap(),
        );
        let other = ConfigAccess::Graphql(
            super::super::GraphqlEndpoint::with_authenticated_delegation(
                endpoint,
                "Bearer other-token",
                identity::Did::new("did:key:other").unwrap(),
            )
            .unwrap(),
        );
        let grants = vec![
            ConfigRelationshipGrant::Resource {
                collection: "Agent".into(),
                doc_id: "agent-doc".into(),
                relation: "parent".into(),
                target_resource: "nodes".into(),
                target_doc_id: "node-doc".into(),
            },
            ConfigRelationshipGrant::Actor {
                collection: "Agent".into(),
                doc_id: "agent-doc".into(),
                relation: "executor".into(),
                target: "did:key:executor".into(),
            },
        ];
        let error = access
            .complete_relationship_grants(actor.as_str(), &grants)
            .await
            .unwrap_err();
        let ConfigClientError::CommittedRelationshipsIncomplete {
            creator, pending, ..
        } = error
        else {
            panic!("expected committed incomplete result")
        };
        assert_eq!(creator, actor.as_str());
        assert_eq!(pending, grants[1..]);
        assert!(matches!(
            other.complete_relationship_grants(&creator, &pending).await,
            Err(ConfigClientError::TransactionActorMismatch)
        ));
        access
            .complete_relationship_grants(&creator, &pending)
            .await
            .unwrap();
        let requests = requests.lock().unwrap();
        assert_eq!(requests.len(), 3);
        assert_eq!(requests[0]["relation"], "parent");
        assert_eq!(requests[1]["relation"], "executor");
        assert_eq!(requests[2], requests[1]);
        server.abort();
    }

    #[tokio::test]
    async fn http_relationships_preserve_actor_and_target_without_transaction() {
        use axum::{
            extract::State,
            http::{HeaderMap, Method},
            routing::post,
            Json, Router,
        };
        use std::sync::{Arc, Mutex};

        type Requests = Arc<Mutex<Vec<(Method, HeaderMap, serde_json::Value)>>>;
        async fn record(
            State(requests): State<Requests>,
            method: Method,
            headers: HeaderMap,
            Json(body): Json<serde_json::Value>,
        ) -> Json<serde_json::Value> {
            requests.lock().unwrap().push((method, headers, body));
            Json(serde_json::json!({"ExistedAlready": true}))
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/api/v0/graphql", listener.local_addr().unwrap());
        let requests: Requests = Arc::new(Mutex::new(Vec::new()));
        let app = Router::new()
            .route(
                "/api/v0/acp/document/relationship",
                post(record).delete(record),
            )
            .with_state(requests.clone());
        let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let actor = identity::Did::new("did:key:actor").unwrap();
        let endpoint = super::super::GraphqlEndpoint::with_authenticated_delegation(
            endpoint,
            "Bearer signed-token",
            actor,
        )
        .unwrap();
        let access = ConfigAccess::Graphql(endpoint);
        access
            .add_resource_relationship("Agent", "doc", "parent", "nodes", "parent")
            .await
            .unwrap();
        access
            .delete_resource_relationship("Agent", "doc", "parent", "nodes", "parent")
            .await
            .unwrap();
        let entries = requests.lock().unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].0, Method::POST);
        assert_eq!(entries[1].0, Method::DELETE);
        for (_, headers, body) in entries.iter() {
            assert_eq!(headers.get("authorization").unwrap(), "Bearer signed-token");
            assert!(headers.get("x-defradb-tx").is_none());
            assert_eq!(body["target_actor"], "nodes:\"parent\"");
            assert_eq!(body["collection"], "Agent");
        }
        server.abort();
    }

    #[tokio::test]
    async fn local_relationships_use_the_explicit_actor_and_are_idempotent() {
        let node = std::sync::Arc::new(defra_node::Node::builder().build().await.unwrap());
        let owner = identity::Did::new("did:key:owner").unwrap();
        let reader = identity::Did::new("did:key:reader").unwrap();
        let yaml = "name: Relationship owner test\nresources:\n  - name: parents\n    relations:\n      - name: reader\n        types: [actor]\n    permissions:\n      - name: read\n        expr: reader\n      - name: update\n      - name: delete\n  - name: children\n    relations:\n      - name: parent\n        types: [parents]\n    permissions:\n      - name: read\n        expr: parent->read\n      - name: update\n      - name: delete\n";
        let policy = node
            .add_dac_policy_as(
                &defra_core::RequestContext::default().with_identity(Some(owner.to_string())),
                yaml,
            )
            .await
            .unwrap();
        let access = ConfigAccess::local_as(node.clone(), owner.clone());
        access.add_collection(&format!("type Parent @policy(id: \"{policy}\", resource: \"parents\") {{ name: String }}\ntype Child @policy(id: \"{policy}\", resource: \"children\") {{ name: String }}")).await.unwrap();
        let created = access.write("test.relationships.create", "mutation { add_Parent(input: {name: \"parent\"}) { _docID } add_Child(input: {name: \"child\"}) { _docID } }").await.unwrap();
        let parent = created["data"]["add_Parent"][0]["_docID"].as_str().unwrap();
        let child = created["data"]["add_Child"][0]["_docID"].as_str().unwrap();
        let unbound = ConfigAccess::local(node.clone());
        assert!(matches!(
            unbound
                .add_resource_relationship("Child", child, "parent", "parents", parent)
                .await,
            Err(ConfigClientError::RelationshipActorRequired)
        ));
        let denied = ConfigAccess::local_as(node.clone(), reader.clone());
        assert!(denied
            .add_resource_relationship("Child", child, "parent", "parents", parent)
            .await
            .is_err());
        for _ in 0..2 {
            access
                .add_resource_relationship("Child", child, "parent", "parents", parent)
                .await
                .unwrap();
            access
                .add_actor_relationship("Parent", parent, "reader", &reader)
                .await
                .unwrap();
        }
        let visible = denied.execute("{ Child { _docID } }").await.unwrap();
        assert_eq!(visible["data"]["Child"].as_array().unwrap().len(), 1);
        for _ in 0..2 {
            access
                .delete_actor_relationship("Parent", parent, "reader", &reader)
                .await
                .unwrap();
        }
        let hidden = denied.execute("{ Child { _docID } }").await.unwrap();
        assert!(hidden["data"]["Child"].as_array().unwrap().is_empty());
        node.shutdown().await.expect("node shutdown");
    }

    #[test]
    fn resource_target_round_trips_and_rejects_unrepresentable_ids() {
        assert_eq!(
            resource_target("agents", "doc:with#separators").unwrap(),
            "agents:\"doc:with#separators\""
        );
        assert!(resource_target("bad:resource", "doc").is_err());
        assert!(resource_target("agents", "doc\"tail").is_err());
        assert!(resource_target("agents", "").is_err());
    }
}
