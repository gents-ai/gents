use super::*;
use gents_protocol::request_admission::{
    AgentRequestAdmissionRecord, AgentRequestCreate, RequestPurpose, RequestScope,
};
use gents_protocol::session_token_usage::SessionTokenUsage;
use gents_session::test_support::AgentHome;
use gents_store::config_client::{ConfigAccess, ConfigApplyTxn, NodePermission};
use gents_store::identity::{KeyIdentity, NodeIdentity};
use serde_json::json;

struct GoalHome {
    home: AgentHome,
    admin: ConfigAccess,
    agent_doc: String,
    reader: KeyIdentity,
    reader_access: ConfigAccess,
    _reader_dir: tempfile::TempDir,
}

impl GoalHome {
    async fn start(name: &str) -> Self {
        let home = AgentHome::start_with(name, crate::lifecycle_hooks::composition()).await;
        let admin = ConfigAccess::local_as(
            home.node.clone(),
            identity::Did::new(home.node_identity.did()).unwrap(),
        );
        let reader_dir = tempfile::tempdir().unwrap();
        let reader = KeyIdentity::load_or_create(reader_dir.path().join("reader"), None).unwrap();
        for permission in [
            NodePermission::DacRelationAdd,
            NodePermission::DacRelationDelete,
            NodePermission::DocumentRead,
            NodePermission::DocumentUpdate,
            NodePermission::DocumentDelete,
            NodePermission::CollectionGet,
        ] {
            admin
                .grant_node_permission(&identity::Did::new(home.agent_did()).unwrap(), permission)
                .await
                .unwrap();
        }
        for permission in [
            NodePermission::DacRelationDelete,
            NodePermission::DocumentDelete,
        ] {
            admin
                .grant_node_permission(&identity::Did::new(home.user.did()).unwrap(), permission)
                .await
                .unwrap();
        }
        for permission in [NodePermission::CollectionGet, NodePermission::DocumentRead] {
            admin
                .grant_node_permission(&identity::Did::new(reader.did()).unwrap(), permission)
                .await
                .unwrap();
        }
        let reader_access =
            ConfigAccess::local_as(home.node.clone(), identity::Did::new(reader.did()).unwrap());
        let agent_doc = crate::goal::client_control::agent_document_for_test(
            &home.user_access,
            home.node_identity.did(),
            home.agent_did(),
        )
        .await;
        Self {
            home,
            admin,
            agent_doc,
            reader,
            reader_access,
            _reader_dir: reader_dir,
        }
    }

    async fn create_shared_session(&self, session_id: &str) -> String {
        let runtime_did = self.home.node_identity.did();
        let agent_did = self.home.agent_did();
        let user_did = self.home.user.did();
        self.home
            .user_access
            .transact_write("test.goal_home.session", |txn| {
                Box::pin(async move {
                    gents_session::session::ensure_session_in_txn(
                        txn,
                        gents_session::session::NewAgentSession {
                            session_id,
                            node_did: runtime_did,
                            agent_did,
                            requester_did: Some(user_did),
                            title: None,
                            provenance: None,
                            created_at: "2026-11-04T00:00:00Z",
                        },
                    )
                    .await?;
                    Ok::<_, anyhow::Error>(())
                })
            })
            .await
            .unwrap();
        let session_doc = self
            .home
            .user_access
            .transact_readonly("test.goal_home.session_document", |txn| {
                Box::pin(async move {
                    gents_store::config_client::agent_session::load_agent_session_owner_in_txn(
                        txn,
                        runtime_did,
                        session_id,
                    )
                    .await
                })
            })
            .await
            .unwrap()
            .unwrap()
            .doc_id;
        gents_session::session::grant_created_session_relationships(
            &self.home.user_access,
            &session_doc,
            &self.agent_doc,
            agent_did,
        )
        .await
        .unwrap();
        self.home
            .user_access
            .add_actor_relationship(
                "AgentSession",
                &session_doc,
                "reader",
                &identity::Did::new(self.reader.did()).unwrap(),
            )
            .await
            .unwrap();
        session_doc
    }
}

async fn reader_sees(access: &ConfigAccess, query: &str, collection: &str) -> bool {
    let response = access.execute(query).await.unwrap();
    if let Some(errors) = response.get("errors") {
        assert!(
            errors.is_null() || errors.as_array().is_some_and(Vec::is_empty),
            "reader {collection} query failed: {response}"
        );
    }
    !response["data"][collection]
        .as_array()
        .unwrap_or_else(|| panic!("reader {collection} query returned no rows array: {response}"))
        .is_empty()
}

#[tokio::test]
async fn goal_usage_auto_resume_deletion_and_concurrent_repair_scenarios() {
    let gh = GoalHome::start("goal-core-scenarios").await;
    let owner = gh.home.node_identity.did();
    let user = &gh.home.user;
    let user_access = &gh.home.user_access;
    let agent = &gh.home.agent_identity;
    let agent_access = &gh.home.agent_access;

    // Session token usage breakdown sums to the total.
    {
        let create = AgentRequestCreate::base(
            RequestPurpose::Normal,
            "usage-request",
            RequestScope::new(owner, user.did(), agent.did(), "usage-session"),
            "test",
            "interactive",
            "2026-07-15T00:00:00Z",
            AgentRequestAdmissionRecord::client(user.did()),
        );
        let lifecycle =
            gents_toolcalls::call::lifecycle::admission_fixture::pending_client_fixture_request(
                &gh.home.node,
                user_access,
                gh.home.node_identity.clone(),
                user,
                agent.as_ref(),
                create,
            )
            .await
            .unwrap();
        let request_doc_id = lifecycle.request().doc_id.clone();
        let session_doc_id = user_access
            .transact_readonly("test.goal_usage.session", |txn| {
                Box::pin(async move {
                    gents_store::config_client::agent_session::load_agent_session_owner_in_txn(
                        txn,
                        owner,
                        "usage-session",
                    )
                    .await
                })
            })
            .await
            .unwrap()
            .unwrap()
            .doc_id;
        for (seq, prompt, completion) in [(1, 1000, 50), (2, 200, 7)] {
            let input = json!({
                "call_id": format!("usage-call-{seq}"), "runtime_instance_id": "goal-test",
                "request_id": "usage-request", "request_doc_id": request_doc_id,
                "session_id": "usage-session", "call_seq": seq, "backend_id": "backend-test",
                "agent_did": agent.did(), "node_did": owner, "call_kind": "inference", "attempt": 1,
                "call_state": "completed", "queued_at": "2026-07-15T00:00:00Z",
                "started_at": "2026-07-15T00:00:00Z", "ended_at": "2026-07-15T00:00:01Z",
                "priority": 0, "queue_depth_at_enqueue": 0, "controller_generation": 1,
                "backend_config_fingerprint": "goal-test", "prompt_tokens": prompt,
                "completion_tokens": completion,
            });
            agent_access.transact_write("test.goal_usage.call", |txn| {
                let input = &input;
                let session_doc_id = &session_doc_id;
                Box::pin(async move {
                    let response = txn.execute_with_variables(
                        "mutation($input: InferenceCallMutationInputArg!) { create_InferenceCall(input: $input) { _docID } }",
                        &json!({"input": input}),
                    ).await?;
                    let doc = gents_store::graphql::created_doc_id(&response, "InferenceCall")?;
                    txn.add_resource_relationship("InferenceCall", &doc, "parent", "sessions", session_doc_id).await?;
                    Ok::<_, anyhow::Error>(())
                })
            }).await.unwrap();
        }
        let usage = agent_access
            .transact_readonly("goal.session_usage_breakdown_test", |txn| {
                Box::pin(async move {
                    session_token_usage_breakdown_in_txn(txn, owner, "usage-session").await
                })
            })
            .await
            .unwrap();
        assert_eq!(
            usage,
            SessionTokenUsage {
                input: 1200,
                output: 57,
                total: 1257
            }
        );
        assert_eq!(usage.input + usage.output, usage.total);
        assert_eq!(
            session_token_usage(
                &gh.home.node,
                &identity::Did::new(agent.did()).unwrap(),
                owner,
                "usage-session"
            )
            .await
            .unwrap(),
            1257
        );
        let calls = user_access
            .execute("{ InferenceCall { _docID } }")
            .await
            .unwrap();
        assert_eq!(calls["data"]["InferenceCall"].as_array().unwrap().len(), 2);
    }

    // auto_resume_at_reset round-trips through the live schema.
    {
        gh.create_shared_session("auto-resume-session").await;
        let goal = seed_goal_from_access(
            agent_access,
            owner,
            "auto-resume-session",
            Some("ship"),
            None,
            None,
            None,
        )
        .await
        .unwrap();
        assert_eq!(goal.auto_resume_at_reset, None);
        let doc = escape_graphql_string(&goal.doc_id);
        agent_access
            .write(
                "test.auto_resume.set",
                &format!(
                    r#"mutation {{ update_Goal(docID: "{doc}", input: {{auto_resume_at_reset: true}}) {{_docID}} }}"#
                ),
            )
            .await
            .unwrap();
        let stored = agent_access
            .transact_readonly("test.auto_resume.read", |txn| {
                Box::pin(load_canonical_goal_in_txn(
                    txn,
                    owner,
                    "auto-resume-session",
                ))
            })
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.auto_resume_at_reset, Some(true));
    }

    // Session goal delete uses selected agent native owner.
    {
        let create = AgentRequestCreate::base(
            RequestPurpose::Normal,
            "delete-request",
            RequestScope::new(owner, user.did(), agent.did(), "delete-session"),
            "test",
            "interactive",
            "2026-07-15T00:00:00Z",
            AgentRequestAdmissionRecord::client(user.did()),
        );
        let _lifecycle =
            gents_toolcalls::call::lifecycle::admission_fixture::pending_client_fixture_request(
                &gh.home.node,
                user_access,
                gh.home.node_identity.clone(),
                user,
                agent.as_ref(),
                create,
            )
            .await
            .unwrap();
        let goal = seed_goal_from_access(
            agent_access,
            owner,
            "delete-session",
            Some("test deletion"),
            None,
            None,
            None,
        )
        .await
        .unwrap();
        gents_store::test_support::relationships::actor_in_txn(
            agent_access,
            "Goal",
            &goal.doc_id,
            "reader",
            &identity::Did::new(owner).unwrap(),
            false,
        )
        .await
        .unwrap();
        let denied = delete_goal_from_access(&gh.admin, &goal).await;
        assert!(
            denied.is_err(),
            "runtime identity must not delete the selected agent's goal"
        );
        assert!(delete_goal_from_access(agent_access, &goal).await.unwrap());
        let remaining = agent_access
            .execute(r#"{ Goal(filter: { session_id: { _eq: "delete-session" } }) { _docID } }"#)
            .await
            .unwrap();
        assert!(remaining["data"]["Goal"].as_array().unwrap().is_empty());
    }

    // Held user deletes own goal and claim by physical ID without touching sibling sessions.
    {
        for (req, sess) in [
            ("user-delete-request", "user-delete-session"),
            ("other-delete-request", "other-delete-session"),
        ] {
            let create = AgentRequestCreate::base(
                RequestPurpose::Normal,
                req,
                RequestScope::new(owner, user.did(), agent.did(), sess),
                "test",
                "interactive",
                "2026-07-15T00:00:00Z",
                AgentRequestAdmissionRecord::client(user.did()),
            );
            gents_session::request::lifecycle::reserve_goal_session(user_access, &create)
                .await
                .unwrap();
        }
        crate::goal::create_goal_for_session_from_access(
            user_access,
            owner,
            "user-delete-session",
            "user-owned goal",
            None,
        )
        .await
        .unwrap();
        let other = crate::goal::create_goal_for_session_from_access(
            user_access,
            owner,
            "other-delete-session",
            "keep this goal",
            None,
        )
        .await
        .unwrap();
        let other_goal_doc_id = other.goal().doc_id.clone();
        assert_eq!(
            delete_goals_for_session_from_access(user_access, owner, "user-delete-session")
                .await
                .unwrap(),
            1
        );
        let rows = user_access
            .execute(
                r#"{
                    Goal(filter: { session_id: { _in: ["user-delete-session", "other-delete-session"] } }) {_docID session_id}
                    GoalCreationClaim(filter: { session_id: { _in: ["user-delete-session", "other-delete-session"] } }) {_docID session_id}
                }"#,
            )
            .await
            .unwrap();
        let goals = rows["data"]["Goal"].as_array().unwrap();
        let claims = rows["data"]["GoalCreationClaim"].as_array().unwrap();
        assert_eq!(goals.len(), 1);
        assert_eq!(goals[0]["_docID"], other_goal_doc_id);
        assert_eq!(claims.len(), 1);
        assert_eq!(claims[0]["session_id"], "other-delete-session");
    }

    // Signed relationship repair rebuilds after a concurrent Goal write.
    {
        let session_doc = gh.create_shared_session("repair-session").await;
        let input = json!({
            "goal_id": "race-goal", "creation_key": "race-claim", "node_did": owner,
            "session_id": "repair-session", "session_doc_id": session_doc, "requester_did": user.did(),
            "agent_did": agent.did(), "creator_did": user.did(), "parent_linked": false,
            "objective": "Finish work", "created_at": "2026-11-04T00:00:00Z"
        });
        let goal_doc = user_access
            .transact_write("test.goal_repair_race.unlinked", |txn| {
                let input = &input;
                Box::pin(async move {
                    let written = txn
                        .execute_client_signed_create(
                            "mutation($input: GoalMutationInputArg!) { create_Goal(input: $input) { _docID } }",
                            &json!({"input": input}),
                            gents_protocol::schemas::GOAL,
                        )
                        .await?;
                    let doc = gents_store::graphql::created_doc_id(&written, "Goal")?;
                    Ok::<_, anyhow::Error>(doc)
                })
            })
            .await
            .unwrap();
        let query = format!(
            r#"{{ Goal(filter: {{_docID: {{_eq: "{}"}}}}) {{_docID parent_linked objective}} }}"#,
            escape_graphql_string(&goal_doc)
        );
        let rewrite = async {
            user_access
                .transact_write("test.goal_repair_race.rewrite", |txn| {
                    let goal_doc = &goal_doc;
                    Box::pin(async move {
                        txn.execute_client_signed_update(
                            "mutation($id: ID!, $input: GoalMutationInputArg!) { update_Goal(filter: { _docID: { _eq: $id } }, input: $input) { _docID } }",
                            &json!({"id": goal_doc, "input": {"objective": "Finish rewritten work"}}),
                            gents_protocol::schemas::GOAL,
                        )
                        .await
                    })
                })
                .await
                .expect("the creator rewrites the Goal between the repair's mark and commit");
        };
        let (repaired, paused) = ConfigApplyTxn::with_concurrent_write_before_commit(
            crate::goal::creation::finish_goal_relationships(user_access, owner, "repair-session"),
            rewrite,
        )
        .await;
        assert!(paused, "the repair paused after its signed marker");
        repaired.unwrap();
        let rows = user_access.execute(&query).await.unwrap();
        let rows = rows["data"]["Goal"].as_array().unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0]["parent_linked"], true);
        assert_eq!(rows[0]["objective"], "Finish rewritten work");
        assert!(
            reader_sees(&gh.reader_access, &query, "Goal").await,
            "the session reader sees the Goal through its parent grant"
        );
        assert!(
            reader_sees(agent_access, &query, "Goal").await,
            "the working Agent sees the Goal through its grant"
        );
    }
    gh.home.node.shutdown().await.expect("node shutdown");
}

#[tokio::test]
async fn goal_and_claim_publication_links_match_lean() {
    #[derive(Debug, Clone, Copy)]
    struct LinkObservation {
        linked: bool,
        reader: bool,
        agent: bool,
    }

    struct LinkRun {
        owner: Option<&'static str>,
        initial: LinkObservation,
        after: Vec<LinkObservation>,
        after_revocation: Option<LinkObservation>,
        repair_allowed: Option<bool>,
    }

    let gh = GoalHome::start("goal-links").await;
    let mut ordinal = 0usize;

    let mut run_link_case = async |collection: &str,
                                   actor_name: &str,
                                   mode: &str,
                                   commit: bool,
                                   steps: &[[bool; 3]]|
           -> LinkRun {
        ordinal += 1;
        let session_id = format!("links-session-{ordinal}");
        let goal_id = format!("model-goal-{ordinal}");
        let creation_key = format!("model-claim-{ordinal}");
        let runtime_did = gh.home.node_identity.did();
        let user_did = gh.home.user.did();
        let agent_did = gh.home.agent_did();
        let user_access = &gh.home.user_access;
        let agent_access = &gh.home.agent_access;
        let session_doc = gh.create_shared_session(&session_id).await;
        let session_query = format!(
            r#"{{ AgentSession(filter: {{_docID: {{_eq: "{}"}}}}) {{_docID}} }}"#,
            escape_graphql_string(&session_doc)
        );
        assert!(
            reader_sees(&gh.reader_access, &session_query, "AgentSession").await,
            "modeled reader must be able to query its granted session"
        );
        let (actor, access) = match actor_name {
            "user" => (user_did, user_access),
            "agent" => (agent_did, agent_access),
            other => panic!("unknown model actor {other}"),
        };
        let input = json!({
            "goal_id": goal_id, "creation_key": creation_key, "node_did": runtime_did,
            "session_id": session_id, "session_doc_id": session_doc, "requester_did": user_did,
            "agent_did": agent_did, "creator_did": actor, "parent_linked": mode == "fresh",
            "objective": "Finish work", "created_at": "2026-11-04T00:00:00Z"
        });
        let author_access = if mode == "foreign" {
            agent_access
        } else {
            access
        };
        let staged_doc = std::sync::Mutex::new(None::<String>);
        let outcome = author_access.transact_write("test.goal_links.publication", |txn| {
            let input = &input;
            let session_id = &session_id;
            let staged_doc = &staged_doc;
            Box::pin(async move {
                let doc = if mode == "fresh" {
                    let staged = stage_goal_and_claim(
                        txn,
                        GoalStaging {
                            start: crate::operations::goal::GoalStart {
                                node_did: runtime_did,
                                session_id,
                                objective: "Finish work",
                                token_budget: None,
                                limits: gents_protocol::goal::GoalLimits::default(),
                            },
                            initial_state: GoalState {
                                status: GoalStatus::Active,
                                wrapup_requested: false,
                                wrapup_completed: false,
                            },
                            now: "2026-11-04T00:00:00Z",
                            identity: None,
                            first_request_doc_id: None,
                        },
                    )
                    .await?;
                    if collection == "Goal" {
                        staged.goal.doc_id
                    } else {
                        let response = txn
                            .execute(&format!(
                                r#"{{ GoalCreationClaim(filter: {{ session_id: {{ _eq: "{}" }} }}) {{ _docID }} }}"#,
                                escape_graphql_string(session_id)
                            ))
                            .await?;
                        response["data"]["GoalCreationClaim"][0]["_docID"]
                            .as_str()
                            .unwrap()
                            .to_owned()
                    }
                } else {
                    let schema = match collection {
                        "Goal" => gents_protocol::schemas::GOAL,
                        "GoalCreationClaim" => gents_protocol::schemas::GOAL_CREATION_CLAIM,
                        other => panic!("unknown modeled collection {other}"),
                    };
                    let mutation = format!(
                        "mutation($input: {collection}MutationInputArg!) {{ create_{collection}(input: $input) {{ _docID }} }}"
                    );
                    let written = txn
                        .execute_client_signed_create(&mutation, &json!({"input": input}), schema)
                        .await?;
                    gents_store::graphql::created_doc_id(&written, collection)?
                };
                *staged_doc.lock().unwrap() = Some(doc.clone());
                if !commit {
                    anyhow::bail!("test requested publication rollback");
                }
                Ok::<_, anyhow::Error>(doc)
            })
        }).await;
        let doc = if commit {
            outcome.unwrap()
        } else {
            assert_eq!(
                outcome.unwrap_err().to_string(),
                "test requested publication rollback"
            );
            staged_doc.into_inner().unwrap().unwrap()
        };
        if mode == "foreign" {
            agent_access
                .add_actor_relationship(
                    collection,
                    &doc,
                    "reader",
                    &identity::Did::new(actor).unwrap(),
                )
                .await
                .unwrap();
        }
        if mode == "partial" {
            access
                .add_resource_relationship(collection, &doc, "parent", "sessions", &session_doc)
                .await
                .unwrap();
        }
        let query = format!(
            r#"{{ {collection}(filter: {{_docID: {{_eq: "{}"}}}}) {{_docID parent_linked}} }}"#,
            escape_graphql_string(&doc)
        );
        let observe = || {
            let query = &query;
            let reader_access = &gh.reader_access;
            async move {
                let rows = access.execute(query).await.unwrap();
                LinkObservation {
                    linked: rows["data"][collection]
                        .as_array()
                        .unwrap()
                        .first()
                        .is_some_and(|row| row["parent_linked"] == true),
                    reader: reader_sees(reader_access, query, collection).await,
                    agent: !agent_access.execute(query).await.unwrap()["data"][collection]
                        .as_array()
                        .unwrap()
                        .is_empty(),
                }
            }
        };
        let rows = access.execute(&query).await.unwrap();
        if rows["data"][collection].as_array().unwrap().is_empty() {
            return LinkRun {
                owner: None,
                initial: observe().await,
                after: Vec::new(),
                after_revocation: None,
                repair_allowed: None,
            };
        }
        let owners = author_access
            .transact_readonly("test.goal_links.owner", |txn| {
                let doc = &doc;
                Box::pin(async move {
                    txn.document_owners(collection, std::slice::from_ref(doc))
                        .await
                })
            })
            .await
            .unwrap();
        let owner = match owners[0].owner.as_ref().map(|did| did.as_str()) {
            Some(did) if did == user_did => Some("user"),
            Some(did) if did == agent_did => Some("agent"),
            other => panic!("unexpected goal link owner {other:?}"),
        };
        if mode == "foreign" {
            let repair_allowed =
                crate::goal::creation::finish_goal_relationships(access, runtime_did, &session_id)
                    .await
                    .is_ok();
            return LinkRun {
                owner,
                initial: observe().await,
                after: Vec::new(),
                after_revocation: None,
                repair_allowed: Some(repair_allowed),
            };
        }
        let initial = observe().await;
        let mut after = Vec::new();
        for &[parent_ok, worker_ok, mark_ok] in steps {
            if parent_ok && worker_ok && mark_ok {
                crate::goal::creation::finish_goal_relationships(access, runtime_did, &session_id)
                    .await
                    .unwrap();
            } else {
                let result: anyhow::Result<()> = access
                    .transact_write("test.goal_links.rollback", |attempt| {
                        let doc = &doc;
                        let session_doc = &session_doc;
                        Box::pin(async move {
                            if parent_ok {
                                attempt
                                    .add_resource_relationship(
                                        collection,
                                        doc,
                                        "parent",
                                        "sessions",
                                        session_doc,
                                    )
                                    .await?;
                            }
                            if worker_ok {
                                attempt
                                    .add_actor_relationship(
                                        collection,
                                        doc,
                                        "working_agent",
                                        &identity::Did::new(agent_did)?,
                                    )
                                    .await?;
                            }
                            anyhow::bail!("test requested relationship rollback")
                        })
                    })
                    .await;
                assert_eq!(
                    result.unwrap_err().to_string(),
                    "test requested relationship rollback"
                );
            }
            after.push(observe().await);
        }
        access
            .delete_resource_relationship(collection, &doc, "parent", "sessions", &session_doc)
            .await
            .unwrap();
        crate::goal::creation::finish_goal_relationships(access, runtime_did, &session_id)
            .await
            .unwrap();
        LinkRun {
            owner,
            initial,
            after,
            after_revocation: Some(observe().await),
            repair_allowed: None,
        }
    };

    for case in gents_lean_vocab::families::goal_links_user_owned::cases() {
        let i = &case.input;
        let steps: Vec<[bool; 3]> = i
            .steps
            .iter()
            .map(|step| [step.parent_ok, step.worker_ok, step.mark_ok])
            .collect();
        let run = run_link_case(&i.collection, &i.actor, &i.mode, i.commit, &steps).await;
        let observed = |o: LinkObservation| {
            gents_lean_vocab::families::goal_links_user_owned::UserObservation {
                linked: o.linked,
                parent: o.reader,
                agent_visible: o.agent,
            }
        };
        case.check(
            gents_lean_vocab::families::goal_links_user_owned::UserOutput {
                owner: run.owner.map(str::to_owned),
                initial: observed(run.initial),
                after: run.after.into_iter().map(observed).collect(),
                after_revocation: run.after_revocation.map(observed),
            },
        );
    }
    for case in gents_lean_vocab::families::goal_links_agent_owned::cases() {
        let i = &case.input;
        let steps: Vec<[bool; 3]> = i
            .steps
            .iter()
            .map(|step| [step.parent_ok, step.worker_ok, step.mark_ok])
            .collect();
        let run = run_link_case(&i.collection, &i.actor, &i.mode, i.commit, &steps).await;
        let observed = |o: LinkObservation| {
            gents_lean_vocab::families::goal_links_agent_owned::AgentObservation {
                linked: o.linked,
                parent: o.reader,
            }
        };
        case.check(
            gents_lean_vocab::families::goal_links_agent_owned::AgentOutput {
                owner: run.owner.map(str::to_owned),
                initial: observed(run.initial),
                after: run.after.into_iter().map(observed).collect(),
                after_revocation: run.after_revocation.map(observed),
            },
        );
    }
    for case in gents_lean_vocab::families::goal_links_foreign::cases() {
        let i = &case.input;
        let steps: Vec<[bool; 3]> = i
            .steps
            .iter()
            .map(|step| [step.parent_ok, step.worker_ok, step.mark_ok])
            .collect();
        let run = run_link_case(&i.collection, &i.actor, &i.mode, i.commit, &steps).await;
        case.check(
            gents_lean_vocab::families::goal_links_foreign::ForeignOutput {
                owner: run.owner.map(str::to_owned),
                repair_allowed: run.repair_allowed.expect("a foreign row attempts repair"),
                linked: run.initial.linked,
                parent: run.initial.reader,
            },
        );
    }
    gh.home.node.shutdown().await.expect("node shutdown");
}
