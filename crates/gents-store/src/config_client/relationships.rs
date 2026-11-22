use super::{graphql, graphql_api_base, ConfigAccess, ConfigClientError};
use crate::config_client::WriteRoute;

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
    /// An owner-staged change, decided by the runtime's owner-staged route
    /// after the documents that imply it commit.
    OwnerStaged {
        change: gents_protocol::owner_staged::OwnerStagedChange,
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
                ConfigRelationshipGrant::OwnerStaged { change } => {
                    self.request_owner_staged(change, true).await
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
    /// Request one owner-staged change as this access's actor
    /// (`Identity.OwnerStaged.request`). Over HTTP it is one signed command to
    /// the runtime's owner-staged route; locally it is decided in its own
    /// transaction.
    pub async fn request_owner_staged(
        &self,
        change: &gents_protocol::owner_staged::OwnerStagedChange,
        grant: bool,
    ) -> Result<(), ConfigClientError> {
        match self.write_route() {
            WriteRoute::Http(endpoint) => {
                super::owner_staged_route::request(endpoint, change, grant).await
            }
            WriteRoute::Local(_) => {
                self.transact_write("config.request_owner_staged", |txn| {
                    Box::pin(async move { txn.request_owner_staged(change, grant).await })
                })
                .await
            }
        }
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
        crate::graphql::validate_collection_identifier(collection)?;
        let grants_member_admin = !delete
            && collection == "Node"
            && relation == crate::identity::NODE_ADMINISTRATOR_RELATION
            && identity::Did::new(target).is_ok();
        match self.write_route() {
            WriteRoute::Local(local) => {
                local.require_actor_signer()?;
                local
                    .identity()
                    .ok_or(ConfigClientError::RelationshipActorRequired)?;
                let context = local.request_context();
                let (collection, doc_id, relation, target) = (
                    collection.to_owned(),
                    doc_id.to_owned(),
                    relation.to_owned(),
                    target.to_owned(),
                );
                let deliberate = grants_member_admin.then(|| (doc_id.clone(), target.clone()));
                let node = local.node().clone();
                local
                    .run_owned(move |node| async move {
                        if delete {
                            node.delete_dac_actor_relationship_as(
                                &context,
                                &collection,
                                &doc_id,
                                &relation,
                                &target,
                            )
                            .await?;
                        } else {
                            node.add_dac_actor_relationship_as(
                                &context,
                                &collection,
                                &doc_id,
                                &relation,
                                &target,
                            )
                            .await?;
                        }
                        Ok::<_, ConfigClientError>(())
                    })
                    .await?;
                if let Some((doc, member)) = deliberate {
                    crate::identity::close_owed_removal(&node, &doc, &member).await?;
                }
                Ok(())
            }
            WriteRoute::Http(endpoint) => {
                if endpoint.is_anonymous() {
                    return Err(ConfigClientError::RelationshipActorRequired);
                }
                let path = if grants_member_admin {
                    super::owner_staged_route::ADMIN_GRANT_PATH
                } else {
                    "/acp/document/relationship"
                };
                let url = format!("{}{path}", graphql_api_base(endpoint.url())?);
                let client = graphql::http_client().await?;
                let method = if delete {
                    reqwest::Method::DELETE
                } else {
                    reqwest::Method::POST
                };
                let body = serde_json::json!({
                    "collection": collection, "doc_id": doc_id, "relation": relation, "target_actor": target,
                });
                let response = endpoint
                    .send_resolving(|endpoint| {
                        endpoint.authorize(client.request(method.clone(), &url).json(&body))
                    })
                    .await??;
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

pub(super) fn entity_set_target(
    resource: &str,
    doc_id: &str,
    relation: &str,
) -> Result<String, ConfigClientError> {
    let target = format!("{resource}:\"{doc_id}\"#{relation}");
    match acp::parse_target_subject(&target) {
        Ok(acp::Subject::EntitySet {
            resource: parsed_resource,
            object_id,
            relation: parsed_relation,
        }) if !relation.is_empty()
            && parsed_resource == resource
            && object_id == doc_id
            && parsed_relation == relation =>
        {
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
        let node = std::sync::Arc::new(crate::test_support::node_builder().build().await.unwrap());
        let owner = identity::Did::new(
            node.node_identity_did()
                .expect("fixture node has an identity"),
        )
        .unwrap();
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
        access.add_collection(&gents_protocol::schemas::HomePolicyIds::node_store().render(&format!("type Parent @policy(id: \"{policy}\", resource: \"parents\") {{ name: String }}\ntype Child @policy(id: \"{policy}\", resource: \"children\") {{ name: String }}"))).await.unwrap();
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

    /// The access the offline operator CLI builds: a Local client whose endpoint
    /// holds the client signing key of the identity that owns the node -- one key
    /// in the home, proven by its DID, managing what it published.
    fn operator_access(
        node: &std::sync::Arc<defra_node::Node>,
        identity: &crate::identity::KeyIdentity,
    ) -> (ConfigAccess, identity::Did) {
        use crate::identity::NodeIdentity as _;
        let capability = identity
            .client_signing_capability()
            .expect("a held local key makes a capability");
        let did = identity.did().to_owned();
        let endpoint =
            super::super::GraphqlEndpoint::as_principal("http://127.0.0.1:1/api/v0/graphql", &did)
                .with_client_signing_capability(capability)
                .expect("the endpoint's principal is the key's owner");
        let access = ConfigAccess::local_as(
            node.clone(),
            identity::Did::new(&did).expect("a key identity makes a DID"),
        )
        .with_client_signed_writes_through(endpoint)
        .expect("the client key proves the access's actor");
        (
            access,
            identity::Did::new(&did).expect("a key identity makes a DID"),
        )
    }

    /// The unsigned relationship read is a proof of operator, not the absence
    /// of an Agent signer: a signer-less Local client the runtime or session
    /// side built must be refused, and only the access the offline CLI builds --
    /// a Local one holding the endpoint with the client signing key -- may read
    /// relationships without one.
    #[tokio::test]
    async fn only_a_client_signed_local_access_reads_relationships_unsigned() {
        let (builder, node_key) = crate::test_support::keyed_node_builder();
        let node = std::sync::Arc::new(builder.build().await.unwrap());
        let owner = identity::Did::new(
            node.node_identity_did()
                .expect("fixture node has an identity"),
        )
        .unwrap();
        let yaml = "name: Relationship reader proof\nresources:\n  - name: parents\n    relations:\n      - name: reader\n        types: [actor]\n    permissions:\n      - name: read\n        expr: reader\n      - name: update\n      - name: delete\n  - name: children\n    relations:\n      - name: parent\n        types: [parents]\n    permissions:\n      - name: read\n        expr: parent->read\n      - name: update\n      - name: delete\n";
        let policy = node
            .add_dac_policy_as(
                &defra_core::RequestContext::default().with_identity(Some(owner.to_string())),
                yaml,
            )
            .await
            .unwrap();
        let owner_access = ConfigAccess::local_as(node.clone(), owner.clone());
        owner_access
            .add_collection(&format!(
                "type Parent @policy(id: \"{policy}\", resource: \"parents\") {{ name: String }}\ntype Child @policy(id: \"{policy}\", resource: \"children\") {{ name: String }}"
            ))
            .await
            .unwrap();
        let created = owner_access
            .write(
                "test.relationships.proof",
                "mutation { add_Parent(input: {name: \"parent\"}) { _docID } }",
            )
            .await
            .unwrap();
        let parent = created["data"]["add_Parent"][0]["_docID"]
            .as_str()
            .unwrap()
            .to_owned();
        let reader = identity::Did::new("did:key:reader").unwrap();
        owner_access
            .add_actor_relationship("Parent", &parent, "reader", &reader)
            .await
            .unwrap();

        // A signer-less Local access with an asserted identity, of the shape the
        // runtime and session side build: no client signing key backs it, so it
        // must not inherit the operator's unsigned read.
        let asserted =
            ConfigAccess::local_with_optional_agent_signer(node.clone(), reader.clone(), None)
                .unwrap();
        let ConfigAccess::Local(asserted_local) = &asserted else {
            panic!("the fixture access is local");
        };
        assert!(
            asserted_local.client_endpoint().is_none(),
            "no client signing key backs this access"
        );
        assert!(matches!(
            asserted
                .list_document_relationships_as_node("Parent", &parent)
                .await,
            Err(ConfigClientError::ManagedAgentSignerRequired)
        ));

        // The operator's access is the one the offline CLI builds: Local, with
        // the endpoint holding a client signing key for its own actor.
        // The CLI's client identity is the node's owner: one key, held by the
        // home, whose DID the endpoint proves and whose resources it manages.
        let (operator, operator_did) = operator_access(&node, &node_key);
        let ConfigAccess::Local(operator_local) = &operator else {
            panic!("the operator access is local");
        };
        assert!(
            operator_local.client_endpoint().is_some(),
            "the offline CLI's access holds the client signing key"
        );
        assert_eq!(operator_did.as_str(), owner.as_str());
        // The read itself is Local: the endpoint is the proof, not the transport
        // for reads, so it needs no server to answer.
        let relationships = operator
            .list_document_relationships_as_node("Parent", &parent)
            .await
            .unwrap();
        assert!(
            relationships
                .iter()
                .any(|relationship| relationship.subject == reader.as_str()),
            "the proof-present access reads the grant: {relationships:?}"
        );
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
        assert_eq!(
            entity_set_target("agents", "doc:with#separators", "executor").unwrap(),
            "agents:\"doc:with#separators\"#executor"
        );
        assert!(entity_set_target("agents", "doc", "").is_err());
        assert!(entity_set_target("agents", "doc\"tail", "executor").is_err());
        assert!(entity_set_target("bad:resource", "doc", "executor").is_err());
    }
}
