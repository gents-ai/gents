mod support;

fn assert_exact_result_projection(actual: &str, expected_text: &str, result_doc_id: &str) {
    assert!(
        !result_doc_id.trim().is_empty(),
        "lossless model output must still bind exact durable result evidence"
    );
    assert_eq!(
        actual, expected_text,
        "untruncated model-facing output must not render its durable result-document pointer"
    );
}

#[path = "e2e_subagent/r4_subagent_completion.rs"]
mod r4_subagent_completion;
#[path = "e2e_subagent/r4_subagent_tools.rs"]
mod r4_subagent_tools;
#[path = "e2e_subagent/r4c_list_background_tools.rs"]
mod r4c_list_background_tools;
#[path = "e2e_subagent/r4c_list_subagents.rs"]
mod r4c_list_subagents;
#[path = "e2e_subagent/r4c_read_subagent_transcript.rs"]
mod r4c_read_subagent_transcript;
#[path = "e2e_subagent/r4c_read_tool_output.rs"]
mod r4c_read_tool_output;
#[path = "e2e_subagent/r4c_steer_subagent.rs"]
mod r4c_steer_subagent;
#[path = "e2e_subagent/r6_background_recovery.rs"]
mod r6_background_recovery;
#[path = "e2e_subagent/r6_background_tools.rs"]
mod r6_background_tools;
#[path = "e2e_subagent/subagent_convergence.rs"]
mod subagent_convergence;
#[path = "e2e_subagent/subagent_enablement_e2e.rs"]
mod subagent_enablement_e2e;
