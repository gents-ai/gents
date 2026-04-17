use std::path::Path;

use serde_json::{json, Value};

use crate::cli::args::ToolCeilingArg;
use crate::shared::StoredInitConfig;

pub(super) fn diagnose_tool_ceiling(init_config: Option<&StoredInitConfig>) -> Value {
    match init_config {
        Some(config) => {
            let tool_root = config.tool_root.as_deref();
            let ok = match config.tool_ceiling {
                ToolCeilingArg::Readonly | ToolCeilingArg::Readwrite => tool_root
                    .map(Path::new)
                    .map(|path| path.is_dir())
                    .unwrap_or(false),
                ToolCeilingArg::MetaOnly => true,
            };
            let error = if ok {
                None
            } else {
                Some(
                    "readonly/readwrite tool ceiling requires an existing tool_root directory"
                        .to_string(),
                )
            };
            json!({
                "ok": ok,
                "tool_ceiling": crate::format_tool_ceiling(config.tool_ceiling),
                "tool_root": config.tool_root,
                "error": error,
            })
        }
        None => json!({
            "ok": true,
            "error": null,
            "note": "no local init.json found; tool ceiling is unknown until `defra-agent init` runs"
        }),
    }
}
