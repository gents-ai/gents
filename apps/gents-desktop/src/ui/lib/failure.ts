import { toast } from "sonner";

import { actionFailure, wasShown } from "../../hooks/desktopShellRuntime";

/** Tells the person an action failed, unless the action already did. */
export function toastFailure(label: string, error: unknown) {
  if (!wasShown(error)) toast(actionFailure(label, error));
}
