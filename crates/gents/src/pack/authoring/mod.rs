//! Pack authoring: the shared library surface behind `gents pack`
//! `scaffold`/`check`/`build`/`test`/`verify`.
//!
//! Everything here preserves the canonical behavior of the existing CLI
//! command modules (`crates/gents-cli/src/commands/pack/{scaffold,check,
//! build,test,inspect}.rs`) as native functions with typed inputs and
//! results, so the CLI adapters become thin printers over these owners
//! instead of the place the behavior lives. Nothing here spawns the CLI or
//! re-decides a rule an existing owner already decides: pack identity,
//! digests, configuration loading, graph compilation and plugin admission
//! stay with `crate::pack`, `crate::pack_archive`, `crate::graph_package`,
//! `crate::graph_pipeline` and `crate::plugin`.
//!
//! Two behaviors stay outside this layer because their owners are not in
//! this crate, and a caller integrating the CLI must keep running them:
//!
//! - Event-source filter validation runs every filter as a query against an
//!   embedded node through `ConfigAccess` and the CLI's pack-schema applier
//!   (`commands::schema::apply_pack_schemas_if_present`). [`check::CheckReport`]
//!   names what remains unverified instead of silently skipping it.
//! - Plugin compilation shells out to external language toolchains through
//!   `afterburner::cli::compile`, which is behind the `afterburner` `bin`
//!   feature that only `gents-cli` carries. The compile step is the
//!   [`build::PluginCompiler`] seam here; the CLI's
//!   `commands::afterburner_build::{load_and_validate, compile}` is the
//!   implementation to supply.
//!
//! Registration: this module is not yet wired into `crate::pack`. Wiring it
//! is one line (`pub mod authoring;` in `crates/gents/src/pack.rs`) plus the
//! CLI delegation, tracked with the integration step; until then the files
//! here are unwired and nothing in the workspace reads them.
pub mod build;
pub mod check;
pub mod inspect;
pub mod names;
pub mod scaffold;
pub mod template;
pub mod test;

pub use build::{build_all, build_graph_plans, build_pack, build_plugin, plugin_entry, BuildReport, PluginCompiler, PluginReport, BUILDER_ID};
pub use check::{check_dir, check_files, check_readme, load_config, read_manifest, topology_block, write_topology, CheckReport};
pub use inspect::{preview_config, verify_pack_file};
pub use names::{to_snake_case, Names};
pub use scaffold::{scaffold, tool_markdown, write_plugin_source, ScaffoldInput, ScaffoldReport};
pub use template::{Kind, Template};
pub use test::{run_plugin_cases, PluginCase, PluginCases};
