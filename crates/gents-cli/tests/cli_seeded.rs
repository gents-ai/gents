//! EmbeddedNode-seeded CLI suites: read/format surfaces that need a store
//! but no server process.

mod support;

#[path = "suites/cli_adapter_interop_roundtrip.rs"]
mod cli_adapter_interop_roundtrip;
#[path = "suites/cli_background.rs"]
mod cli_background;
#[path = "suites/cli_mcp_probe.rs"]
mod cli_mcp_probe;
#[path = "suites/cli_trace_export.rs"]
mod cli_trace_export;
