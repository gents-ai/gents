import { useEffect, useRef, type FormEvent, type KeyboardEvent } from "react";

import { isTerminalTurnState } from "../../lib/chat-shell";
import { CancelButton } from "../cancelUx";

export type ChatComposerProps = {
  activeRequestId: string | null;
  approxSerializedBytes: number;
  behaviorLabel: string | null;
  canSend: boolean;
  configuredPeerCount: number;
  dialedPeerCount: number;
  draft: string;
  rowCount: number;
  sendHint: string | null;
  sending: boolean;
  turnState: string | null;
  onDraftChange: (value: string) => void;
  onInterruptClick: () => void;
  onSend: (event: FormEvent) => void;
};

/** Operator-facing turn status — never the raw state-machine enum. */
function turnStatusLabel(turnState: string | null): string | null {
  if (!turnState || isTerminalTurnState(turnState)) {
    return null;
  }
  return turnState === "streaming" ? "Responding…" : "Working…";
}

const COMPOSER_MAX_HEIGHT_PX = 320;

export function ChatComposer({
  activeRequestId,
  canSend,
  draft,
  sendHint,
  sending,
  turnState,
  onDraftChange,
  onInterruptClick,
  onSend,
}: ChatComposerProps) {
  const inputRef = useRef<HTMLTextAreaElement | null>(null);

  // Auto-grow: single line at rest, expands with content up to a cap.
  useEffect(() => {
    const input = inputRef.current;
    if (!input) {
      return;
    }
    input.style.height = "auto";
    input.style.height = `${Math.min(input.scrollHeight, COMPOSER_MAX_HEIGHT_PX)}px`;
  }, [draft]);

  function onComposerKeyDown(event: KeyboardEvent<HTMLTextAreaElement>) {
    if (
      event.key !== "Enter" ||
      event.shiftKey ||
      event.altKey ||
      event.ctrlKey ||
      event.metaKey ||
      event.nativeEvent.isComposing ||
      !canSend
    ) {
      return;
    }

    event.preventDefault();
    event.currentTarget.form?.requestSubmit();
  }

  return (
    <form
      className="panel composer-panel"
      data-testid="composer-form"
      onSubmit={onSend}
    >
      <textarea
        className="composer-input"
        data-testid="composer-input"
        ref={inputRef}
        rows={1}
        onChange={(event) => onDraftChange(event.currentTarget.value)}
        onKeyDown={onComposerKeyDown}
        placeholder="Message the selected agent"
        value={draft}
      />

      <div className="composer-footer">
        <div className="muted small" data-testid="composer-status">
          {sendHint ?? turnStatusLabel(turnState) ?? "⏎ send · ⇧⏎ new line"}
        </div>
        <div className="composer-actions">
          <CancelButton
            activeRequestId={activeRequestId}
            turnState={turnState}
            onInterruptClick={onInterruptClick}
          />
          <button
            className="primary-button"
            data-testid="composer-send"
            disabled={!canSend}
            type="submit"
          >
            {sending ? "Sending…" : "Send"}
          </button>
        </div>
      </div>
    </form>
  );
}
