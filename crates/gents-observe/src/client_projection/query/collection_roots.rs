use std::collections::BTreeSet;

use super::snapshot_loaders::selected_node_root;
use super::*;
use gents_documents::views::agents::AgentList;
use gents_documents::views::config::{
    AgentContextList, AgentTargetList, CompactionConfigList, DatastoreToolSurfaceList,
    DefaultAgentSelectionList, NodeList, SkillList, ToolsList,
};
use gents_documents::views::tasks::{EventSourceList, ScheduleList, TaskList};
use gents_inference::views::backends::{BackendList, ObservationList};
use gents_inference::views::profiles::ProfileList;
use gents_inference::views::settings::{ExecutionList, SamplingList};
use gents_protocol::views::View;
use gents_store::config_client::ConfigAccess;
use gents_store::views::runtime::{NodeReadinessList, NodeRuntimeList};

/// One self-contained root of the node snapshot: the view the snapshot reads
/// for it, the rows it fills and how a reload of it replaces them. The full
/// snapshot and a collection reload both read the root through this one
/// definition, so a collection reload shows exactly the rows a full load would.
pub(super) trait Root {
    type View<'n>: View;

    fn view(node_did: &str) -> Self::View<'_>;

    fn store<'n>(
        rows: Vec<<Self::View<'n> as View>::Row>,
        into: &mut ClientStoreRows,
    ) -> Result<()>;

    fn take(from: &mut ClientStoreRows, into: &mut ClientStoreRows);
}

macro_rules! collection_roots {
    ($lt:lifetime; $($root:ident($collection:pat) => $field:ident: $view:ty =
        |$node:ident| $make:expr, |$row:ident| $map:expr;)+) => {
        /// A configuration collection whose node snapshot root is one
        /// self-contained view (`ObserverResync.Change.collection`): its rows
        /// depend on no other root and no other root's follow-up read depends
        /// on them, so a change to it re-reads that root alone.
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum CollectionRoot {
            $($root,)+
        }

        pub(super) mod root {
            use super::*;

            $(
                pub(in crate::client_projection::query) struct $root;

                impl Root for $root {
                    type View<$lt> = $view;

                    fn view($node: &str) -> Self::View<'_> {
                        $make
                    }

                    fn store<$lt>(
                        rows: Vec<<Self::View<$lt> as View>::Row>,
                        into: &mut ClientStoreRows,
                    ) -> Result<()> {
                        into.$field = rows.into_iter().map(|$row| $map).collect::<Result<_>>()?;
                        Ok(())
                    }

                    fn take(from: &mut ClientStoreRows, into: &mut ClientStoreRows) {
                        into.$field = std::mem::take(&mut from.$field);
                    }
                }
            )+
        }

        impl CollectionRoot {
            /// The root a change to `collection` re-reads alone, if it has one.
            pub fn of(collection: &str) -> Option<Self> {
                match collection {
                    $($collection => Some(Self::$root),)+
                    _ => None,
                }
            }

            async fn read(
                self,
                access: &ConfigAccess,
                node_did: &str,
                into: &mut ClientStoreRows,
            ) -> std::result::Result<(), SnapshotReadError> {
                match self {
                    $(Self::$root => read_root::<root::$root>(access, node_did, into).await,)+
                }
            }

            fn take(self, from: &mut ClientStoreRows, into: &mut ClientStoreRows) {
                match self {
                    $(Self::$root => <root::$root as Root>::take(from, into),)+
                }
            }
        }
    };
}

collection_roots! { 'n;
    DefaultAgentSelections(DEFAULT_AGENT_SELECTION_NAME) => default_agent_selections:
        DefaultAgentSelectionList = |node| DefaultAgentSelectionList::new(node),
        |row| Ok(row.document);
    Agents(AGENT_NAME) => agents: AgentList = |node| AgentList::new(node), |row| Ok(row.agent);
    Runtimes(NODE_RUNTIME_NAME) => runtimes: NodeRuntimeList<'n> =
        |node| NodeRuntimeList::new(node), |row| Ok(row);
    NodeReadiness(NODE_READINESS_NAME) => node_readiness: NodeReadinessList<'n> =
        |node| NodeReadinessList::new(node), |row| Ok(row);
    MailboxItems(MAILBOX_ITEM_NAME) => mailbox_items: gents_automation::views::MailboxRows =
        |node| gents_automation::views::MailboxRows::new(node).open().for_reader(), |row| Ok(row);
    Goals(GOAL_NAME) => goals: gents_automation::views::GoalRows =
        |node| gents_automation::views::GoalRows::new(node), |row| Ok(row);
    Tasks(TASK_NAME) => tasks: TaskList = |node| TaskList::new(node), |row| Ok(row.task);
    Schedules(SCHEDULE_NAME) => schedules: ScheduleList =
        |node| ScheduleList::new(node), |row| Ok(row.schedule);
    Skills(SKILL_NAME) => skills: SkillList = |node| SkillList::new(node), |row| Ok(row.document);
    Tools(TOOLS_NAME) => tools: ToolsList = |node| ToolsList::new(node), |row| Ok(row.document);
    Contexts("AgentContext") => contexts: AgentContextList =
        |node| AgentContextList::new(node), |row| Ok(row.document);
    Compactions("CompactionConfig") => compactions: CompactionConfigList =
        |node| CompactionConfigList::new(node), |row| Ok(row.document);
    InferenceBackends(INFERENCE_BACKEND_NAME) => inference_backends: BackendList =
        |node| BackendList::new(node),
        |row| row.metadata.into_backend().map_err(anyhow::Error::from);
    BackendObservations(INFERENCE_BACKEND_OBSERVATION_NAME) => backend_observations:
        ObservationList = |node| ObservationList::new(node), |row| Ok(row.observation);
    InferenceProfiles(INFERENCE_PROFILE_NAME) => inference_profiles: ProfileList =
        |node| ProfileList::new(node), |row| Ok(row.profile);
    InferenceSampling("InferenceSampling") => inference_sampling: SamplingList =
        |node| SamplingList::new(node), |row| Ok(row.settings);
    InferenceExecution("InferenceExecution") => inference_execution: ExecutionList =
        |node| ExecutionList::new(node), |row| Ok(row.settings);
    ToolServiceRegistries(TOOL_SERVICE_REGISTRY_NAME) => tool_service_registries:
        gents_tools::mcp::views::Services<'n> = |node| gents_tools::mcp::views::Services {
            node_did: node,
            service_id: None,
            enabled_only: false,
        },
        |row| Ok(row);
    EventSources("EventSource") => event_sources: EventSourceList =
        |node| EventSourceList::new(node), |row| Ok(row.event_source);
    AgentTargets(AGENT_TARGET_NAME) => agent_targets: AgentTargetList =
        |node| AgentTargetList::new(node), |row| Ok(row.document);
    DatastoreToolSurfaces("DatastoreToolSurface") => datastore_tool_surfaces:
        DatastoreToolSurfaceList = |node| DatastoreToolSurfaceList::new(node),
        |row| Ok(row.document);
}

/// Reads one root with the Node root beside it: like the full snapshot, a
/// collection reload of a Node this client cannot observe is a refusal, not
/// an empty collection.
async fn read_root<R>(
    access: &ConfigAccess,
    node_did: &str,
    into: &mut ClientStoreRows,
) -> std::result::Result<(), SnapshotReadError>
where
    R: Root,
{
    let (nodes, rows) = access
        .read_views(&(NodeList::new(node_did), R::view(node_did)))
        .await
        .map_err(anyhow::Error::from)?;
    selected_node_root(!nodes.is_empty())?;
    R::store(rows, into)?;
    Ok(())
}

/// The rows of `roots` alone, as the node snapshot reads them
/// (`ObserverResync.startCollections`).
pub async fn load_collection_roots_on(
    access: &ConfigAccess,
    node_did: &str,
    roots: &BTreeSet<CollectionRoot>,
) -> std::result::Result<ClientStore, SnapshotReadError> {
    let mut rows = ClientStoreRows::default();
    for root in roots {
        root.read(access, node_did, &mut rows).await?;
    }
    Ok(ClientStore::from_rows(rows))
}

/// `current` with the rows of `roots` replaced by `incoming`'s, every other
/// row kept.
pub fn replace_collection_roots(
    current: &ClientStore,
    roots: &BTreeSet<CollectionRoot>,
    incoming: ClientStore,
) -> ClientStore {
    let mut rows = current.to_rows();
    let mut incoming = incoming.to_rows();
    for root in roots {
        root.take(&mut incoming, &mut rows);
    }
    ClientStore::from_rows(rows)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn replacing_a_root_keeps_every_other_row() {
        let observation = |backend: &str| {
            serde_json::from_value::<gents_documents::InferenceBackendObservation>(
                serde_json::json!({"backend_id": backend, "probe_status": null, "last_probe": null}),
            )
            .unwrap()
        };
        let session = serde_json::from_value(serde_json::json!({
            "node_did": "did:node", "session_id": "session", "agent_did": "agent",
            "created_at": "2026-01-01T00:00:00Z",
        }))
        .unwrap();
        let current = ClientStore::from_rows(ClientStoreRows {
            sessions: vec![session],
            backend_observations: vec![observation("old"), observation("removed")],
            ..Default::default()
        });
        let incoming = ClientStore::from_rows(ClientStoreRows {
            backend_observations: vec![observation("old")],
            ..Default::default()
        });
        let roots = BTreeSet::from([CollectionRoot::BackendObservations]);
        let replaced = replace_collection_roots(&current, &roots, incoming);
        assert_eq!(replaced.sessions, current.sessions);
        assert_eq!(replaced.backend_observations, vec![observation("old")]);
    }

    #[test]
    fn only_self_contained_roots_reload_alone() {
        let alone: Vec<_> = OBSERVED_COLLECTIONS
            .iter()
            .filter(|collection| CollectionRoot::of(collection).is_some())
            .collect();
        assert!(alone.contains(&&INFERENCE_BACKEND_OBSERVATION_NAME));
        for collection in [
            NODE_NAME,
            AGENT_REQUEST_NAME,
            AGENT_SESSION_NAME,
            TRIGGER_NAME,
            AGENT_MESSAGE_NAME,
            AGENT_OUTPUT_SEGMENT_NAME,
            AGENT_TOOL_CALL_NAME,
            COMPACTION_ENTRY_NAME,
        ] {
            assert_eq!(CollectionRoot::of(collection), None, "{collection}");
        }
    }
}
