/* Removing a pack whose plugins an agent's tools still select. The runtime
   refuses to retire a plugin a Tools document selects, so Remove says who
   uses them, takes them out of each agent's tools, then removes the pack. */
import { toast } from "sonner";
import type { PackWireSources } from "@source-inc/gents-desktop-client";
import type { NodeView } from "../../../hooks/fleetStore";
import { agentName } from "../behavior";
import { toolsUsers } from "../agent/ToolsSummary";
import { packOrigin } from "../agent/ListDetail";
import { ErrorDetail } from "./PackCard";
import { shortName } from "./model";

/** One agent whose tools select some of a pack's plugins. */
export type PackUse = {
  agentDid: string;
  name: string;
  /** the plugins it selects, `<namespace>/<name>`, in its tools' order */
  plugins: string[];
};

/** The agents whose tools select any of `pack`'s plugins, as far as `node`
    shows them: each agent's context names its tools, which list the plugins
    it is offered. A tool set no agent uses selects nothing that matters, and
    one the pack installed goes with it, so neither is counted: taking the
    plugins out of the pack's own tool set would only edit what is about to
    be deleted, and the runtime then refuses the removal as an edit. */
export function packUses(
  node: NodeView,
  sources: PackWireSources,
  pack: string,
): PackUse[] {
  const shipped = new Set(
    sources.plugins.filter((p) => p.pack === pack).map((p) => p.plugin),
  );
  if (shipped.size === 0) return [];
  const uses = new Map<string, PackUse>();
  for (const tools of node.tools) {
    if (packOrigin(tools.tags ?? null) === pack) continue;
    const selected = (tools.integrations?.plugins ?? [])
      .map((ref) => ref.plugin)
      .filter((plugin) => shipped.has(plugin));
    if (selected.length === 0) continue;
    for (const agent of toolsUsers(node, tools.tools_id)) {
      const use = uses.get(agent.agentDid) ?? {
        agentDid: agent.agentDid,
        name: agent.displayName,
        plugins: [],
      };
      for (const plugin of selected)
        if (!use.plugins.includes(plugin)) use.plugins.push(plugin);
      uses.set(agent.agentDid, use);
    }
  }
  return [...uses.values()];
}

/** a plugin's tool, as the model is offered it: its name */
const toolName = (plugin: string) => shortName(plugin);

/** What Remove asks before it removes `pack`, naming the agents that use
    its tools and saying they come out of their tools first. */
export function removalQuestion(pack: string, uses: readonly PackUse[]): string {
  const question = `Remove ${pack} and everything it installed?`;
  if (uses.length === 0) return question;
  const name = shortName(pack);
  const who = uses.map(
    ({ name: agent, plugins }) =>
      `${agent} uses ${plugins.length} ${plugins.length === 1 ? "tool" : "tools"} from ${name}: ${plugins.map(toolName).join(", ")}.`,
  );
  const whose = uses.length === 1 ? "its" : "their";
  return `${question}\n\n${who.join(" ")} Removing ${name} takes them out of ${whose} tools first.`;
}

/* the runtime's refusal to retire a plugin a Tools document still selects */
const STILL_SELECTED = /still selects it through Tools/;
const SELECTING_AGENT = /agent "([^"]+)"/g;

/** The plain words for a removal the runtime refused because an agent's
    tools still select the pack's plugins, naming the agents as `node` does;
    null for any other failure. */
export function refusedRemoval(
  pack: string,
  error: string,
  node: NodeView | null,
): string | null {
  if (!STILL_SELECTED.test(error)) return null;
  const agents = [
    ...new Set([...error.matchAll(SELECTING_AGENT)].map((m) => m[1]!)),
  ].map((did) => agentName(did, node));
  const name = shortName(pack);
  const [who, whose] =
    agents.length === 0
      ? ["an agent still uses", "that agent's"]
      : agents.length === 1
        ? [`${agents[0]} still uses`, `${agents[0]}'s`]
        : [`${joined(agents)} still use`, "their"];
  return `Couldn't remove ${name}: ${who} tools from it. Take them out of ${whose} tools, then remove ${name} again.`;
}

const joined = (names: readonly string[]) =>
  names.length <= 2
    ? names.join(" and ")
    : `${names.slice(0, -1).join(", ")} and ${names[names.length - 1]}`;

/** a failure in plain words, with the runtime's text under Details */
export function failedWithDetails(message: string, error: string) {
  toast.error(message, {
    description: <ErrorDetail error={error} />,
    duration: Infinity,
  });
}

/* the runtime's refusal to remove a pack whose documents were edited since
   it was installed, naming them as `Collection/id` */
const EDITED = /documents were edited since it was installed: ([^.]+)\./;

/** The documents of `pack` edited since it was installed, as the runtime's
    refusal names them, or null when the refusal is about something else. */
export function editedDocuments(error: string): string[] | null {
  const named = EDITED.exec(error)?.[1];
  return named ? named.split(/,\s*/).filter(Boolean) : null;
}

/** What Remove asks when some of the pack's own documents were changed
    since it was installed: removing deletes them with the rest. */
export function editedQuestion(pack: string, documents: readonly string[]): string {
  const name = shortName(pack);
  const which = documents.join(", ");
  return documents.length === 1
    ? `${which} was changed since ${name} was installed. Removing ${name} deletes it with the rest of the pack.`
    : `${documents.length} of ${name}'s documents were changed since it was installed: ${which}. Removing ${name} deletes them with the rest of the pack.`;
}
