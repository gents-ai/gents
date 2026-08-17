//! No-infrastructure CLI suites: pure filesystem/exit-code/help surface.
//! Aggregated per the test-estate consolidation: one link unit instead of
//! eight. Run one module with `cargo test -p gents-cli --test cli_offline <module>::`.

mod support;

#[path = "suites/cli_config_apply_order.rs"]
mod cli_config_apply_order;
#[path = "suites/cli_config_diff.rs"]
mod cli_config_diff;
#[path = "suites/cli_config_export_import.rs"]
mod cli_config_export_import;
#[path = "suites/cli_config_native_root.rs"]
mod cli_config_native_root;
#[path = "suites/cli_config_validate.rs"]
mod cli_config_validate;
#[path = "suites/cli_help.rs"]
mod cli_help;
#[path = "suites/cli_provision.rs"]
mod cli_provision;
#[path = "suites/cli_schema.rs"]
mod cli_schema;
