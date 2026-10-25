use gents_protocol::graphql::GraphqlError;

use super::{
    PackKind, PLUGIN_ARTIFACT_PREFIX, SUPPORTED_PLUGIN_LANGUAGES, UX_PLUGIN_AREAS, UX_PLUGIN_PREFIX,
};

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
        source: Box<GraphqlError>,
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
    #[error("plugin service requires child_process authority")]
    ServiceChildProcess,
    #[error("plugin service needs a nonempty argv without NUL bytes")]
    ServiceArgv,
    #[error("service startup_timeout_secs must be 1..300")]
    ServiceStartupTimeout,
    #[error("service is a reserved host-filled plugin input")]
    ServiceReservedInput,
    #[error(transparent)]
    Authority(#[from] crate::plugin::authority::AuthorityError),
    #[error("plugin name must be snake_case: {name:?}")]
    PluginName { name: String },
    #[error("plugin {name:?} needs a description; it is what the model is shown")]
    PluginDescription { name: String },
    #[error("plugin {name:?} artifact must be a compiled .afb path, got {artifact:?}")]
    PluginArtifactExtension { name: String, artifact: String },
    #[error("unsafe plugin artifact path: {artifact:?}")]
    PluginArtifactPath { artifact: String },
    #[error("plugin {name:?} artifact must live under {PLUGIN_ARTIFACT_PREFIX}, got {artifact:?}")]
    PluginArtifactPrefix { name: String, artifact: String },
    #[error(
        "plugin {name:?} declares language {language:?}, which is not one gents pack build knows how to compile; supported languages: {}",
        SUPPORTED_PLUGIN_LANGUAGES.join(", ")
    )]
    PluginLanguage { name: String, language: String },
    #[error("unsafe plugin source path: {path:?}")]
    PluginSourcePath { path: String },
    #[error("plugin {name:?} instructions must be plugins/{name}/TOOL.md, got {instructions:?}")]
    PluginInstructions { name: String, instructions: String },
    #[error("plugin {name:?} needs an object input_schema")]
    PluginInputSchema { name: String },
    #[error("plugin {name:?} asks to listen on a port; a pack's plugins are called, not served")]
    PluginListen { name: String },
    #[error("plugin {name:?} declares a network grant the host cannot serve: {why}")]
    PluginNetworkUnservable { name: String, why: String },
    #[error("plugin {name:?} bind_dir.input_field must not be blank")]
    BindDirInputFieldBlank { name: String },
    #[error("plugin {name:?} bind_dir needs a description; it is what an operator is shown")]
    BindDirDescription { name: String },
    #[error(
        "plugin {name:?} bind_dir.input_field {input_field:?} must be a string property of input_schema, or of at least one oneOf branch, since call_bound always injects a string"
    )]
    BindDirInputFieldType { name: String, input_field: String },
    #[error(
        "plugin {name:?} bind_dir.original_field {original:?} must be a string property of input_schema other than input_field"
    )]
    BindDirOriginalField { name: String, original: String },
    #[error(
        "plugin {name:?} declares bind_dir.write_fields but bind_dir.access is read; a call that sets one would write, so declare access read_write"
    )]
    BindDirWriteFieldsReadOnly { name: String },
    #[error(
        "plugin {name:?} bind_dir.write_fields names {field:?}, which must be a property of input_schema other than the bound path fields"
    )]
    BindDirWriteField { name: String, field: String },
    #[error(
        "plugin {name:?} declares both bind_dir and a manifold fs grant; a directory bound per call and a standing filesystem grant cannot be expressed together"
    )]
    BindDirWithFsGrant { name: String },
    #[error("plugin {name:?} declares memory_mib {value}, but it {reason}")]
    MemoryMib {
        name: String,
        value: u32,
        reason: String,
    },
    #[error("plugin {name:?} declares wall_clock_secs {value}, but it {reason}")]
    WallClockSecs {
        name: String,
        value: u32,
        reason: String,
    },
    #[error("plugin {name:?} declares max_output_mib {value}, but it {reason}")]
    MaxOutputMib {
        name: String,
        value: u32,
        reason: String,
    },

    #[error("invalid pack identity/version")]
    ManifestIdentity,
    #[error("unsupported pack manifest version")]
    UnsupportedManifestVersion,
    #[error(
        "only document packs support package dependencies; nested dependencies are unsupported"
    )]
    DependenciesKind,
    #[error(
        "dependency {dependency:?} must be a coordinate (name or ns/name), not a pinned version"
    )]
    DependencyPinned { dependency: String },
    #[error("dependency {dependency:?} is not a valid pack coordinate")]
    DependencyCoordinate { dependency: String },
    #[error("pack needs description and authors")]
    PackDescriptionAuthors,
    #[error("pack name must be snake_case")]
    PackName,
    #[error("pack namespace must be snake_case: {namespace:?}")]
    PackNamespace { namespace: String },
    #[error("unsafe/private pack asset: {path}")]
    AssetUnsafe { path: String },
    #[error("non-canonical pack asset spelling: {path}")]
    AssetNonCanonical { path: String },
    #[error("duplicate asset {path}")]
    AssetDuplicate { path: String },
    #[error("pack must declare README.md")]
    ReadmeMissing,
    #[error("pack declares the plugin {name:?} twice")]
    PluginDuplicate { name: String },
    #[error("pack declares the plugin {name:?} but not its artifact {artifact:?} as an asset")]
    PluginArtifactUndeclared { name: String, artifact: String },
    #[error(
        "pack declares the plugin {name:?} but not its instructions {instructions:?} as an asset"
    )]
    PluginInstructionsUndeclared { name: String, instructions: String },
    #[error("a plugins pack must declare at least one plugin")]
    PluginsPackEmpty,
    #[error(
        "ux plugins ship in an assets or plugins pack; a {kind:?} pack is never listed for them"
    )]
    UxKindNotListed { kind: PackKind },
    #[error("pack declares the ux plugin {name:?} twice")]
    UxDuplicate { name: String },
    #[error("pack declares the ux plugin {name:?} but not its {what} {path:?} as an asset")]
    UxAssetUndeclared {
        name: String,
        what: &'static str,
        path: String,
    },
    #[error("inference slot name must be snake_case: {name:?}")]
    SlotName { name: String },
    #[error("pack declares inference slot {name:?} twice")]
    SlotDuplicate { name: String },
    #[error("inference slot {name:?} needs a description")]
    SlotDescription { name: String },
    #[error(
        "inference slot {name:?} must name at least one agent, unless it is optional, and an optional slot names none"
    )]
    SlotAgentsMismatch { name: String },
    #[error("inference slot {name:?} repeats or has a blank agent")]
    SlotAgentRepeated { name: String },
    #[error("agent {agent:?} belongs to more than one inference slot")]
    AgentInMultipleSlots { agent: String },
    #[error("asset and plugins packs can declare only optional inference slots")]
    SlotsNotOptional,
    #[error("plugin {name:?} names the model slot {slot:?}, which the pack does not declare")]
    PluginModelSlotUndeclared { name: String, slot: String },
    #[error("plugin {name:?} model slot {slot:?} must be optional and have no agents")]
    PluginModelSlotNotOptional { name: String, slot: String },
    #[error("optional inference slot {name:?} is used by no plugin")]
    OptionalSlotUnused { name: String },
    #[error("document/graph pack requires a config asset")]
    ConfigAssetRequired,
    #[error("config asset must be declared")]
    ConfigAssetUndeclared,
    #[error("asset-only pack cannot declare configuration")]
    AssetPackConfig,
    #[error("asset-only pack cannot declare installed schemas")]
    AssetPackSchemas,
    #[error("schema asset must be declared: {schema}")]
    SchemaUndeclared { schema: String },
    #[error("duplicate schema asset: {schema}")]
    SchemaDuplicate { schema: String },
    #[error("graph pack compiler version does not match runtime")]
    GraphCompilerVersion,
    #[error("non-graph pack cannot select a graph compiler")]
    NonGraphCompiler,
    #[error("pack references missing asset {path:?}")]
    MissingAsset { path: String },
}
