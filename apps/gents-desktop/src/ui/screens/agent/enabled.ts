import type { ConfigComponentPatch } from "@source-inc/gents-desktop-client";
import type { ShellActions } from "@/../hooks/shellActions";

type Switchable = Extract<
  ConfigComponentPatch["collection"],
  "Agent" | "InferenceBackend" | "Task" | "Schedule" | "Trigger" | "EventSource"
>;

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
