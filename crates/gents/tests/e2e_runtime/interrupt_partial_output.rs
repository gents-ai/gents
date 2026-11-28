use gents::test_support::running_runtime::{AgentRuntimeHome, BootedRuntime};
use gents_inference::inference_observation::InferenceKey;
use gents_inference::test_support::scripted_backend::{ScriptedBackend, StreamScript};
use gents_lean_vocab::tla::replay::{Red, Trace};
use gents_protocol::output::rows::{decode_output_segment_row, AGENT_OUTPUT_SEGMENT_FIELDS};
use gents_protocol::output::OutputSource;
use gents_protocol::request_lifecycle::RequestLifecycleState;
use gents_store::graphql::escape_graphql_string;
use gents_store::identity::NodeIdentity;
use gents_store::test_support::snapshot_reader::SnapshotReader;
use gents_store::test_support::{rows_when, STALL_CEILING};

async fn replay(trace: Trace) {
    let chunks = ["first chunk.", " second chunk."];
    assert_eq!(trace.constants["Chunks"], chunks.len());
    let supplied = trace.steps.iter().filter(|s| s.action == "Receive").count();
    let backend = ScriptedBackend::start(
        "default",
        vec![StreamScript::paused(
            "stream-body",
            chunks[..supplied].iter().copied(),
        )],
    )
    .unwrap();
    let first_poll = backend.hold("start-attempt");
    let home = AgentRuntimeHome::start(
        "interrupt-partial-output",
        None,
        backend.endpoint(),
        1,
        gents_inference::backends::registry::HEALTHY_PROBE_STATUS,
    )
    .await;
    let runtime = BootedRuntime::boot(&home, Default::default()).await;
    let request = "request-with-partial";
    let session = "session-with-partial";
    let doc = home
        .submit_request(
            &home.binding.agent_did,
            request,
            session,
            "start-attempt stream-body",
        )
        .await;
    first_poll.entered().await;
    let access = home.agent_access();
    let escaped = escape_graphql_string(&doc);
    let query = format!(
        r#"{{ AgentRequest(filter: {{_docID: {{_eq: "{escaped}"}}}}) {{execution_generation lifecycle_state}} }}"#
    );
    let processing = home.home.user_access.read_data(&query).await;
    assert_eq!(
        processing["AgentRequest"][0]["lifecycle_state"],
        "processing"
    );
    let key = InferenceKey {
        node: runtime.node_did().into(),
        session: session.into(),
        request: request.into(),
        generation: processing["AgentRequest"][0]["execution_generation"]
            .as_str()
            .unwrap()
            .into(),
    };
    let mut receipts = runtime
        .observations
        .received_text_chunks(&key)
        .expect("active attempt at the provider");
    first_poll.release();
    let mut interrupted = false;
    let mut received = 0;
    for step in &trace.steps {
        match step.action.as_str() {
            "Receive" => {
                received += 1;
                tokio::time::timeout(STALL_CEILING, receipts.wait_for(|count| *count >= received))
                    .await
                    .expect("provider receipt stalled")
                    .unwrap();
            }
            "Latch" => {
                gents_session::request::interrupt::interrupt_request_by_doc_id_with_access(
                    &home.home.user_access,
                    &doc,
                    runtime.node_did(),
                    Some(home.home.user.did()),
                )
                .await
                .unwrap();
                interrupted = true;
            }
            "EndOfStream" => backend.hold("stream-body").close(),
            "Process" | "Observe" | "InterruptBranch" | "Drain" | "Terminalize" => {}
            action => panic!("unmapped partial-output action {action}"),
        }
    }
    let expected = if interrupted {
        RequestLifecycleState::Interrupted
    } else {
        RequestLifecycleState::Failed
    };
    let finished = rows_when(&home.home.user_access, "AgentRequest", &query, |data| {
        let row = &data["AgentRequest"][0];
        row["lifecycle_state"] != RequestLifecycleState::Processing.as_str()
            || row["execution_generation"] != key.generation
    })
    .await;
    assert_eq!(
        finished["AgentRequest"][0]["lifecycle_state"],
        expected.as_str(),
        "{}: {finished}",
        trace.config
    );
    let query = format!(
        r#"{{ AgentOutputSegment(filter: {{request_doc_id: {{_eq: "{escaped}"}}}}) {{ {AGENT_OUTPUT_SEGMENT_FIELDS} }} }}"#
    );
    let response = access.execute(&query).await.unwrap();
    let mut segments = response["data"]["AgentOutputSegment"]
        .as_array()
        .unwrap()
        .iter()
        .map(|row| decode_output_segment_row(row).unwrap().segment)
        .filter(|segment| matches!(segment.source, OutputSource::ProviderTurn { .. }))
        .collect::<Vec<_>>();
    assert_eq!(segments.iter().filter(|s| s.close.is_some()).count(), 1);
    segments.sort_by_key(|s| s.ordinal);
    let durable: String = segments.iter().map(|s| s.payload.as_str()).collect();
    let observed = *receipts.borrow();
    assert_eq!(observed, received);
    let mut properties = vec![(
        "NoReceivedOutputDropped",
        durable == chunks[..observed].concat(),
    )];
    let red = if trace.config == "MCInterruptPartialOutputWitness" {
        properties.push((
            "NoInterruptAfterOutput",
            !(interrupted && !durable.is_empty()),
        ));
        Red::Witness
    } else {
        Red::Defect
    };
    trace.check(red, &properties);
    runtime.shutdown().await.unwrap();
    backend.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn received_chunks_survive_the_terminal_drain() {
    for json in [
        include_str!("../../proofs/tla/generated/traces/MCInterruptPartialOutputNoDrainFlush.json"),
        include_str!("../../proofs/tla/generated/traces/MCInterruptPartialOutputWitness.json"),
    ] {
        replay(Trace::load(json, "InterruptPartialOutput", &[])).await;
    }
}
