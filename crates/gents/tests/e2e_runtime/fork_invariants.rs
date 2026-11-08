use gents::session::{fork_on, fork_via_http, ForkError, ForkOutcome, ForkParams};
use gents::store::graphql::escape_graphql_string;
use gents::NodeIdentity;
use gents_protocol::output::rows::{
    decode_transcript_message_row, TranscriptMessageRow, AGENT_MESSAGE_FIELDS,
};
use gents_protocol::output::*;

use crate::support::agent_requests::{canonical_now, RequestActors, RequestSeed};
use crate::support::agent_transcript::{self, Imported, Source};
use crate::support::snapshots::{
    fetch_compaction_entry_snapshots_for_session, fetch_session_snapshot,
};
use crate::support::{test_db, TestDb};

/// The Agent authors its own transcript: every session, request, segment,
/// header, tool row and compaction cursor below is written by the fixture
/// Agent's managed signer, and the fork runs as that same Agent.
struct Fixture {
    db: TestDb,
    actors: RequestActors,
}

impl Fixture {
    async fn new(name: &str) -> Self {
        let db = test_db(name).await;
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
            boundary: gents::session::ForkBoundary::UserTurn(turn),
            caller_node_did: self.node_did(),
            caller_requester_did: Some(requester),
            target_agent_did: None,
        }
    }

    fn own<'a>(&'a self, source: &'a str, turn: u32) -> ForkParams<'a> {
        self.params(source, self.agent_did(), turn)
    }

    async fn fork(&self, params: ForkParams<'_>) -> Result<ForkOutcome, ForkError> {
        fork_on(&self.actors.agent_access, params).await
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

    async fn body(&self, header: &str, requester: &str) -> String {
        let (_, native) = gents::session::load_canonical_message(
            &self.actors.agent_access,
            header,
            self.node_did(),
            Some(requester),
        )
        .await
        .unwrap();
        gents_protocol::transcript::present_message(&native).body_markdown
    }
}

fn origin(
    session: &gents_protocol::session::AgentSession,
) -> &gents_protocol::session::SessionFork {
    session
        .provenance
        .as_ref()
        .and_then(|value| value.fork.as_ref())
        .unwrap()
}

#[tokio::test]
async fn fork_copies_exact_header_prefix_by_reference() {
    let f = Fixture::new("fork-prefix").await;
    let parent = f.source("parent").await;
    let one = f.import(&parent, 1, MessageRole::User, "u1", true).await;
    let two = f
        .import(&parent, 2, MessageRole::Assistant, "a1", true)
        .await;
    f.import(&parent, 3, MessageRole::User, "u2", true).await;
    let outcome = f.fork(f.own("parent", 1)).await.unwrap();
    assert_eq!(
        (
            outcome.copied_messages,
            outcome.copied_tool_calls,
            outcome.copied_tool_results
        ),
        (2, 0, 0)
    );
    let copied = f.headers(&outcome.session_id).await;
    let unchanged_parent = f.headers("parent").await;
    assert_eq!(unchanged_parent.len(), 3);
    assert!(matches!(
        &unchanged_parent[0].message.publication,
        MessagePublication::RequestExecution { .. }
    ));
    assert_eq!(copied.len(), 2);
    let mut referenced = Vec::new();
    for (copy, source) in copied.iter().zip([one, two]) {
        assert_eq!(copy.message.request_doc_id, None);
        assert!(
            matches!(&copy.message.publication, MessagePublication::Fork { origin_message_doc_id } if origin_message_doc_id == &source.header)
        );
        let MessageBlock::Text { text } = &copy.message.blocks[0] else {
            panic!("text")
        };
        assert_eq!(&text.output.close_doc_id, source.close.as_ref().unwrap());
        referenced.push(source.close.unwrap());
    }
    assert_eq!(f.body(&copied[0].doc_id, f.agent_did()).await, "u1");
    assert_eq!(f.body(&copied[1].doc_id, f.agent_did()).await, "a1");
    // The source keeps its three closures; the fork holds its own copy of
    // exactly the two closures its retained headers reference, keyed by the
    // logical source closing record.
    let segments = f
        .rows("{ AgentOutputSegment { session_id request_doc_id } }")
        .await;
    let segments = segments["AgentOutputSegment"].as_array().unwrap();
    assert_eq!(
        segments
            .iter()
            .filter(|row| row["session_id"] == "parent")
            .count(),
        3
    );
    let mut child = segments
        .iter()
        .filter(|row| row["session_id"] == outcome.session_id.as_str())
        .map(|row| row["request_doc_id"].as_str().unwrap().to_owned())
        .collect::<Vec<_>>();
    child.sort();
    referenced.sort();
    assert_eq!(child, referenced);
    assert_eq!(segments.len(), 5);
}

#[tokio::test]
async fn fork_via_http_uses_same_header_contract() {
    let f = Fixture::new("fork-http").await;
    let parent = f.source("parent-http").await;
    let source = f.import(&parent, 1, MessageRole::User, "u", true).await;
    let agent = f.actors.agent();
    let endpoint = gents::store::config_client::GraphqlEndpoint::as_principal(
        f.db.client_endpoint(),
        agent.did(),
    )
    .with_client_signing_capability(agent.client_signing_capability().unwrap())
    .unwrap()
    .with_client_signed_agent_creates();
    let outcome = fork_via_http(&endpoint, f.own("parent-http", 1))
        .await
        .unwrap();
    let copied = f.headers(&outcome.session_id).await;
    assert!(
        matches!(&copied[0].message.publication, MessagePublication::Fork { origin_message_doc_id } if origin_message_doc_id == &source.header)
    );
}

#[tokio::test]
async fn fork_preserves_exact_requester_scope_and_denies_cross_requester_access() {
    let f = Fixture::new("fork-requester-scope").await;
    let requester = f.agent_did().to_owned();
    let parent = f.source("requester-parent").await;
    let source = f
        .import(&parent, 1, MessageRole::User, "tenant-bound", true)
        .await;
    let outcome = f
        .fork(f.params("requester-parent", &requester, 1))
        .await
        .unwrap();
    let child_session = fetch_session_snapshot(&f.actors, &outcome.session_id)
        .await
        .unwrap();
    assert_eq!(
        child_session.requester_did.as_deref(),
        Some(requester.as_str())
    );
    let copied = f.headers(&outcome.session_id).await;
    assert_eq!(
        copied[0].message.requester_did.as_deref(),
        Some(requester.as_str())
    );
    assert!(
        matches!(&copied[0].message.publication, MessagePublication::Fork { origin_message_doc_id } if origin_message_doc_id == &source.header)
    );
    assert_eq!(f.body(&copied[0].doc_id, &requester).await, "tenant-bound");
    let other_requester = f.actors.fixture.user.did().to_owned();
    let denied = f
        .fork(f.params("requester-parent", &other_requester, 1))
        .await
        .unwrap_err();
    assert!(matches!(
        denied,
        ForkError::ForkSourceNotFound(_) | ForkError::ForkNotSameAgent
    ));
}

#[tokio::test]
async fn fork_never_copies_executable_tool_rows() {
    let f = Fixture::new("fork-no-tools").await;
    let parent = f.source("parent-tools").await;
    f.import(&parent, 1, MessageRole::User, "u", true).await;
    agent_transcript::tool_call(&f.actors, &parent, 1, "call").await;
    let outcome = f.fork(f.own("parent-tools", 1)).await.unwrap();
    assert_eq!(
        (outcome.copied_tool_calls, outcome.copied_tool_results),
        (0, 0)
    );
    let source_tools = f
        .rows(r#"{ AgentToolCall(filter: {session_id: {_eq: "parent-tools"}}) {_docID} }"#)
        .await;
    assert_eq!(source_tools["AgentToolCall"].as_array().unwrap().len(), 1);
    let child = escape_graphql_string(&outcome.session_id);
    let response = f
        .rows(&format!(
            r#"{{ AgentToolCall(filter: {{session_id: {{_eq: "{child}"}}}}) {{_docID}} }}"#
        ))
        .await;
    assert!(response["AgentToolCall"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn fork_copies_only_compaction_cursors_in_retained_prefix() {
    let f = Fixture::new("fork-compaction").await;
    let parent = f.source("parent-compaction").await;
    f.import(&parent, 1, MessageRole::User, "u1", true).await;
    f.import(&parent, 2, MessageRole::User, "u2", true).await;
    agent_transcript::compaction(&f.actors, &parent, 1, "early", 1, 1).await;
    agent_transcript::compaction(&f.actors, &parent, 2, "late", 2, 2).await;
    let outcome = f.fork(f.own("parent-compaction", 1)).await.unwrap();
    assert_eq!(outcome.copied_compaction_entries, 1);
    let rows = fetch_compaction_entry_snapshots_for_session(&f.actors, &outcome.session_id).await;
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].summary, "early");
    assert_eq!(rows[0].compacted_through_sequence, Some(1));
    assert_eq!(rows[0].request_doc_id, None);
}

#[tokio::test]
async fn fork_boundaries_and_provenance_are_exact() {
    let f = Fixture::new("fork-boundaries").await;
    let parent = f.source("parent-boundaries").await;
    for (seq, role) in [
        (1, MessageRole::User),
        (2, MessageRole::Assistant),
        (3, MessageRole::User),
        (4, MessageRole::Assistant),
    ] {
        f.import(&parent, seq, role, "text", true).await;
    }
    let empty = f.fork(f.own("parent-boundaries", 0)).await.unwrap();
    let full = f.fork(f.own("parent-boundaries", 2)).await.unwrap();
    assert_eq!(empty.copied_messages, 0);
    assert_eq!(full.copied_messages, 4);
    let session = fetch_session_snapshot(&f.actors, &empty.session_id)
        .await
        .unwrap();
    assert_eq!(origin(&session).source_session_id, "parent-boundaries");
    assert_eq!(origin(&session).at_user_turn, 0);
}

#[tokio::test]
async fn fork_rejects_invalid_cut_and_rolls_back_invalid_reference() {
    let f = Fixture::new("fork-invalid").await;
    let parent = f.source("parent-invalid").await;
    f.import(&parent, 1, MessageRole::User, "valid", true).await;
    let range = f.fork(f.own("parent-invalid", 2)).await.unwrap_err();
    assert!(matches!(range, ForkError::ForkAtUserTurnOutOfRange(2, 1)));
    f.import(&parent, 2, MessageRole::Assistant, "invalid", false)
        .await;
    let missing = f.fork(f.own("parent-invalid", 1)).await.unwrap_err();
    let ForkError::ForkCopyFailed(cause) = missing else {
        panic!("missing closure must reject the copy step: {missing:?}");
    };
    let missing_detail = format!("{cause:#}");
    assert!(
        missing_detail.contains("unresolved closing segment missing-close"),
        "fixture's only defect must be its missing closure: {missing_detail}"
    );
    let sessions = f.rows("{ AgentSession { session_id } }").await;
    assert_eq!(sessions["AgentSession"].as_array().unwrap().len(), 1);
    let messages = f.rows("{ AgentMessage { session_id } }").await;
    assert!(messages["AgentMessage"]
        .as_array()
        .unwrap()
        .iter()
        .all(|row| row["session_id"] == "parent-invalid"));
}

#[tokio::test]
async fn fork_rejects_cross_agent_source_references() {
    let f = Fixture::new("fork-behavior").await;
    let parent = f.source("parent-behavior").await;
    let (alternate, _) = crate::support::install_managed_agent(
        &f.db,
        &f.actors.fixture.access,
        &f.actors.fixture.user,
        "Alternate agent",
    )
    .await;
    f.import(&parent, 1, MessageRole::User, "u", true).await;
    // Selecting another Agent is the configuring User's choice, made through
    // its client as `gents session fork --agent` does.
    let error = fork_on(
        &f.actors.fixture.access,
        ForkParams {
            target_agent_did: Some(alternate.did()),
            ..f.own("parent-behavior", 1)
        },
    )
    .await
    .unwrap_err();
    assert!(
        matches!(error, ForkError::CrossAgentForkUnavailable),
        "{error:?}"
    );
    assert!(matches!(
        f.fork(ForkParams {
            target_agent_did: Some("missing"),
            ..f.own("parent-behavior", 1)
        })
        .await
        .unwrap_err(),
        ForkError::ForkAgentNotFound(_)
    ));
}

#[tokio::test]
async fn concurrent_forks_are_disjoint() {
    let f = Fixture::new("fork-concurrent").await;
    let parent = f.source("parent-concurrent").await;
    f.import(&parent, 1, MessageRole::User, "u", true).await;
    let (left, right) = tokio::join!(
        f.fork(f.own("parent-concurrent", 1)),
        f.fork(f.own("parent-concurrent", 1))
    );
    let (left, right) = (left.unwrap(), right.unwrap());
    assert_ne!(left.session_id, right.session_id);
    let left = f.headers(&left.session_id).await.remove(0);
    let right = f.headers(&right.session_id).await.remove(0);
    assert_ne!(left.doc_id, right.doc_id);
    assert_ne!(left.message.message_key, right.message.message_key);
}

#[tokio::test]
async fn fork_of_fork_names_immediate_origin_and_hydrates() {
    let f = Fixture::new("fork-ancestry").await;
    let parent = f.source("grandparent").await;
    f.import(&parent, 1, MessageRole::User, "ancestry", true)
        .await;
    let child = f.fork(f.own("grandparent", 1)).await.unwrap();
    let child_header = f.headers(&child.session_id).await.remove(0);
    let grandchild = f.fork(f.own(&child.session_id, 1)).await.unwrap();
    let grandchild_header = f.headers(&grandchild.session_id).await.remove(0);
    assert!(
        matches!(&grandchild_header.message.publication, MessagePublication::Fork { origin_message_doc_id } if origin_message_doc_id == &child_header.doc_id)
    );
    assert_eq!(
        f.body(&grandchild_header.doc_id, f.agent_did()).await,
        "ancestry"
    );
    assert_eq!(
        origin(
            &fetch_session_snapshot(&f.actors, &grandchild.session_id)
                .await
                .unwrap()
        )
        .source_session_id,
        child.session_id
    );
}

#[tokio::test]
async fn fork_rejects_busy_wrong_principal_and_missing_source() {
    let f = Fixture::new("fork-rejections").await;
    f.source("parent-rejections").await;
    f.actors
        .create_request(
            RequestSeed::new("pending", "parent-rejections", &canonical_now()).state("pending"),
        )
        .await;
    assert!(matches!(
        f.fork(f.own("parent-rejections", 0)).await.unwrap_err(),
        ForkError::ForkSourceBusy
    ));
    let wrong = f
        .fork(ForkParams {
            caller_node_did: "did:key:foreign",
            ..f.own("parent-rejections", 0)
        })
        .await
        .unwrap_err();
    assert!(matches!(
        wrong,
        ForkError::ForkSourceNotFound(_) | ForkError::ForkNotSameAgent
    ));
    assert!(matches!(
        f.fork(f.own("absent", 0)).await.unwrap_err(),
        ForkError::ForkSourceNotFound(_)
    ));
}
