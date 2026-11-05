import type { NodeView } from "../../../hooks/fleetStore";
import { packOrigin } from "./ListDetail";

/** One document a pack installed: its id in its section, and its name. */
export type PackItem = { id: string; name: string };

/** The documents a pack installed, by kind, each with the section that lists
    them. An installed pack is named "namespace/name"; the documents it
    installed carry its name alone as their origin. */
export function packContents(deployment: NodeView, pack: string) {
  const origin = pack.slice(pack.lastIndexOf("/") + 1);
  const from = <T extends { tags?: readonly string[] | null }>(
    docs: readonly T[],
    item: (doc: T) => PackItem,
  ): PackItem[] =>
    docs
      .filter((doc) => packOrigin(doc.tags ? [...doc.tags] : null) === origin)
      .map(item);
  return [
    {
      one: "agent",
      many: "agents",
      section: "agents",
      items: from(deployment.agents, (a) => ({ id: a.agentId, name: a.displayName })),
    },
    {
      one: "skill",
      many: "skills",
      section: "skills",
      items: from(deployment.skills, (s) => ({
        id: s.skillId,
        name: s.displayName ?? s.name ?? s.skillId,
      })),
    },
    {
      one: "task",
      many: "tasks",
      section: "tasks",
      items: from(deployment.tasks, (t) => ({
        id: t.taskId,
        name: t.name ?? t.taskId,
      })),
    },
    {
      one: "tool set",
      many: "tool sets",
      section: "tools",
      items: from(deployment.tools, (t) => ({
        id: t.tools_id,
        name: t.display_name ?? t.tools_id,
      })),
    },
    {
      one: "profile",
      many: "profiles",
      section: "profiles",
      items: from(deployment.inferenceProfiles, (p) => ({
        id: p.profile_id,
        name: p.display_name ?? p.profile_id,
      })),
    },
    {
      one: "provider",
      many: "providers",
      section: "inference",
      items: from(deployment.inferenceBackends, (b) => ({
        id: b.backendId,
        name: b.name ?? b.backendId,
      })),
    },
    {
      one: "schedule",
      many: "schedules",
      section: "schedules",
      items: from(deployment.schedules, (s) => ({
        id: s.schedule_id,
        name: s.display_name ?? s.schedule_id,
      })),
    },
    {
      one: "event source",
      many: "event sources",
      section: "event-sources",
      items: from(deployment.eventSources, (e) => ({
        id: e.event_source_id,
        name: e.display_name ?? e.event_source_id,
      })),
    },
  ]
    .map((kind) => ({ ...kind, count: kind.items.length }))
    .filter((kind) => kind.count > 0);
}
