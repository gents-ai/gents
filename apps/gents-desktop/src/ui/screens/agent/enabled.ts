import type { ConfigComponentPatch } from "@source-inc/gents-desktop-client";
import type { ShellActions } from "@/../hooks/shellActions";

/** A collection whose documents carry `enabled` and that a patch can name. */
type Switchable = Extract<
  ConfigComponentPatch["collection"],
  "Agent" | "InferenceBackend" | "Task" | "Schedule" | "Trigger" | "EventSource"
>;

/**
 * Turns a document on or off with a patch of `enabled` alone. Saving the
 * whole document rebuilt from its view would rewrite every other field: a
 * concurrent edit would be lost, and a document missing a field the view
 * fills in (an agent with no profile yet) would be saved with it.
 */
export function setEnabled(
  changeConfig: ShellActions["changeConfig"],
  nodeDid: string,
  collection: Switchable,
  id: string,
  enabled: boolean,
) {
  return changeConfig(
    "patchConfigComponents",
    {
      nodeDid,
      patches: [{ collection, id, changes: { enabled } } as ConfigComponentPatch],
    },
    `turn it ${enabled ? "on" : "off"}`,
  );
}
