//! The templates `gents pack new`/`init` start a pack from, and the kind
//! each one covers.
//!
//! Canonical with the CLI's `PackTemplate`/`PackKindArg` (`crates/gents-cli/
//! src/cli/args.rs`); a runtime caller names the template directly instead of
//! through clap. [`Template::for_kind`] preserves the CLI's mapping from a
//! `--kind` to a default template.

/// What `gents pack new` and `init` start a pack from.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Template {
    /// A documents pack: one agent, context, tools and task.
    Minimal,
    /// The minimal pack plus a job schema, an event source and a trigger.
    Automation,
    /// A graph pack: one stage from a job entry to one result.
    Graph,
    /// A plugins pack with one plugin, ready to build.
    PluginTool,
    /// A pack of files with no configuration.
    Assets,
}

/// The pack kind a manifest declares, as a scaffold input (the manifest's
/// own `PackKind` lives in `crate::pack`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Documents,
    Graph,
    Assets,
    Plugins,
}

impl Template {
    /// The template a `--kind` picks when `--template` is absent: the CLI
    /// scaffold's exact mapping.
    pub fn for_kind(kind: Option<Kind>) -> Template {
        match kind {
            Some(Kind::Graph) => Template::Graph,
            Some(Kind::Plugins) => Template::PluginTool,
            Some(Kind::Assets) => Template::Assets,
            Some(Kind::Documents) | None => Template::Minimal,
        }
    }

    /// The manifest `"kind"` value the template writes.
    pub fn manifest_kind(self) -> &'static str {
        match self {
            Template::Graph => "graph",
            Template::Minimal | Template::Automation => "documents",
            Template::PluginTool => "plugins",
            Template::Assets => "assets",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Source-derived: every mapping in the CLI scaffold's `match args.kind`
    /// (`crates/gents-cli/src/commands/pack/scaffold.rs`), including the
    /// `None` default.
    #[test]
    fn kind_to_template_matches_the_cli_mapping() {
        assert_eq!(Template::for_kind(None), Template::Minimal);
        assert_eq!(Template::for_kind(Some(Kind::Documents)), Template::Minimal);
        assert_eq!(Template::for_kind(Some(Kind::Graph)), Template::Graph);
        assert_eq!(
            Template::for_kind(Some(Kind::Plugins)),
            Template::PluginTool
        );
        assert_eq!(Template::for_kind(Some(Kind::Assets)), Template::Assets);
    }

    #[test]
    fn manifest_kinds_match_the_scaffold_writes() {
        assert_eq!(Template::Graph.manifest_kind(), "graph");
        assert_eq!(Template::Minimal.manifest_kind(), "documents");
        assert_eq!(Template::Automation.manifest_kind(), "documents");
        assert_eq!(Template::PluginTool.manifest_kind(), "plugins");
        assert_eq!(Template::Assets.manifest_kind(), "assets");
    }
}
