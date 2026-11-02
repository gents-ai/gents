import type { NodeView } from "../../../hooks/fleetStore";
import { packOrigin } from "./ListDetail";

/** The documents a pack installed, by kind, each with the section that lists
    them. An installed pack is named "namespace/name"; the documents it
    installed carry its name alone as their origin. */
export function packContents(deployment: NodeView, pack: string) {
  const origin = pack.slice(pack.lastIndexOf("/") + 1);
  const from = (docs: readonly { tags?: readonly string[] | null }[]) =>
    docs.filter((doc) => packOrigin(doc.tags ? [...doc.tags] : null) === origin).length;
  return [
    { one: "agent", many: "agents", section: "agents", count: from(deployment.agents) },
    { one: "skill", many: "skills", section: "skills", count: from(deployment.skills) },
    { one: "task", many: "tasks", section: "tasks", count: from(deployment.tasks) },
    {
      one: "tool set",
      many: "tool sets",
      section: "tools",
      count: from(deployment.tools),
    },
    {
      one: "profile",
      many: "profiles",
      section: "profiles",
      count: from(deployment.inferenceProfiles),
    },
    {
      one: "provider",
      many: "providers",
      section: "inference",
      count: from(deployment.inferenceBackends),
    },
    {
      one: "schedule",
      many: "schedules",
      section: "schedules",
      count: from(deployment.schedules),
    },
    {
      one: "event source",
      many: "event sources",
      section: "event-sources",
      count: from(deployment.eventSources),
    },
  ].filter((kind) => kind.count > 0);
}
