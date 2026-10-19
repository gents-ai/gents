import {
  capturedContextLabel,
  type CapturedContext,
} from "@source-inc/gents-desktop-client";
import { useId, useLayoutEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";

function ContextDialog({
  context,
  onClose,
}: {
  context: CapturedContext;
  onClose(): void;
}) {
  const dialog = useRef<HTMLDialogElement>(null);
  const title = useId();
  useLayoutEffect(() => {
    const element = dialog.current!;
    element.showModal();
    return () => element.close();
  }, []);
  return createPortal(
    <dialog
      ref={dialog}
      className="tool-activity-dialog captured-context-dialog"
      aria-labelledby={title}
      onClose={onClose}
    >
      <header className="tool-activity-header">
        <h2 id={title}>{capturedContextLabel(context)}</h2>
        <button
          type="button"
          aria-label="Close context preview"
          onClick={() => dialog.current?.close()}
        >
          <svg viewBox="0 0 24 24" aria-hidden="true">
            <path d="m6 6 12 12M6 18 18 6" />
          </svg>
        </button>
      </header>
      <div className="captured-context-body">
        <p className="captured-context-source">
          {context.unsaved
            ? "Unsaved buffer"
            : context.kind === "terminal"
              ? "Copied terminal excerpt"
              : "Captured context"}
          {context.version !== null ? `, version ${context.version}` : ""}
        </p>
        {context.uri && <p className="captured-context-uri">{context.uri}</p>}
        <pre aria-label="Captured text">{context.content}</pre>
        {!context.content && (
          <p className="captured-context-source">Empty file</p>
        )}
      </div>
    </dialog>,
    document.body,
  );
}

export function CapturedContextItems({
  contexts,
}: {
  contexts: readonly CapturedContext[];
}) {
  const [selected, setSelected] = useState<number | null>(null);
  const buttons = useRef(new Map<number, HTMLButtonElement>());
  const focusAfterClose = useRef<number | null>(null);
  useLayoutEffect(() => {
    if (selected !== null || focusAfterClose.current === null) return;
    buttons.current.get(focusAfterClose.current)?.focus();
    focusAfterClose.current = null;
  }, [selected]);
  if (!contexts.length) return null;
  return (
    <>
      <ul className="message-contexts" aria-label="Submitted context">
        {contexts.map((context, index) => (
          <li key={index}>
            <button
              type="button"
              ref={(element) => {
                if (element) buttons.current.set(index, element);
                else buttons.current.delete(index);
              }}
              aria-haspopup="dialog"
              aria-label={`Preview ${capturedContextLabel(context)}`}
              title={context.uri || capturedContextLabel(context)}
              onClick={() => setSelected(index)}
            >
              <svg viewBox="0 0 24 24" aria-hidden="true">
                <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8Zm0 0v6h6" />
              </svg>
              <span>{capturedContextLabel(context)}</span>
            </button>
          </li>
        ))}
      </ul>
      {selected !== null && contexts[selected] && (
        <ContextDialog
          context={contexts[selected]}
          onClose={() => {
            focusAfterClose.current = selected;
            setSelected(null);
          }}
        />
      )}
    </>
  );
}
