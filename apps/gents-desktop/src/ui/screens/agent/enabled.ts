import type { ConfigComponentPatch } from "@source-inc/gents-desktop-client";
import type { ShellActions } from "@/../hooks/shellActions";

/** A collection whose documents carry `enabled` and that a patch can name. */
type Switchable = Extract<
  ConfigComponentPatch["collection"],
  "AgentBehavior" | "InferenceBackend" | "Task" | "Schedule" | "Trigger" | "EventSource"
>;

/**
 * Turns a document on or off with a patch of `enabled` alone. Saving the
 * whole document rebuilt from its view would rewrite every other field: a
 * concurrent edit would be lost, and a document missing a field the view
 * fills in (a behavior with no profile yet) would be saved with it.
 */
export function setEnabled(
  changeConfig: ShellActions["changeConfig"],
  agentDid: string,
  collection: Switchable,
  id: string,
  enabled: boolean,
) {
  return changeConfig(
    "patchConfigComponents",
    {
      agentDid,
      patches: [{ collection, id, changes: { enabled } } as ConfigComponentPatch],
    },
    `turn it ${enabled ? "on" : "off"}`,
  );
}
