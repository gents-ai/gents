/* The frame's `toast`: the same call shape as the shell's (`toast.success`,
   `toast.error`, `toast(message)`), delivered to the shell's toaster over
   the port, since a frame draws no toasts of its own. */
export type ToastKind = "message" | "success" | "error" | "info" | "warning";

let sink: ((kind: ToastKind, text: string) => void) | null = null;

/** the runtime installs where a toast goes; nothing is shown before that */
export function setToastSink(
  next: ((kind: ToastKind, text: string) => void) | null,
): void {
  sink = next;
}

const text = (message: unknown) =>
  typeof message === "string"
    ? message
    : message instanceof Error
      ? message.message
      : String(message);

function send(kind: ToastKind, message: unknown) {
  sink?.(kind, text(message));
}

export const toast = Object.assign((message: unknown) => send("message", message), {
  success: (message: unknown) => send("success", message),
  error: (message: unknown) => send("error", message),
  info: (message: unknown) => send("info", message),
  warning: (message: unknown) => send("warning", message),
});
