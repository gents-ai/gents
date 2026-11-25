use gents_session::request::lifecycle::{
    build_signed_client_request, submit_client_request, ExecutionOrigin, RequestIdentity,
    RequestSigner, RequestSpec,
};
use gents_store::local::document_acp::DocumentAcp;

use crate::test_support::vera_test_home::{seen, ClientUser, Household, PROVIDERS};

/// What a client request leaves on one home: the publication's outcome, and
/// which of its relationships the store then honours.
#[derive(Debug, PartialEq, Eq)]
struct Published {
    outcome: Result<(), String>,
    agent_works_the_session: bool,
    agent_works_the_request: bool,
    user_administers_the_session: bool,
    user_administers_the_request: bool,
}

async fn publish(provider: DocumentAcp) -> anyhow::Result<Published> {
    let household = Household::open(provider).await?;
    let agent = household.derive_agent()?;
    household.create_agent(&agent).await?;
    let path = household.directory.path().join("user.key");
    let raw = tokio::task::spawn_blocking(move || {
        gents_store::identity::load_or_create_file_identity(&path)
    })
    .await??;
    let user = ClientUser(household.user.clone(), raw);
    let access = household.as_user()?;
    let published = async {
        let create = build_signed_client_request(
            &access,
            RequestSpec::new(
                gents_protocol::request_admission::RequestPurpose::Normal,
                RequestIdentity {
                    request_id: uuid::Uuid::new_v4().to_string(),
                    node_did: household.node_did.to_string(),
                    requester_did: Some(household.user_did.to_string()),
                    agent_did: agent.agent_did.clone(),
                    session_id: "client-request-session".into(),
                    content: "hello".into(),
                    execution_origin: ExecutionOrigin::Interactive,
                    created_at: chrono::Utc::now()
                        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
                },
                gents_protocol::request_admission::AgentRequestAdmissionRecord::client(
                    household.user_did.as_str(),
                ),
            ),
            RequestSigner::Identity(&user),
        )
        .await?;
        submit_client_request(&access, &create).await?;
        Ok::<_, anyhow::Error>(create)
    }
    .await;
    let (outcome, create) = match published {
        Ok(create) => (Ok(()), Some(create)),
        Err(error) => (Err(seen(error.as_ref())), None),
    };
    household
        .settled(&household.user_did, "the client request's relationships")
        .await?;
    let agent_did = identity::Did::new(&agent.agent_did)?;
    let session = create
        .as_ref()
        .and_then(|create| create.session_doc_id.clone());
    let request = super::stored(
        &household.node,
        &household.user_did,
        "AgentRequest",
        "_docID",
    )
    .await?
    .into_iter()
    .next();
    let holds = |actor: identity::Did,
                 permission: &'static str,
                 resource: &'static str,
                 doc: Option<String>| {
        let household = &household;
        async move {
            match doc {
                Some(doc) => household.holds(&actor, permission, resource, &doc).await,
                None => Ok(false),
            }
        }
    };
    let published = Published {
        outcome,
        agent_works_the_session: holds(
            agent_did.clone(),
            "working_agent",
            "sessions",
            session.clone(),
        )
        .await?,
        agent_works_the_request: holds(
            agent_did,
            "working_agent",
            "delivered_requests",
            request.clone(),
        )
        .await?,
        user_administers_the_session: holds(
            household.user_did.clone(),
            "delete",
            "sessions",
            session,
        )
        .await?,
        user_administers_the_request: holds(
            household.user_did.clone(),
            "delete",
            "delivered_requests",
            request,
        )
        .await?,
    };
    household.shutdown().await?;
    Ok(published)
}

/// Lean `Identity.Household`: a client request's session and request name
/// their Agent the working Agent and sit under it (`parent`), so the Agent
/// works both and the Agent's session administrators may delete them. The
/// client stages those relationships in its HTTP transaction, on either kind
/// of home.
#[tokio::test]
async fn a_client_request_publishes_its_relationships() -> anyhow::Result<()> {
    for provider in PROVIDERS {
        assert_eq!(
            publish(provider).await?,
            Published {
                outcome: Ok(()),
                agent_works_the_session: true,
                agent_works_the_request: true,
                user_administers_the_session: true,
                user_administers_the_request: true,
            },
            "{provider}"
        );
    }
    Ok(())
}
