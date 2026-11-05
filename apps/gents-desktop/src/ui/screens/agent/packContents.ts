import type { NodeView } from "../../../hooks/fleetStore";
import { packOrigin } from "./ListDetail";

/** One document a pack installed: its id in its section, its name, and
    what more a review says about it. */
export type PackItem = {
  id: string;
  name: string;
  detail?: string;
  /** the collection a review reports it in */
  collection?: string;
};

/** The kinds of document a pack adds that people look for, in the words they
    use, each with the database collection that holds it and the section of
    the node's configuration that lists it. */
export const PACK_KINDS = [
  { collection: "Agent", one: "agent", many: "agents", section: "agents" },
  { collection: "Skill", one: "skill", many: "skills", section: "skills" },
  { collection: "Task", one: "task", many: "tasks", section: "tasks" },
  { collection: "Tools", one: "tool set", many: "tool sets", section: "tools" },
  {
    collection: "InferenceProfile",
    one: "profile",
    many: "profiles",
    section: "profiles",
  },
  {
    collection: "InferenceBackend",
    one: "provider",
    many: "providers",
    section: "inference",
  },
  { collection: "Schedule", one: "schedule", many: "schedules", section: "schedules" },
  {
    collection: "EventSource",
    one: "event source",
    many: "event sources",
    section: "event-sources",
  },
] as const;

/** Settings a pack adds that belong to something else it adds: an agent's
    context and the compaction that context uses, and the targets a tool set
    calls. They're listed under what they belong to, not as kinds of their
    own. */
export const SETTING_KINDS = [
  {
    collection: "AgentContext",
    one: "agent context",
    many: "agent contexts",
    section: "contexts",
  },
  {
    collection: "CompactionConfig",
    one: "compaction config",
    many: "compaction configs",
    section: "compactions",
  },
  {
    collection: "AgentTarget",
    one: "agent target",
    many: "agent targets",
    section: "agent-targets",
  },
] as const;

/** A kind of document in words: its own word, or else its collection's
    name, spaced. */
export function kindOf(collection: string) {
  const known = [...PACK_KINDS, ...SETTING_KINDS].find(
    (k) => k.collection === collection,
  )?.one;
  if (known) return known;
  if (collection === "DatastoreToolSurface") return "data tool set";
  return collection.replace(/([a-z])([A-Z])/g, "$1 $2").toLowerCase();
}

type Kind = (typeof PACK_KINDS)[number];
const kind = (collection: Kind["collection"]) => {
  const { one, many, section } = PACK_KINDS.find((k) => k.collection === collection)!;
  return { one, many, section };
};

type Tagged = { tags?: readonly string[] | null };

/** The node's documents of each kind a pack adds, each with its tags. */
export function nodeDocuments(deployment: NodeView) {
  const of = <T extends Tagged>(docs: readonly T[], item: (doc: T) => PackItem) =>
    docs.map((doc) => ({ ...item(doc), tags: doc.tags ? [...doc.tags] : null }));
  return [
    {
      ...kind("Agent"),
      collection: "Agent",
      items: of(deployment.agents, (a) => ({ id: a.agentId, name: a.displayName })),
    },
    {
      ...kind("Skill"),
      collection: "Skill",
      items: of(deployment.skills, (s) => ({
        id: s.skillId,
        name: s.displayName ?? s.name ?? s.skillId,
      })),
    },
    {
      ...kind("Task"),
      collection: "Task",
      items: of(deployment.tasks, (t) => ({
        id: t.taskId,
        name: t.name ?? t.taskId,
      })),
    },
    {
      ...kind("Tools"),
      collection: "Tools",
      items: of(deployment.tools, (t) => ({
        id: t.tools_id,
        name: t.display_name ?? t.tools_id,
      })),
    },
    {
      ...kind("InferenceProfile"),
      collection: "InferenceProfile",
      items: of(deployment.inferenceProfiles, (p) => ({
        id: p.profile_id,
        name: p.display_name ?? p.profile_id,
      })),
    },
    {
      ...kind("InferenceBackend"),
      collection: "InferenceBackend",
      items: of(deployment.inferenceBackends, (b) => ({
        id: b.backendId,
        name: b.name ?? b.backendId,
      })),
    },
    {
      ...kind("Schedule"),
      collection: "Schedule",
      items: of(deployment.schedules, (s) => ({
        id: s.schedule_id,
        name: s.display_name ?? s.schedule_id,
      })),
    },
    {
      ...kind("EventSource"),
      collection: "EventSource",
      items: of(deployment.eventSources, (e) => ({
        id: e.event_source_id,
        name: e.display_name ?? e.event_source_id,
      })),
    },
  ];
}

/** The documents a pack installed, by kind, each with the section that lists
    them. An installed pack is named "namespace/name"; the documents it
    installed carry its name alone as their origin. */
export function packContents(deployment: NodeView, pack: string) {
  const origin = pack.slice(pack.lastIndexOf("/") + 1);
  return nodeDocuments(deployment)
    .map(({ one, many, section, items }) => {
      const mine = items
        .filter((doc) => packOrigin(doc.tags) === origin)
        .map(({ id, name }) => ({ id, name }));
      return { one, many, section, items: mine, count: mine.length };
    })
    .filter((kind) => kind.count > 0);
}

/** The settings a pack installed that belong to something else it
    installed, each with its collection. */
export function packSettings(deployment: NodeView, pack: string): PackItem[] {
  const origin = pack.slice(pack.lastIndexOf("/") + 1);
  const mine = <T extends Tagged>(docs: readonly T[] | undefined) =>
    (docs ?? []).filter(
      (doc) => packOrigin(doc.tags ? [...doc.tags] : null) === origin,
    );
  return [
    ...mine(deployment.contexts).map((c) => ({
      collection: "AgentContext",
      id: c.context_id,
      name: c.display_name ?? c.context_id,
    })),
    ...mine(deployment.compactions).map((c) => ({
      collection: "CompactionConfig",
      id: c.compaction_id,
      name: c.display_name ?? c.compaction_id,
    })),
    ...mine(deployment.agentTargets).map((t) => ({
      collection: "AgentTarget",
      id: t.target_id,
      name: t.name || t.target_id,
    })),
  ];
}
