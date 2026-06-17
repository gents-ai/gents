import { useEffect, useState } from "react";
import type { FormEvent } from "react";

import type { P2PHealth } from "../../lib/types";
import { displayConversationTitle } from "../../lib/types";

export type ChatHeaderProps = {
  behaviorLabel: string | null;
  runtimeHealth: P2PHealth | null;
  selectedConversationTitle: string | null;
  selectedSessionId: string | null;
  onRenameConversationTitle: (sessionId: string, title: string) => void | Promise<void>;
};

export function ChatHeader({
  behaviorLabel,
  runtimeHealth,
  selectedConversationTitle,
  selectedSessionId,
  onRenameConversationTitle,
}: ChatHeaderProps) {
  const visibleConversationTitle = selectedSessionId
    ? displayConversationTitle(selectedConversationTitle)
    : "Start a conversation";
  const [isRenamingTitle, setIsRenamingTitle] = useState(false);
  const [titleDraft, setTitleDraft] = useState(selectedConversationTitle ?? "");
  const [renamingTitle, setRenamingTitle] = useState(false);

  useEffect(() => {
    setIsRenamingTitle(false);
    setTitleDraft(selectedConversationTitle ?? "");
  }, [selectedConversationTitle, selectedSessionId]);

  async function submitTitleRename(event?: FormEvent) {
    event?.preventDefault();
    if (!selectedSessionId) {
      return;
    }

    const trimmed = titleDraft.trim();
    if (!trimmed) {
      setIsRenamingTitle(false);
      setTitleDraft(selectedConversationTitle ?? "");
      return;
    }

    if (trimmed === (selectedConversationTitle ?? "").trim()) {
      setIsRenamingTitle(false);
      return;
    }

    setRenamingTitle(true);
    try {
      await onRenameConversationTitle(selectedSessionId, trimmed);
      setIsRenamingTitle(false);
    } catch {
      // The shell surfaces the error banner; keep the inline editor open.
    } finally {
      setRenamingTitle(false);
    }
  }

  return (
    <header className="chat-header">
      <div className="chat-title-block">
        {selectedSessionId ? (
          isRenamingTitle ? (
            <form className="title-rename-form" onSubmit={submitTitleRename}>
              <input
                autoFocus
                className="title-rename-input"
                data-testid="conversation-title-input"
                onBlur={() => void submitTitleRename()}
                onChange={(event) => setTitleDraft(event.currentTarget.value)}
                onKeyDown={(event) => {
                  if (event.key === "Escape") {
                    setIsRenamingTitle(false);
                    setTitleDraft(selectedConversationTitle ?? "");
                  }
                }}
                value={titleDraft}
              />
            </form>
          ) : (
            <div className="chat-title-row">
              <h2>{visibleConversationTitle}</h2>
              <button
                className="icon-button"
                data-testid="conversation-title-edit"
                disabled={renamingTitle}
                onClick={() => setIsRenamingTitle(true)}
                type="button"
              >
                Edit
              </button>
            </div>
          )
        ) : (
          <h2>{visibleConversationTitle}</h2>
        )}
      </div>
      <div className="chat-status">
        {behaviorLabel ? <span className="chip">{behaviorLabel}</span> : null}
        <span
          className={runtimeHealth?.status === "healthy" ? "chip chip-green" : "chip"}
        >
          {runtimeHealth?.status ?? "unknown"}
        </span>
      </div>
    </header>
  );
}
