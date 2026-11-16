#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Template {
    Minimal,
    Automation,
    PluginTool,
    Assets,
    /// A documents pack shipping one workflow: a Rust job plugin, the worker
    /// persona it spawns and the caller Tools template `pack wire` copies.
    Workflow,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Documents,
    Assets,
    Plugins,
}

impl Template {
    pub fn for_kind(kind: Option<Kind>) -> Template {
        match kind {
            Some(Kind::Plugins) => Template::PluginTool,
            Some(Kind::Assets) => Template::Assets,
            Some(Kind::Documents) | None => Template::Minimal,
        }
    }

    pub fn manifest_kind(self) -> &'static str {
        match self {
            Template::Minimal | Template::Automation | Template::Workflow => "documents",
            Template::PluginTool => "plugins",
            Template::Assets => "assets",
        }
    }
}
