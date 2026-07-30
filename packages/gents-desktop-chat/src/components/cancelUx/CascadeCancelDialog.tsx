import {
  useEffect,
  useRef,
  useState,
  type JSX,
  type KeyboardEvent,
  type MouseEvent,
} from "react";

import {
  interruptChatRequest,
  previewChatInterruptCascade,
} from "../../interruptRequest.js";
import type {
  CascadeAffectedRequest,
  CascadeCancelPreview,
} from "@source-inc/gents-desktop-client";

export type CascadeCancelDialogProps = {
  open: boolean;
  rootRequestId: string;
  agentDid?: string | null;
  onClose: () => void;
  onAccepted: (interruptRequestedAt: string | null) => void;
  onAlreadyInterrupted: () => void;
  onError?: (message: string) => void;
  previewInterrupt?: typeof previewChatInterruptCascade;
  interrupt?: typeof interruptChatRequest;
};

type Phase = "loading" | "showing" | "submitting";

export function CascadeCancelDialog(
  props: CascadeCancelDialogProps,
): JSX.Element | null {
  const {
    open,
    rootRequestId,
    agentDid,
    onClose,
    onAccepted,
    onAlreadyInterrupted,
    onError,
    previewInterrupt = previewChatInterruptCascade,
    interrupt = interruptChatRequest,
  } = props;

  const [phase, setPhase] = useState<Phase>("loading");
  const [preview, setPreview] = useState<CascadeCancelPreview | null>(null);
  const [updated, setUpdated] = useState(false);
  const [announce, setAnnounce] = useState("");

  const dialogRef = useRef<HTMLDivElement | null>(null);
  const backdropRef = useRef<HTMLDivElement | null>(null);
  const cancelRef = useRef<HTMLButtonElement | null>(null);
  const confirmRef = useRef<HTMLButtonElement | null>(null);

  useEffect(() => {
    if (!open) return;
    let cancelled = false;
    setPhase("loading");
    setPreview(null);
    setUpdated(false);
    setAnnounce("");
    previewInterrupt({
      requestId: rootRequestId,
      agentDid: agentDid ?? null,
      includeTerminal: true,
    })
      .then((p) => {
        if (cancelled) return;
        setPreview(p);
        setPhase("showing");
      })
      .catch((e: unknown) => {
        if (cancelled) return;
        onError?.(typeof e === "string" ? e : String(e));
        onClose();
      });
    return () => {
      cancelled = true;
    };
  }, [open, rootRequestId, agentDid, onClose, onError, previewInterrupt]);

  useEffect(() => {
    if (open && phase === "showing") {
      cancelRef.current?.focus();
    }
  }, [open, phase]);

  useEffect(() => {
    if (!open) return;
    const handler = (e: globalThis.KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    document.addEventListener("keydown", handler);
    return () => document.removeEventListener("keydown", handler);
  }, [open, onClose]);

  if (!open) return null;

  function onConfirm() {
    if (!preview || phase === "submitting") return;
    setPhase("submitting");
    interrupt({
      requestId: rootRequestId,
      agentDid: agentDid ?? null,
      cause: "userCancelled",
      cascade: true,
      expectedPreviewSignature: preview.previewSignature,
    })
      .then((r) => {
        if (r.stalePreview && r.preview) {
          setPreview(r.preview);
          setUpdated(true);
          setAnnounce(
            "Cascade preview has changed — please re-confirm before proceeding.",
          );
          setPhase("showing");
          setTimeout(() => confirmRef.current?.focus(), 0);
          return;
        }
        if (r.alreadyInterrupted) {
          onAlreadyInterrupted();
          onClose();
          return;
        }
        onAccepted(r.interruptRequestedAt ?? null);
        onClose();
      })
      .catch((e: unknown) => {
        onError?.(typeof e === "string" ? e : String(e));
        onClose();
      });
  }

  function onDialogKeyDown(e: KeyboardEvent<HTMLDivElement>) {
    if (e.key !== "Tab") return;
    if (!dialogRef.current) return;
    const focusables = Array.from(
      dialogRef.current.querySelectorAll<HTMLElement>(
        'a[href], button:not([disabled]), select, input:not([disabled]), [tabindex]:not([tabindex="-1"])',
      ),
    );
    if (focusables.length === 0) return;
    const first = focusables[0];
    const last = focusables[focusables.length - 1];
    const active = document.activeElement;
    if (e.shiftKey && active === first) {
      e.preventDefault();
      last.focus();
    } else if (!e.shiftKey && active === last) {
      e.preventDefault();
      first.focus();
    }
  }

  function onBackdropClick(e: MouseEvent<HTMLDivElement>) {
    if (e.target === backdropRef.current) onClose();
  }

  return (
    <div
      ref={backdropRef}
      className="dialog-backdrop open"
      role="presentation"
      onClick={onBackdropClick}
    >
      <div
        ref={dialogRef}
        className="dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby="cascade-cancel-title"
        onKeyDown={onDialogKeyDown}
      >
        <header>
          <h3 id="cascade-cancel-title">Interrupt parent request?</h3>
          <div className="sub">
            <span>
              Root <code>{rootRequestId}</code>
            </span>
            {preview ? (
              <>
                <span>·</span>
                <span>
                  signature <code>{preview.previewSignature}</code>
                </span>
              </>
            ) : null}
            {updated ? (
              <span className="preview-updated-pill">
                preview updated — re-confirm to commit
              </span>
            ) : null}
          </div>
          { }
          <div
            role="status"
            aria-live="polite"
            style={{
              position: "absolute",
              width: 1,
              height: 1,
              padding: 0,
              margin: -1,
              overflow: "hidden",
              clip: "rect(0,0,0,0)",
              whiteSpace: "nowrap",
            }}
          >
            {announce}
          </div>
        </header>

        <div className="body">
          {phase === "loading" ? <p>Loading preview…</p> : null}
          {preview ? (
            <>
              {renderGroup(
                "will-interrupt",
                "Will be interrupted",
                preview.willInterrupt,
              )}
              {renderGroup(
                "will-detach",
                "Will keep running detached",
                preview.willDetach,
              )}
              {renderGroup(
                "already-terminal",
                "Already finished",
                preview.alreadyTerminal,
              )}
              {renderUnknownPolicy(preview.unknownPolicy)}
            </>
          ) : null}
        </div>

        <footer>
          <button
            ref={cancelRef}
            type="button"
            className="btn btn-ghost"
            onClick={onClose}
          >
            Cancel
          </button>
          <button
            ref={confirmRef}
            type="button"
            data-testid="cascade-interrupt-confirm"
            className="btn btn-danger"
            disabled={phase === "submitting" || !preview}
            onClick={onConfirm}
          >
            {phase === "submitting"
              ? "Interrupting…"
              : "Interrupt parent + eligible descendants"}
          </button>
        </footer>
      </div>
    </div>
  );
}

function renderGroup(
  cls: string,
  heading: string,
  items: CascadeAffectedRequest[],
) {
  if (items.length === 0) return null;
  return (
    <section className={`group ${cls}`}>
      <h4>
        {heading} <span className="count">{items.length}</span>
      </h4>
      <ul>
        {items.map((it) => (
          <li key={it.requestId}>
            <span className="reqid">{it.requestId}</span>
            <span className="desc">
              {it.toolName ?? ""}
              {it.behaviorId ? ` · ${it.behaviorId}` : ""}
            </span>
            <span className="meta">
              {it.awaitMode ?? "-"}/{it.cancelPolicy ?? "?"} ·{" "}
              {it.lifecycleState ?? "?"}
            </span>
          </li>
        ))}
      </ul>
    </section>
  );
}

function renderUnknownPolicy(items: CascadeAffectedRequest[]) {
  if (items.length === 0) return null;
  return (
    <section className="group unknown-policy">
      <h4>
        No cancellation policy — will be left running{" "}
        <span className="count">{items.length}</span>
      </h4>
      <ul>
        {items.map((it) => (
          <li key={it.requestId}>
            <span className="reqid">{it.requestId}</span>
            <span className="desc">{it.toolName ?? ""}</span>
            <span className="meta">
              {it.awaitMode ?? "-"}/? · {it.lifecycleState ?? "?"}
            </span>
          </li>
        ))}
        <li className="warning-row">
          These background tasks don&apos;t declare how they handle
          cancellation, so confirming will leave them running.
        </li>
      </ul>
    </section>
  );
}
