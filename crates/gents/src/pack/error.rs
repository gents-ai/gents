use gents_protocol::graphql::GraphqlError;

use super::{UX_PLUGIN_AREAS, UX_PLUGIN_PREFIX};

#[derive(Debug, thiserror::Error)]
pub enum PackError {
    #[error("pack origin requires a snake_case pack name")]
    OriginName,
    #[error("configuration document carries more than one pack origin")]
    MultipleOrigins,
    #[error("configuration document carries invalid pack origin {origin:?}")]
    InvalidOrigin { origin: String },
    #[error(
        "pack node sets default_agent_id {default:?}; a pack must not choose the default agent. Remove it from the pack: the user selects the default (\"Make default\" in the desktop, or default_agent_id in the node of their own `gents config apply` root)"
    )]
    DefaultAgentSet { default: String },

    #[error("{access:?} is not an access; use read or read_write")]
    UnknownAccess { access: String },

    #[error("ux plugin name must be snake_case: {name:?}")]
    UxPluginName { name: String },
    #[error("ux plugin {name:?} needs a description; it is what the UX Plugins panel shows")]
    UxPluginDescription { name: String },
    #[error("unsafe ux plugin entry path: {entry:?}")]
    UxPluginEntryPath { entry: String },
    #[error("ux plugin {name:?} entry must be a .js file under {UX_PLUGIN_PREFIX}, got {entry:?}")]
    UxPluginEntryFile { name: String, entry: String },
    #[error("ux plugin {name:?} names the plugin {plugin:?}, which this pack does not declare")]
    UxPluginUnknownPlugin { name: String, plugin: String },
    #[error(
        "ux plugin {name:?} is produced by a plugin, which emits its own css; drop the css field"
    )]
    UxPluginCssWithPlugin { name: String },
    #[error("ux plugin {name:?} declares both entry and plugin; exactly one produces the module")]
    UxPluginEntryAndPlugin { name: String },
    #[error("ux plugin {name:?} declares neither entry nor plugin; one must produce the module")]
    UxPluginNoModule { name: String },
    #[error("ux plugin {name:?} css must be a .css file under {UX_PLUGIN_PREFIX}, got {css:?}")]
    UxPluginCssFile { name: String, css: String },
    #[error("ux plugin {name:?} must declare at least one area it contributes to")]
    UxPluginNoAreas { name: String },
    #[error(
        "ux plugin {name:?} declares the area {area:?}, which the desktop does not have; known areas: {}",
        UX_PLUGIN_AREAS.join(", ")
    )]
    UxPluginUnknownArea { name: String, area: String },
    #[error(
        "ux plugin {name:?} declares the directive {directive:?}; a directive name is lowercase letters, digits and dashes, starting with a letter"
    )]
    UxPluginDirective { name: String, directive: String },
    #[error("ux plugin {name:?} declares directives but not the transcript.directives area")]
    UxPluginDirectivesWithoutArea { name: String },
    #[error(
        "ux plugin {name:?} declares the data surface {surface:?}; a surface id is non-empty, has no whitespace and is at most {} characters",
        super::MAX_UX_DATA_SURFACE_ID_LEN
    )]
    UxPluginDataSurface { name: String, surface: String },
    #[error("ux plugin {name:?} declares the data surface {surface:?} twice")]
    UxPluginDataSurfaceDuplicate { name: String, surface: String },
    #[error(
        "ux plugin {name:?} declares the data surface {surface:?} with no tools; name the tools its pages may run"
    )]
    UxPluginDataSurfaceNoTools { name: String, surface: String },
    #[error(
        "ux plugin {name:?} declares the tool {tool:?} on data surface {surface:?}; a tool name is lowercase letters, digits and underscores, starting with a letter, at most 64 characters"
    )]
    UxPluginToolName {
        name: String,
        tool: String,
        surface: String,
    },
    #[error("ux plugin {name:?} declares the tool {tool:?} on data surface {surface:?} twice")]
    UxPluginToolDuplicate {
        name: String,
        tool: String,
        surface: String,
    },

    #[error("plugin {plugin:?}'s TOOL.md is {len} bytes; the limit is {limit_kib} KiB")]
    ToolInstructionsTooLarge {
        plugin: String,
        len: usize,
        limit_kib: usize,
    },
    #[error("plugin {plugin:?}'s TOOL.md is not UTF-8 text")]
    ToolInstructionsNotUtf8 {
        plugin: String,
        #[source]
        source: std::string::FromUtf8Error,
    },

    #[error(
        "plugin {name:?} data.collections is empty; list the collections it may use or drop data"
    )]
    DataCollectionsEmpty { name: String },
    #[error("plugin {name:?} data.collections lists {collection:?} twice; list each once")]
    DataCollectionDuplicate { name: String, collection: String },
    #[error("plugin {name:?} data.ops has unknown op {op:?}; use {}", super::DATA_OPS.join(", "))]
    DataOpUnknown { name: String, op: String },
    #[error("plugin {name:?} data.stamped key {field:?} must be a GraphQL field name")]
    DataStampedFieldName {
        name: String,
        field: String,
        #[source]
        source: GraphqlError,
    },
    #[error(
        "plugin {name:?} data.stamped field {field:?} looks like a secret name; rename the field"
    )]
    DataStampedSecretName { name: String, field: String },
    #[error("plugin {name:?} asset_mounts class {class:?} must be snake_case")]
    AssetMountClass { name: String, class: String },
    #[error("plugin {name:?} asset_mounts lists class {class:?} twice; list each once")]
    AssetMountClassDuplicate { name: String, class: String },
    #[error("plugin {name:?} catalog.argv must be non-empty and hold non-empty strings")]
    CatalogArgv { name: String },
}
