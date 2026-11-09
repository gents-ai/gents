pub mod build;
pub mod check;
pub mod edit;
pub mod inspect;
pub mod names;
pub mod scaffold;
pub mod template;
pub mod test;

use crate::tools::native::ToolContext;

pub use build::{
    build_all, build_pack, build_plugin, ensure_no_compile_step, pack_built, plugin_entry,
    plugins_with_source, BuildReport, PluginCompiler, PluginReport, BUILDER_ID,
};
pub use check::{
    check_dir, check_files, check_readme, load_config, read_manifest, self_write_warnings,
    CheckReport,
};
pub use edit::{PackAddition, PackEdit, PackPart};
pub use inspect::{preview_config, verify_pack_file};
pub use names::{pack_name, to_snake_case, Names};
pub use scaffold::{scaffold, tool_markdown, write_plugin_source, ScaffoldInput, ScaffoldReport};
pub use template::{Kind, Template};
pub use test::{run_plugin_cases, PluginCase, PluginCases};
