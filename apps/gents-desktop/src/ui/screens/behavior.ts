import type { NodeView } from "../../hooks/fleetStore";
import { agentOf } from "@/lib/agents";

/* two-letter initials on a pastel chip; the ink stays fixed like a marker */
/* a leading article is not a name: "The Engineer" is En, not Te */
const ARTICLES = new Set(["the", "a", "an"]);

export function initials(name: string) {
  const all = name.trim().split(/\s+/);
  const words =
    all.length > 1 && ARTICLES.has(all[0]!.toLowerCase()) ? all.slice(1) : all;
  return (
    words.length > 1 ? words[0]![0]! + words[1]![0]! : words.join(" ").slice(0, 2)
  ).replace(/^(.)(.)$/, (_, a: string, b: string) => a.toUpperCase() + b.toLowerCase());
}

export function behaviorName(behaviorId: string | null, deployment: NodeView | null) {
  return agentOf(deployment, behaviorId)?.displayName ?? "Default";
}

/* The bridge's labels, as a person would say them. Files and bash come
   from the desktop's file_access_label / bash_access_label ("off",
   "read-only", "read / write", "unrestricted"); network is the tool
   selection's command_network_mode (disabled / inherit / enabled); the
   tool ceiling is init_tool_ceiling (meta-only / readonly / readwrite). */
export const fileAccess = (mode: string) =>
  ({ "read / write": "read and edit", "read-only": "read", off: "not touch" })[mode] ??
  mode;
export const bashAccess = (mode: string) =>
  ({ unrestricted: "run any", "read-only": "run read-only", off: "not run" })[mode] ??
  mode;
/* the agent's tool ceiling, or a file label, as a phrase */
export const access = (mode: string) =>
  ({
    readwrite: "read and edit files",
    readonly: "read files",
    "meta-only": "use meta tools only",
    "read / write": "read and edit files",
    "read-only": "read files",
    off: "not touch files",
  })[mode] ?? mode;
export const network = (mode: string | null | undefined) =>
  mode === "enabled"
    ? "the network"
    : mode === "inherit"
      ? "the network (inherited)"
      : "no network";
