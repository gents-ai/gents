use gents_protocol::output::rows::{
    decode_transcript_message_row, TranscriptMessageRow, AGENT_MESSAGE_FIELDS,
};
use gents_protocol::output::*;
use gents_session::session::{fork_on, ForkParams};
use gents_store::graphql::escape_graphql_string;

use crate::support::agent_requests::RequestActors;
use crate::support::agent_transcript::{self, Imported, Source};
use crate::support::trial_store;
use gents_store::test_support::TestStore;

/// The Agent authors its own transcript: every session, request, segment,
/// header, tool row and compaction cursor below is written by the fixture
/// Agent's managed signer, and the fork runs as that same Agent.
struct Fixture {
    db: TestStore,
    actors: RequestActors,
}

impl Fixture {
    async fn new(name: &str) -> Self {
        let db = trial_store(name).await;
        let actors = RequestActors::new(&db).await;
        Self { db, actors }
    }

    fn node_did(&self) -> &str {
        self.actors.node_did()
    }

    fn agent_did(&self) -> &str {
        self.actors.agent_did()
    }

    async fn source(&self, session: &str) -> Source {
        agent_transcript::source(&self.actors, session).await
    }

    async fn import(
        &self,
        source: &Source,
        sequence: u32,
        role: MessageRole,
        text: &str,
        valid_ref: bool,
    ) -> Imported {
        agent_transcript::import(&self.actors, source, sequence, role, text, valid_ref).await
    }

    fn params<'a>(&'a self, source: &'a str, requester: &'a str, turn: u32) -> ForkParams<'a> {
        ForkParams {
            source_session_id: source,
            boundary: gents_session::session::ForkBoundary::UserTurn(turn),
            caller_node_did: self.node_did(),
            caller_requester_did: Some(requester),
            target_agent_did: None,
        }
    }

    fn own<'a>(&'a self, source: &'a str, turn: u32) -> ForkParams<'a> {
        self.params(source, self.agent_did(), turn)
    }

    async fn rows(&self, query: &str) -> serde_json::Value {
        let mut response = self
            .actors
            .agent_access
            .execute(query)
            .await
            .expect("read as the forking Agent");
        response["data"].take()
    }

    async fn headers(&self, session: &str) -> Vec<TranscriptMessageRow> {
        let session = escape_graphql_string(session);
        let data = self.rows(&format!(r#"{{ AgentMessage(filter: {{session_id: {{_eq: "{session}"}}}}, order: {{sequence: ASC}}) {{ {AGENT_MESSAGE_FIELDS} }} }}"#)).await;
        data["AgentMessage"]
            .as_array()
            .unwrap()
            .iter()
            .map(|row| decode_transcript_message_row(row).unwrap())
            .collect()
    }

    /// Every copied child carries its native parent and working-Agent
    /// relationships, published by the forking Agent; the node's User, which
    /// does not manage the Agent's children, cannot remove them.
    async fn assert_children_linked(&self, session: &str) {
        let escaped = escape_graphql_string(session);
        let data = self.rows(&format!(r#"{{ AgentSession(filter: {{session_id: {{_eq: "{escaped}"}}}}) {{_docID}} AgentMessage(filter: {{session_id: {{_eq: "{escaped}"}}}}) {{_docID parent_linked}} AgentOutputSegment(filter: {{session_id: {{_eq: "{escaped}"}}}}) {{_docID parent_linked}} }}"#)).await;
        let session_doc = data["AgentSession"][0]["_docID"].as_str().unwrap();
        let mut linked = 0;
        for collection in ["AgentMessage", "AgentOutputSegment"] {
            for row in data[collection].as_array().unwrap() {
                assert_eq!(row["parent_linked"], true, "{collection} {row}");
                let doc = row["_docID"].as_str().unwrap();
                let relationships = self
                    .actors
                    .agent_access
                    .list_document_relationships(collection, doc)
                    .await
                    .unwrap();
                assert!(
                    relationships
                        .iter()
                        .any(|relationship| relationship.relation == "parent"
                            && relationship.subject.contains(session_doc)),
                    "{collection} {doc} lacks its fork parent: {relationships:?}"
                );
                assert!(
                    relationships
                        .iter()
                        .any(|relationship| relationship.relation == "working_agent"
                            && relationship.subject == self.agent_did()),
                    "{collection} {doc} lacks its working Agent: {relationships:?}"
                );
                assert!(self
                    .actors
                    .fixture
                    .access
                    .delete_resource_relationship(
                        collection,
                        doc,
                        "parent",
                        "sessions",
                        session_doc
                    )
                    .await
                    .is_err());
                linked += 1;
            }
        }
        assert!(linked > 0, "the fork copied no children");
    }
}

#[tokio::test]
async fn fork_via_http_uses_same_header_contract() {
    let f = Fixture::new("fork-http").await;
    let parent = f.source("parent-http").await;
    let source = f.import(&parent, 1, MessageRole::User, "u", true).await;
    let agent = f.actors.agent();
    let endpoint = gents_store::config_client::GraphqlEndpoint::as_principal(
        f.db.graphql_endpoint(),
        agent.did(),
    )
    .with_client_signing_capability(agent.client_signing_capability().unwrap())
    .unwrap()
    .with_client_signed_agent_creates();
    let outcome = fork_on(
        &gents_store::config_client::ConfigAccess::Graphql(endpoint),
        f.own("parent-http", 1),
    )
    .await
    .unwrap();
    let copied = f.headers(&outcome.session_id).await;
    assert!(
        matches!(&copied[0].message.publication, MessagePublication::Fork { origin_message_doc_id } if origin_message_doc_id == &source.header)
    );
    f.assert_children_linked(&outcome.session_id).await;
}
