import { toast } from "sonner";

import { actionFailure, wasShown } from "../../hooks/actionFailure";

export function toastFailure(label: string, error: unknown) {
  if (!wasShown(error)) {
    const message = actionFailure(label, error);
    toast(message, { id: message });
  }
}
