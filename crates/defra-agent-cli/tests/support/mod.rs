#![allow(dead_code, unused_imports)]

pub mod fs;
pub mod graphql;
pub mod mocks;
pub mod ports;
pub mod process;
pub mod waits;

pub use fs::{
    assert_runtime_init_state, project_object_fields, read_captured_log, read_json_file,
    read_runtime_state_json, write_json_file, write_manifest_root_from_export,
};
pub use graphql::{doc_id_for_selection, escape_graphql_string, first_graphql_row, graphql_query};
pub use mocks::{
    completion_text_sse, request_contains_role_text, request_has_tool_result_message,
    request_system_message, request_tool_names, request_tool_result_text, tool_call_sse,
    HttpRequestData, MockChatEndpoint, MockModelEndpoint, MockOpenAIEndpoint,
};
pub use ports::{allocate_port, graphql_url};
pub use process::{
    cli_bin, desktop_bin, run_cli_failure_stderr, run_cli_failure_stdout_json, run_cli_json,
    run_cli_text, run_desktop_init_json, run_init_json, spawn_cli, spawn_server,
    spawn_server_with_env, spawn_server_with_ready_json, wait_for_port, ServeProcess,
};
pub use waits::{
    insert_terminal_response, wait_for_completed_inference_behaviors,
    wait_for_completed_tool_calls, wait_for_connected_peer, wait_for_request,
    wait_for_runtime_doc_id, wait_for_runtime_quiescence, wait_for_runtime_ready,
    wait_for_tool_call,
};

pub const DEFAULT_MODEL_ENDPOINT: &str = "http://workstation-1:8000/v1";
pub const DEFAULT_MODEL_NAME: &str = "MiniMax-M2.7-NVFP4";
