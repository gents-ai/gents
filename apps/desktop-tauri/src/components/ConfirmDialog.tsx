import { useEffect, useRef } from "react";

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

  useEffect(() => {
    if (open) cancelRef.current?.focus();
  }, [open]);

  if (!open) return null;

  return (
    <div
      className="dialog-backdrop open"
      role="presentation"
      onClick={onCancel}
      onKeyDown={(event) => {
        if (event.key === "Escape") {
          event.stopPropagation();
          onCancel();
        }
      }}
    >
      <div
        className="dialog confirm-dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby="confirm-dialog-title"
        data-testid="confirm-dialog"
        onClick={(event) => event.stopPropagation()}
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
