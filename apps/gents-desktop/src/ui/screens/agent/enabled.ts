import type { ConfigComponentPatch } from "@source-inc/gents-desktop-client";
import type { ShellActions } from "@/../hooks/shellActions";

type Switchable = Extract<
  ConfigComponentPatch["collection"],
  "Agent" | "InferenceBackend" | "Task" | "Schedule" | "Trigger" | "EventSource"
>;

/**
 * Patches `enabled` alone; a full rewrite from the view would lose
 * concurrent edits and save fields the view fills in.
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
