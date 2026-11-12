use gents_protocol::document_query::{fields, Direction, Filter, Query, Selection};
use gents_protocol::views::View;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ConfigurationRow<T> {
    #[serde(rename = "_docID")]
    pub doc_id: String,
    #[serde(flatten)]
    pub document: T,
}

macro_rules! configuration_view {
    ($name:ident, $row:ty, $collection:ident, $key:ident, [$($field:ident),+ $(,)?]) => {
        #[derive(Debug, Clone)]
        pub struct $name {
            node_did: String,
            document_ids: Option<Vec<String>>,
            id: Option<String>,
            after: Option<String>,
            limit: Option<u64>,
        }

        impl $name {
            pub fn new(node_did: impl Into<String>) -> Self {
                Self {
                    node_did: node_did.into(),
                    document_ids: None,
                    id: None,
                    after: None,
                    limit: None,
                }
            }

            pub fn doc_ids(mut self, document_ids: Vec<String>) -> Self {
                self.document_ids = Some(document_ids);
                self
            }

            pub fn id(mut self, id: impl Into<String>) -> Self {
                self.id = Some(id.into());
                self
            }

            pub fn after(mut self, id: impl Into<String>) -> Self {
                self.after = Some(id.into());
                self
            }

            pub fn limit(mut self, limit: u64) -> Self {
                self.limit = Some(limit);
                self
            }

            fn selection() -> Selection<fields::$collection::Doc> {
                Selection::new()
                    .field(fields::$collection::DOC_ID)
                    $(.field(fields::$collection::$field))+
            }
        }

        impl View for $name {
            type Collection = fields::$collection::Doc;
            type Row = ConfigurationRow<$row>;

            fn query(&self, _reader_did: &str) -> Query<Self::Collection> {
                let mut query = Query::select(Self::selection())
                    .filter(Filter::eq(fields::$collection::NODE_DID, &self.node_did))
                    .order_by(fields::$collection::$key, Direction::Asc);
                if let Some(ids) = &self.document_ids {
                    query = query.filter(Filter::any_of(fields::$collection::DOC_ID, ids));
                }
                if let Some(id) = &self.id {
                    query = query.filter(Filter::eq(fields::$collection::$key, id));
                }
                if let Some(id) = &self.after {
                    query = query.filter(Filter::gt(fields::$collection::$key, id));
                }
                if let Some(limit) = self.limit {
                    query = query.limit(limit);
                }
                query
            }
        }
    };
}

configuration_view!(
    NodeList,
    crate::Node,
    node,
    NODE_DID,
    [
        NODE_DID,
        DISPLAY_NAME,
        AVATAR,
        ENABLED,
        CREATED_AT,
        CREATED_BY,
        MAX_REQUEST_HOP,
        TAGS,
    ]
);
configuration_view!(
    DefaultAgentSelectionList,
    crate::DefaultAgentSelection,
    default_agent_selection,
    NODE_DID,
    [NODE_DID, DEFAULT_AGENT_DID, UPDATED_AT,]
);
configuration_view!(
    AgentContextList,
    crate::AgentContext,
    agent_context,
    CONTEXT_ID,
    [
        CONTEXT_ID,
        NODE_DID,
        DISPLAY_NAME,
        DESCRIPTION,
        SYSTEM_PROMPT,
        TOOLS_ID,
        COMPACTION_ID,
        SKILL_IDS,
        TAGS,
    ]
);
configuration_view!(
    CompactionConfigList,
    crate::CompactionConfig,
    compaction_config,
    COMPACTION_ID,
    [
        COMPACTION_ID,
        NODE_DID,
        DISPLAY_NAME,
        STRATEGY,
        THRESHOLD,
        KEEP_RECENT_TOKENS,
        TOOL_RESULT_MAX_CHARS,
        SUMMARY_MAX_OUTPUT_TOKENS,
        SUMMARY_FILE_LIST_MAX,
        INFERENCE_PROFILE_ID,
        TAGS,
    ]
);
configuration_view!(
    SkillList,
    crate::SkillDocument,
    skill,
    SKILL_ID,
    [
        SKILL_ID,
        NODE_DID,
        NAME,
        DESCRIPTION,
        INSTRUCTIONS,
        SOURCE_DIRECTORY,
        TOOL_REFS,
        DISPLAY_NAME,
        INTERFACE_JSON,
        ENABLED,
        CREATED_AT,
        TAGS,
    ]
);
configuration_view!(
    ToolsList,
    crate::Tools,
    tools,
    TOOLS_ID,
    [
        TOOLS_ID,
        NODE_DID,
        DISPLAY_NAME,
        HOST,
        REMOTE,
        AGENTS,
        BUILT_INS,
        DATASTORE,
        INTEGRATIONS,
        SELF_CONFIG,
        TAGS,
    ]
);
configuration_view!(
    AgentTargetList,
    crate::AgentTargetDocument,
    agent_target,
    TARGET_ID,
    [
        TARGET_ID,
        NODE_DID,
        TARGET_NODE_DID,
        AGENT_DID,
        NAME,
        DESCRIPTION,
        TAGS,
    ]
);
configuration_view!(
    DatastoreToolSurfaceList,
    crate::DatastoreToolSurfaceDocument,
    datastore_tool_surface,
    SURFACE_ID,
    [
        SURFACE_ID,
        NODE_DID,
        DISPLAY_NAME,
        ENABLED,
        ENTRIES,
        CREATED_AT,
        TAGS,
    ]
);
configuration_view!(
    PageList,
    crate::PageDocument,
    page,
    PAGE_ID,
    [
        PAGE_ID,
        NODE_DID,
        DISPLAY_NAME,
        DESCRIPTION,
        MODULE,
        CSS,
        DATA,
        ENABLED,
        CREATED_AT,
        TAGS,
    ]
);
configuration_view!(
    PluginSourceList,
    crate::PluginSourceDocument,
    plugin_source,
    PLUGIN_ID,
    [
        PLUGIN_ID,
        NODE_DID,
        DESCRIPTION,
        LANGUAGE,
        SOURCE,
        INPUT_SCHEMA,
        DATA,
        TESTS,
        CREATED_AT,
        TAGS,
    ]
);

#[derive(Debug, Clone, Default)]
pub struct VisibleNodes {
    enabled_only: bool,
}

impl VisibleNodes {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn enabled_only(mut self) -> Self {
        self.enabled_only = true;
        self
    }
}

impl View for VisibleNodes {
    type Collection = fields::node::Doc;
    type Row = ConfigurationRow<crate::Node>;

    fn query(&self, _reader_did: &str) -> Query<Self::Collection> {
        let mut query =
            Query::select(NodeList::selection()).order_by(fields::node::NODE_DID, Direction::Asc);
        if self.enabled_only {
            query = query.filter(Filter::ne(fields::node::ENABLED, false));
        }
        query
    }
}

#[derive(Debug, Clone)]
pub struct ContextsUsingTools {
    pub node_did: String,
    pub tools_id: String,
}

impl View for ContextsUsingTools {
    type Collection = fields::agent_context::Doc;
    type Row = ConfigurationRow<crate::AgentContext>;

    fn query(&self, _reader_did: &str) -> Query<Self::Collection> {
        Query::select(AgentContextList::selection())
            .filter(Filter::eq(fields::agent_context::NODE_DID, &self.node_did))
            .filter(Filter::eq(fields::agent_context::TOOLS_ID, &self.tools_id))
            .order_by(fields::agent_context::CONTEXT_ID, Direction::Asc)
    }
}
