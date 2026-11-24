use super::fork::{fork_on, ForkBoundary, ForkParams};
use super::history_fixture::signed_claimed_authored_request;
use super::tests::session_doc_id;
use super::*;
use crate::test_support::{fork_reader_node, history_on, session_rewrite_rows};
use gents_lean_vocab::families::contracts_fork_publication as family;
use gents_protocol::output::rows::{
    decode_output_segment_row, decode_transcript_message_row, AGENT_MESSAGE_FIELDS,
    AGENT_OUTPUT_SEGMENT_FIELDS,
};
use gents_protocol::output::{
    MessageBlock, MessagePublication, OutputOutcome, OutputSegment, OutputSource, OutputWriter,
    PayloadPresentation, PayloadRef, PresentedPayload, SegmentRun, SourceClose, StreamDeclaration,
    StreamPayload, TranscriptMessage,
};
use gents_store::config_client::ConfigAccess;
use gents_store::identity::NodeIdentity as _;
use std::collections::BTreeMap;

const SOURCE: &str = "modeled-fork-source";

fn timestamp(seconds: u64) -> String {
    chrono::DateTime::from_timestamp(i64::try_from(seconds).unwrap(), 0)
        .unwrap()
        .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn seconds(timestamp: &str) -> u64 {
    u64::try_from(
        chrono::DateTime::parse_from_rfc3339(timestamp)
            .unwrap()
            .timestamp(),
    )
    .unwrap()
}

/// Native documents of one modeled history, keyed by model document id.
#[derive(Default)]
struct Imported {
    messages: BTreeMap<u64, String>,
    compactions: BTreeMap<u64, String>,
    closures: BTreeMap<u64, String>,
}

impl Imported {
    fn model_id(map: &BTreeMap<u64, String>, native: &str) -> u64 {
        *map.iter()
            .find(|(_, doc)| doc.as_str() == native)
            .unwrap_or_else(|| panic!("{native} is no imported source document"))
            .0
    }
}

struct Scope<'a> {
    node_did: &'a str,
    requester_did: &'a str,
    request_doc: &'a str,
}

fn closing_record(scope: &Scope<'_>, closure: u64, generation: &str) -> OutputSegment {
    let text = format!("closure {closure}");
    OutputSegment {
        node_did: scope.node_did.into(),
        requester_did: Some(scope.requester_did.into()),
        session_id: SOURCE.into(),
        request_doc_id: scope.request_doc.into(),
        source: OutputSource::Authored {
            key: format!("closure-{closure}"),
        },
        writer: OutputWriter::RequestExecution {
            execution_generation: generation.into(),
        },
        ordinal: Some(0),
        runs: vec![SegmentRun {
            stream: 0,
            bytes: text.len().try_into().unwrap(),
            declaration: Some(StreamDeclaration {
                block_index: 0,
                part_index: 0,
                payload: StreamPayload::Text,
            }),
        }],
        payload: text.clone(),
        close: Some(SourceClose::Closed {
            outcome: OutputOutcome::Complete,
            segments: 1,
            stream_bytes: vec![text.len() as u64],
        }),
        created_at: timestamp(0),
    }
}

fn source_header(
    scope: &Scope<'_>,
    message: &family::MessageWire,
    closures: &BTreeMap<u64, String>,
) -> TranscriptMessage {
    let publication = match (
        message.publication.kind.as_str(),
        message.publication.generation,
    ) {
        ("request_execution", Some(generation)) => MessagePublication::RequestExecution {
            execution_generation: generation.to_string(),
        },
        other => panic!("untranslated source publication {other:?}"),
    };
    TranscriptMessage {
        message_key: message.message_key.clone(),
        session_id: SOURCE.into(),
        node_did: scope.node_did.into(),
        requester_did: Some(scope.requester_did.into()),
        request_doc_id: message.request_doc_id.map(|_| scope.request_doc.to_owned()),
        publication,
        outcome: OutputOutcome::Complete,
        sequence: u32::try_from(message.sequence).unwrap(),
        role: serde_json::from_value(serde_json::json!(message.role)).unwrap(),
        native_id: message.native_id.clone(),
        native_request_id: message.native_request_id.clone(),
        blocks: message
            .payload_refs
            .iter()
            .map(|reference| MessageBlock::Text {
                text: PresentedPayload {
                    output: PayloadRef {
                        close_doc_id: closures[&reference.close_id].clone(),
                        stream: u32::try_from(reference.stream).unwrap(),
                    },
                    presentation: PayloadPresentation::Full,
                },
            })
            .collect(),
        created_at: timestamp(message.created_at),
    }
}

/// Writes the modeled source as its session's Agent writes native output:
/// each closing record, then each header naming them, then each compaction
/// row exactly as modeled, so a case's invalid chain is stored as it is.
async fn import_source(
    client: &ConfigAccess,
    agent: &ConfigAccess,
    agent_did: &identity::Did,
    scope: &Scope<'_>,
    source: &family::HistoryWire,
) -> Imported {
    use gents_protocol::document_query::{fields::compaction_entry as fields, Create, Input};
    use gents_protocol::output::rows::{
        CREATE_AGENT_MESSAGE_MUTATION, CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
    };
    use gents_store::config_client::session_children::{
        output_segment_bound_variables, publish_agent_session_child_relationships_with_access,
        transcript_message_bound_variables,
    };
    let generation = source
        .messages
        .iter()
        .find_map(|message| message.publication.generation)
        .unwrap_or_default()
        .to_string();
    let mut imported = Imported::default();
    for closure in &source.closures {
        assert_eq!(
            closure.logical, closure.doc_id,
            "a source closing record is its own logical reference"
        );
        let segment = closing_record(scope, closure.doc_id, &generation);
        let doc = agent
            .transact_write("test.import_fork_closure", |txn| {
                let segment = &segment;
                Box::pin(async move {
                    let response = txn
                        .execute_with_variables(
                            CREATE_AGENT_OUTPUT_SEGMENT_MUTATION,
                            &output_segment_bound_variables(txn, segment).await?,
                        )
                        .await?;
                    Ok::<_, anyhow::Error>(gents_store::graphql::created_doc_id(
                        &response,
                        "AgentOutputSegment",
                    )?)
                })
            })
            .await
            .unwrap();
        imported.closures.insert(closure.doc_id, doc);
    }
    for message in &source.messages {
        let header = source_header(scope, message, &imported.closures);
        let doc = agent
            .transact_write("test.import_fork_header", |txn| {
                let header = &header;
                Box::pin(async move {
                    let variables = transcript_message_bound_variables(txn, header).await?;
                    let response = txn
                        .execute_with_variables(CREATE_AGENT_MESSAGE_MUTATION, &variables)
                        .await?;
                    Ok::<_, anyhow::Error>(gents_store::graphql::created_doc_id(
                        &response,
                        "AgentMessage",
                    )?)
                })
            })
            .await
            .unwrap();
        imported.messages.insert(message.doc_id, doc);
    }
    publish_agent_session_child_relationships_with_access(agent, scope.node_did, agent_did, SOURCE)
        .await
        .unwrap();
    let session_doc = session_doc_id(client, SOURCE).await;
    for compaction in &source.compactions {
        let sequence = u32::try_from(compaction.sequence).unwrap();
        let input = Input::new()
            .set(
                fields::COMPACTION_KEY,
                super::compaction_entries::compaction_key(
                    scope.node_did,
                    SOURCE,
                    Some(scope.requester_did),
                    sequence,
                ),
            )
            .set(fields::SESSION_ID, SOURCE)
            .set(fields::NODE_DID, scope.node_did)
            .set(fields::REQUESTER_DID, scope.requester_did)
            .set(fields::REQUEST_ID, "modeled-compaction")
            .set(fields::REQUEST_DOC_ID, scope.request_doc)
            .set(fields::SEQUENCE, sequence)
            .set(fields::SUMMARY, "summary")
            .set(
                fields::FILES_READ,
                super::compaction_entries::encode_file_path_delta(&[]).unwrap(),
            )
            .set(
                fields::FILES_MODIFIED,
                super::compaction_entries::encode_file_path_delta(&[]).unwrap(),
            )
            .set(fields::MESSAGES_COMPACTED, 1u32)
            .set(
                fields::COMPACTED_THROUGH_SEQUENCE,
                u32::try_from(compaction.through_sequence).unwrap(),
            )
            .set(fields::ORIGINAL_TOKENS, 100u32)
            .set(fields::COMPACTED_TOKENS, 10u32)
            .set(fields::CREATED_AT, timestamp(0));
        let mutation = Create::new(input).render();
        let doc = agent
            .transact_write("test.import_fork_compaction", |txn| {
                let mutation = &mutation;
                let session_doc = &session_doc;
                Box::pin(async move {
                    let response = txn.run(mutation).await?;
                    let doc = gents_store::graphql::created_doc_id(&response, "CompactionEntry")?;
                    txn.add_resource_relationship(
                        "CompactionEntry",
                        &doc,
                        "parent",
                        "sessions",
                        session_doc,
                    )
                    .await?;
                    Ok::<_, anyhow::Error>(doc)
                })
            })
            .await
            .unwrap();
        imported.compactions.insert(compaction.doc_id, doc);
    }
    imported
}

/// The user turn whose boundary is the case's exclusive sequence cut: the
/// first user turn at or after the cut, or one past the last turn when the
/// cut follows every message.
fn boundary_turn(input: &family::ForkInput) -> u32 {
    let turns = input
        .source
        .messages
        .iter()
        .filter(|message| message.role == "user")
        .map(|message| message.sequence)
        .collect::<Vec<_>>();
    let turn = turns
        .iter()
        .filter(|sequence| **sequence < input.exclusive_sequence_cut)
        .count();
    let after_last = input
        .source
        .messages
        .iter()
        .map(|message| message.sequence + 1)
        .max()
        .unwrap_or(0);
    assert_eq!(
        turns.get(turn).copied().unwrap_or(after_last),
        input.exclusive_sequence_cut,
        "{}: the cut is a user-turn boundary",
        input.name
    );
    u32::try_from(turn).unwrap()
}

async fn child_rows(
    client: &ConfigAccess,
    node_did: &str,
    child: &str,
    requester: &str,
) -> serde_json::Value {
    let scope = format!(
        r#"node_did: {{ _eq: "{}" }}, session_id: {{ _eq: "{}" }}, requester_did: {{ _eq: "{}" }}"#,
        escape_graphql_string(node_did),
        escape_graphql_string(child),
        escape_graphql_string(requester),
    );
    client
        .execute(&format!(
            r#"{{
            AgentMessage(filter: {{ {scope} }}, order: {{ sequence: ASC }}) {{ {AGENT_MESSAGE_FIELDS} }}
            CompactionEntry(filter: {{ {scope} }}, order: {{ sequence: ASC }}) {{ _docID session_id sequence compacted_through_sequence }}
            AgentOutputSegment(filter: {{ {scope} }}) {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }}
        }}"#
        ))
        .await
        .unwrap()
}

/// The child's stored rows in the model's terms: each copy's document id is
/// the offset applied to the source document it copies, each session id the
/// scope it names, and each key the prefix applied to its origin's key.
fn published_history(
    input: &family::ForkInput,
    imported: &Imported,
    child: &str,
    rows: &serde_json::Value,
) -> family::HistoryWire {
    let offset = input.document_id_offset;
    let session = |id: &str| match id {
        SOURCE => input.parent.session,
        id if id == child => input.child.session,
        other => panic!("{other} is neither fork session"),
    };
    let origin_message = |doc: &str| {
        input
            .source
            .messages
            .iter()
            .find(|message| message.doc_id == Imported::model_id(&imported.messages, doc))
            .unwrap()
    };
    let messages = rows["data"]["AgentMessage"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| decode_transcript_message_row(row).unwrap().message)
        .map(|message| {
            let MessagePublication::Fork {
                origin_message_doc_id,
            } = &message.publication
            else {
                panic!("a fork copy is published as a fork: {message:?}")
            };
            let origin = origin_message(origin_message_doc_id);
            family::MessageWire {
                doc_id: origin.doc_id + offset,
                session: session(&message.session_id),
                message_key: format!("{}{}", input.child_key_prefix, origin.message_key),
                sequence: u64::from(message.sequence),
                native_id: message.native_id.clone(),
                native_request_id: message.native_request_id.clone(),
                created_at: seconds(&message.created_at),
                request_doc_id: message.request_doc_id.as_ref().map(|_| {
                    origin
                        .request_doc_id
                        .expect("a copy names a request only its origin named")
                }),
                origin_header_id: Some(origin.doc_id),
                payload_refs: message
                    .payload_references()
                    .into_iter()
                    .map(|reference| family::PayloadRefWire {
                        close_id: Imported::model_id(&imported.closures, &reference.close_doc_id),
                        stream: u64::from(reference.stream),
                    })
                    .collect(),
                role: serde_json::to_value(message.role)
                    .unwrap()
                    .as_str()
                    .unwrap()
                    .to_owned(),
                publication: family::PublicationWire {
                    kind: "fork".into(),
                    generation: None,
                    call: None,
                    origin: Some(origin.doc_id),
                },
            }
        })
        .collect::<Vec<_>>();
    let compactions = rows["data"]["CompactionEntry"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            let sequence = row["sequence"].as_u64().unwrap();
            let origin = input
                .source
                .compactions
                .iter()
                .find(|compaction| compaction.sequence == sequence)
                .expect("a copied compaction keeps its source sequence");
            family::CompactionWire {
                doc_id: origin.doc_id + offset,
                session: session(row["session_id"].as_str().unwrap()),
                sequence,
                through_sequence: row["compacted_through_sequence"].as_u64().unwrap(),
            }
        })
        .collect();
    let closing = rows["data"]["AgentOutputSegment"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| decode_output_segment_row(row).unwrap())
        .filter(|row| row.segment.close.is_some())
        .collect::<Vec<_>>();
    let mut closures = Vec::new();
    for reference in messages.iter().flat_map(|message| &message.payload_refs) {
        if closures
            .iter()
            .any(|closure: &family::HistoryClosureWire| closure.logical == reference.close_id)
        {
            continue;
        }
        let logical = &imported.closures[&reference.close_id];
        let [record] = closing
            .iter()
            .filter(|row| &row.segment.request_doc_id == logical)
            .collect::<Vec<_>>()[..]
        else {
            panic!("the child holds one closing record for {logical}")
        };
        closures.push(family::HistoryClosureWire {
            doc_id: reference.close_id + offset,
            session: session(&record.segment.session_id),
            logical: reference.close_id,
        });
    }
    assert_eq!(
        closing.len(),
        closures.len(),
        "the child holds only the closing records its copies reference"
    );
    family::HistoryWire {
        messages,
        compactions,
        closures,
    }
}

/// Which direct session grant lets another user read the child's whole
/// transcript as its owner reads it.
async fn fork_readers(
    db: &gents_store::test_support::TestStore,
    client: &ConfigAccess,
    requester: &str,
    child: &str,
) -> Vec<family::ForkReader> {
    use gents_store::config_client::NodePermission;
    let node_did = db.node_identity.did();
    let owned = history_on(client, child, node_did, Some(requester), None)
        .await
        .unwrap();
    let sharee =
        tokio::task::spawn_blocking(|| gents_store::test_support::test_identity("fork-sharee"))
            .await
            .unwrap();
    let sharee_did = identity::Did::new(sharee.did()).unwrap();
    let admin = ConfigAccess::local_as(db.node.clone(), identity::Did::new(node_did).unwrap());
    for permission in [NodePermission::DocumentRead, NodePermission::CollectionGet] {
        admin
            .grant_node_permission(&sharee_did, permission)
            .await
            .unwrap();
    }
    let sharee_access = ConfigAccess::local_as(db.node.clone(), sharee_did.clone());
    let mut readers = Vec::new();
    for (grant, session) in [
        ("child_session", Some(child)),
        ("source_session", Some(SOURCE)),
        ("unrelated_session", None),
    ] {
        let doc = match session {
            Some(session) => Some(session_doc_id(client, session).await),
            None => None,
        };
        if let Some(doc) = &doc {
            client
                .add_actor_relationship("AgentSession", doc, "reader", &sharee_did)
                .await
                .unwrap();
        }
        readers.push(family::ForkReader {
            grant: grant.into(),
            reads_transcript: history_on(&sharee_access, child, node_did, Some(requester), None)
                .await
                .is_ok_and(|history| history == owned),
        });
        if let Some(doc) = &doc {
            client
                .delete_actor_relationship("AgentSession", doc, "reader", &sharee_did)
                .await
                .unwrap();
        }
    }
    readers
}

#[tokio::test]
async fn generated_fork_publication_replays_the_served_fork_of_each_source() {
    let cases = family::cases();
    assert!(!cases.is_empty());
    for case in cases {
        let input = &case.input;
        let (db, user, client, binding) =
            fork_reader_node(&format!("fork-publication-{}", input.name)).await;
        let node_did = db.node_identity.did();
        let agent =
            gents_store::identity::resolve_agent_identity(db.node_identity.as_ref(), &binding)
                .unwrap();
        let agent_did = identity::Did::new(agent.did()).unwrap();
        let agent_access =
            ConfigAccess::local_managed_agent(db.node.clone(), agent_did.clone(), agent.as_ref())
                .unwrap();
        let mut lifecycle = signed_claimed_authored_request(
            &db,
            &user,
            &client,
            &binding,
            &format!("{SOURCE}-request"),
            SOURCE,
        )
        .await;
        let writer = crate::request::streaming::DefraStreamWriter::new_for_lifecycle(
            db.node.clone(),
            &lifecycle,
            std::time::Duration::ZERO,
        )
        .unwrap();
        lifecycle.begin_owned_execution(&writer).await.unwrap();
        let request_doc = lifecycle.request().doc_id.clone();
        let scope = Scope {
            node_did,
            requester_did: user.did(),
            request_doc: &request_doc,
        };
        if input.idle {
            lifecycle
                .terminalize_owned(
                    crate::request::lifecycle::RequestTerminalOutcome::Completed,
                    gents_protocol::output::TerminalOutput::NoMessage,
                    None,
                )
                .await
                .unwrap();
        }
        let imported =
            import_source(&client, &agent_access, &agent_did, &scope, &input.source).await;
        let foreign =
            tokio::task::spawn_blocking(|| gents_store::test_support::test_identity("fork-other"))
                .await
                .unwrap();
        let before = session_rewrite_rows(&client).await;
        let forked = fork_on(
            &client,
            ForkParams {
                source_session_id: SOURCE,
                boundary: ForkBoundary::UserTurn(boundary_turn(input)),
                caller_node_did: if input.authorized {
                    node_did
                } else {
                    foreign.did()
                },
                caller_requester_did: Some(user.did()),
                target_agent_did: None,
            },
        )
        .await;
        let output = match forked {
            Ok(receipt) => {
                let rows = child_rows(&client, node_did, &receipt.session_id, user.did()).await;
                family::ForkPublication {
                    published: Some(published_history(
                        input,
                        &imported,
                        &receipt.session_id,
                        &rows,
                    )),
                    readers: fork_readers(&db, &client, user.did(), &receipt.session_id).await,
                }
            }
            Err(error) => {
                assert_eq!(
                    session_rewrite_rows(&client).await,
                    before,
                    "{}: a refused fork writes nothing: {error:#}",
                    input.name
                );
                family::ForkPublication {
                    published: None,
                    readers: Vec::new(),
                }
            }
        };
        case.check(output);
        db.node.shutdown().await.expect("shut down test node");
    }
}
