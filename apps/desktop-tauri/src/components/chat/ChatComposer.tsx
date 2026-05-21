import type { FormEvent, KeyboardEvent } from "react";

import { CancelButton } from "../cancelUx";
import { formatBytes } from "../../lib/types";

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

export function ChatComposer({
  activeRequestId,
  approxSerializedBytes,
  behaviorLabel,
  canSend,
  configuredPeerCount,
  dialedPeerCount,
  draft,
  rowCount,
  sendHint,
  sending,
  turnState,
  onDraftChange,
  onInterruptClick,
  onSend,
}: ChatComposerProps) {
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
      <div className="composer-toolbar">
        <div className="muted small">
          Selected behavior: {behaviorLabel ?? "default"}
        </div>
        <div className="muted small">
          {rowCount} rows / {formatBytes(approxSerializedBytes)}
        </div>
      </div>

      <textarea
        className="composer-input"
        data-testid="composer-input"
        onChange={(event) => onDraftChange(event.currentTarget.value)}
        onKeyDown={onComposerKeyDown}
        placeholder="Message the selected agent"
        value={draft}
      />

      <div className="composer-footer">
        <div className="muted small">
          {sendHint ?? turnState ?? "idle"} · peers {dialedPeerCount}/
          {configuredPeerCount}
        </div>
        <div className="composer-actions" style={{ display: "flex", gap: 8 }}>
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
