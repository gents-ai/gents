pub mod args;
pub mod cli_process;
pub mod defaults;
pub mod http;
pub mod http_client;
pub mod p2p_relay;
pub mod p2p_status;
pub mod prepare_runtime;
pub mod readiness;
pub mod serve;
#[cfg(any(test, feature = "test-fixtures"))]
pub mod test_support;
pub mod tool_ceiling;
pub mod usage_read;

#[cfg(test)]
#[ctor::ctor]
fn select_unit_test_homes() {
    gents_service::home_state::use_unit_test_homes();
}
