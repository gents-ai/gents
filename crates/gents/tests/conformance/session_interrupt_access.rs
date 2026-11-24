use std::sync::Arc;

use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose, RequestScope,
};
use gents_session::session::interrupt_marker;
use gents_store::config_client::ConfigAccess;
use gents_store::graphql::escape_graphql_string;
use gents_store::identity::NodeIdentity;

use crate::support::agent_requests::{canonical_now, RequestActors};

/// Who created a session through a path, as `Identity.SessionInterrupt`'s
/// fixture names its creator.
#[derive(Clone, Copy)]
enum Creator {
    User,
    Agent,
}

impl Creator {
    fn modeled(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Agent => "agent",
        }
    }
}

struct Actor {
    access: ConfigAccess,
    did: String,
}

struct World {
    actors: RequestActors,
    user: Actor,
    agent: Actor,
    node_admin: Actor,
    other: Actor,
}

impl World {
    fn actor(&self, modeled: &str) -> &Actor {
        match modeled {
            "user" => &self.user,
            "agent" => &self.agent,
            "node-admin" => &self.node_admin,
            "other" => &self.other,
            other => panic!("unmodeled actor {other}"),
        }
    }

    fn create(
        &self,
        purpose: RequestPurpose,
        id: &str,
        session: &str,
        requester: &str,
    ) -> AgentRequestCreate {
        let admission = if requester == self.agent.did {
            AgentRequestAdmissionRecord::local_self(&self.agent.did)
        } else {
            AgentRequestAdmissionRecord::client(requester)
        };
        AgentRequestCreate::base(
            purpose,
            id,
            RequestScope::new(self.actors.node_did(), requester, &self.agent.did, session),
            "marker access",
            "interactive",
            canonical_now(),
            admission,
        )
    }

    /// The session doc and its marker doc, as the session's creator reads them.
    async fn marker_of(&self, creator: &Actor, session: &str) -> (String, String) {
        let response = creator
            .access
            .execute(&format!(
                r#"{{ AgentSession(filter: {{ session_id: {{ _eq: "{}" }} }}, limit: 2) {{ _docID }} }}"#,
                escape_graphql_string(session)
            ))
            .await
            .expect("read the created session");
        let session_doc = response["data"]["AgentSession"][0]["_docID"]
            .as_str()
            .unwrap_or_else(|| panic!("{session}: its creator reads no session"))
            .to_owned();
        let response = creator
            .access
            .execute(&format!(
                r#"{{ AgentSessionInterrupt(filter: {{ session_doc_id: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
                escape_graphql_string(&session_doc)
            ))
            .await
            .expect("read the created marker");
        let marker = response["data"]["AgentSessionInterrupt"][0]["_docID"]
            .as_str()
            .unwrap_or_else(|| panic!("{session}: its creator reads no marker"))
            .to_owned();
        (session_doc, marker)
    }
}

async fn agent_actor(
    db: &gents_store::test_support::TestStore,
    agent: &Arc<dyn NodeIdentity>,
) -> Actor {
    Actor {
        access: ConfigAccess::local_managed_agent(
            db.node.clone(),
            agent.did().parse().expect("Agent DID"),
            agent.as_ref(),
        )
        .expect("managed Agent access"),
        did: agent.did().to_owned(),
    }
}

/// Every session-creating path gives the session a marker its readers read
/// and its updaters bump (Lean `Identity.SessionInterrupt`): the marker's
/// `parent` relationship is staged in the session's own transaction, whoever
/// signs it. A session never arrives over P2P: both collections are
/// runtime-local, so no replicator is admitted for them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn every_session_creating_path_gives_its_marker_to_the_sessions_readers() {
    let db = crate::support::trial_store("session-interrupt-access").await;
    let fixture = crate::support::managed_agent_fixture(&db).await;
    let (admin_agent, _) =
        crate::support::install_managed_agent(&db, &fixture.access, &fixture.user, "Administrator")
            .await;
    let (other_agent, _) =
        crate::support::install_managed_agent(&db, &fixture.access, &fixture.user, "Other").await;
    let user = Actor {
        access: fixture.access.clone(),
        did: fixture.user.did().to_owned(),
    };
    let actors = RequestActors::from_fixture(&db, fixture).await;
    let node = user
        .access
        .execute(&format!(
            r#"{{ Node(filter: {{node_did: {{_eq: "{}"}}}}) {{_docID}} }}"#,
            escape_graphql_string(db.node_identity.did()),
        ))
        .await
        .unwrap();
    let node_doc = node["data"]["Node"][0]["_docID"]
        .as_str()
        .expect("Node visible");
    user.access
        .add_actor_relationship(
            "Node",
            node_doc,
            "administrator",
            &admin_agent.did().parse().unwrap(),
        )
        .await
        .unwrap();
    let world = World {
        agent: agent_actor(&db, actors.agent()).await,
        node_admin: agent_actor(&db, &admin_agent).await,
        other: agent_actor(&db, &other_agent).await,
        user,
        actors,
    };
    let user_did = world.user.did.clone();
    let agent_did = world.agent.did.clone();

    let mut created: Vec<(&str, Creator, String)> = Vec::new();

    let spec = gents_session::request::lifecycle::RequestSpec::new(
        RequestPurpose::Normal,
        gents_session::request::lifecycle::RequestIdentity {
            requester_did: Some(user_did.clone()),
            request_id: "client-admission".into(),
            node_did: world.actors.node_did().to_owned(),
            agent_did: agent_did.clone(),
            session_id: "client-admission".into(),
            content: "marker access".into(),
            execution_origin: gents_session::request::lifecycle::ExecutionOrigin::Interactive,
            created_at: canonical_now(),
        },
        AgentRequestAdmissionRecord::client(&user_did),
    );
    let client = gents_session::request::lifecycle::build_signed_client_request(
        &world.user.access,
        spec,
        gents_session::request::lifecycle::RequestSigner::Identity(&world.actors.fixture.user),
    )
    .await
    .expect("sign the client request");
    gents_session::request::lifecycle::submit_client_request(&world.user.access, &client)
        .await
        .expect("one-transaction client admission");
    created.push(("client admission", Creator::User, "client-admission".into()));

    let reserved = world.create(
        RequestPurpose::Normal,
        "goal-reservation",
        "goal-reservation",
        &user_did,
    );
    gents_session::request::lifecycle::reserve_goal_session(&world.user.access, &reserved)
        .await
        .expect("Goal session reservation");
    created.push(("Goal reservation", Creator::User, "goal-reservation".into()));

    let forked = gents_session::session::fork_on(
        &world.user.access,
        gents_session::session::ForkParams {
            source_session_id: "goal-reservation",
            boundary: gents_session::session::ForkBoundary::UserTurn(0),
            caller_node_did: world.actors.node_did(),
            caller_requester_did: Some(&user_did),
            target_agent_did: None,
        },
    )
    .await
    .expect("fork the client session");
    created.push(("fork", Creator::User, forked.session_id));

    let published = world.create(
        RequestPurpose::Normal,
        "self-signed",
        "self-signed",
        &agent_did,
    );
    world
        .agent
        .access
        .transact_write("test.session_interrupt_access.publish", |txn| {
            let published = &published;
            Box::pin(async move {
                gents_session::request::lifecycle::runtime_publication::publish(txn, published)
                    .await
                    .map_err(anyhow::Error::from)
            })
        })
        .await
        .expect("self-signed runtime publication");
    created.push((
        "self-signed Agent publication",
        Creator::Agent,
        "self-signed".into(),
    ));

    let cases = gents_lean_vocab::families::session_interrupt_access_marker::cases();
    for (path, creator, session) in &created {
        let creator_actor = world.actor(creator.modeled());
        let (session_doc, marker_doc) = world.marker_of(creator_actor, session).await;
        let relationships = creator_actor
            .access
            .list_document_relationships(
                gents_protocol::schemas::AGENT_SESSION_INTERRUPT_NAME,
                &marker_doc,
            )
            .await
            .expect("the creator lists its marker's relationships");
        assert!(
            relationships
                .iter()
                .any(|edge| edge.relation == "parent" && edge.subject.contains(&session_doc)),
            "{path}: the marker has no parent relationship to its session: {relationships:?}"
        );
        for case in cases
            .iter()
            .filter(|case| case.input.creator == creator.modeled())
        {
            let actor = world.actor(&case.input.actor);
            let read = actor
                .access
                .transact_readonly("test.session_interrupt_access.read", |txn| {
                    let session_doc = &session_doc;
                    Box::pin(async move { interrupt_marker::read_in_txn(txn, session_doc).await })
                })
                .await
                .unwrap_or_else(|error| panic!("{path}: {} read: {error:#}", case.input.actor));
            let update = match &read {
                Some(marker) => actor
                    .access
                    .transact_write("test.session_interrupt_access.bump", |txn| {
                        Box::pin(async move {
                            interrupt_marker::bump_in_txn(txn, marker, &canonical_now()).await
                        })
                    })
                    .await
                    .map(|bumped| bumped.is_some())
                    .unwrap_or(false),
                None => false,
            };
            case.check(
                gents_lean_vocab::families::session_interrupt_access_marker::Output {
                    read: read.is_some(),
                    update,
                },
            );
        }
    }

    for collection in [
        "AgentSession",
        gents_protocol::schemas::AGENT_SESSION_INTERRUPT_NAME,
    ] {
        assert!(
            gents_protocol::peer_schema::is_managed_collection(collection)
                && !gents_protocol::schemas::is_replicated_protocol_collection(collection),
            "{collection} is runtime-local, so no session or marker arrives over P2P"
        );
    }
    db.node.shutdown().await.unwrap();
}
