import { useEffect, useRef, type KeyboardEvent } from "react";

/// In-app confirmation dialog. Native `window.confirm`/`alert` are dead in
/// the packaged app (wry's WKWebView has no JS-dialog delegate and resolves
/// confirm() to false without showing anything) — always use this instead.
export type ConfirmDialogProps = {
  open: boolean;
  title: string;
  message: string;
  confirmLabel?: string;
  cancelLabel?: string;
  danger?: boolean;
  onConfirm: () => void;
  onCancel: () => void;
};

export function ConfirmDialog({
  open,
  title,
  message,
  confirmLabel = "Confirm",
  cancelLabel = "Cancel",
  danger = false,
  onConfirm,
  onCancel,
}: ConfirmDialogProps) {
  const cancelRef = useRef<HTMLButtonElement | null>(null);
  const dialogRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    if (open) cancelRef.current?.focus();
  }, [open]);

  // ESC handler — document-level while open, so it works no matter where
  // focus has landed (same pattern as CascadeCancelDialog).
  useEffect(() => {
    if (!open) return;
    const handler = (event: globalThis.KeyboardEvent) => {
      if (event.key === "Escape") onCancel();
    };
    document.addEventListener("keydown", handler);
    return () => document.removeEventListener("keydown", handler);
  }, [open, onCancel]);

  if (!open) return null;

  // Tab trap: aria-modal promises inertness, so keep focus cycling inside
  // the dialog instead of escaping into the (visually hidden) page.
  function onDialogKeyDown(event: KeyboardEvent<HTMLDivElement>) {
    if (event.key !== "Tab" || !dialogRef.current) return;
    const focusables = Array.from(
      dialogRef.current.querySelectorAll<HTMLElement>(
        'button:not([disabled]), a[href], select, input:not([disabled]), [tabindex]:not([tabindex="-1"])',
      ),
    );
    if (focusables.length === 0) return;
    const first = focusables[0];
    const last = focusables[focusables.length - 1];
    const active = document.activeElement;
    if (event.shiftKey && active === first) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && active === last) {
      event.preventDefault();
      first.focus();
    }
  }

  return (
    <div className="dialog-backdrop open" role="presentation" onClick={onCancel}>
      <div
        className="dialog confirm-dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby="confirm-dialog-title"
        data-testid="confirm-dialog"
        ref={dialogRef}
        onClick={(event) => event.stopPropagation()}
        onKeyDown={onDialogKeyDown}
      >
        <header>
          <h3 id="confirm-dialog-title">{title}</h3>
        </header>
        <div className="body">
          <p>{message}</p>
        </div>
        <footer className="confirm-dialog-actions">
          <button
            className="btn btn-ghost"
            data-testid="confirm-dialog-cancel"
            ref={cancelRef}
            type="button"
            onClick={onCancel}
          >
            {cancelLabel}
          </button>
          <button
            className={danger ? "btn btn-danger" : "btn"}
            data-testid="confirm-dialog-confirm"
            type="button"
            onClick={onConfirm}
          >
            {confirmLabel}
          </button>
        </footer>
      </div>
    </div>
  );
}
