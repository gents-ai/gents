import {
  useEffect,
  useMemo,
  useRef,
  useState,
  type FormEvent,
  type KeyboardEvent,
} from "react";

import { isTerminalTurnState } from "../../chat-shell.js";
import type { SkillView } from "@source-inc/gents-desktop-client";
import { CancelButton } from "../cancelUx/index.js";
import { applySkillSelection, slashSkillSuggestion } from "./slashSkills.js";

export type ChatComposerProps = {
  activeRequestId: string | null;
  approxSerializedBytes: number;
  behaviorLabel: string | null;
  canSend: boolean;
  configuredPeerCount: number;
  dialedPeerCount: number;
  draft: string;
  interruptVisible: boolean;
  rowCount: number;
  sendHint: string | null;
  sending: boolean;
  turnState: string | null;
  onDraftChange: (value: string) => void;
  onConfigureInference?: () => void;
  onInterruptClick: () => void;
  onSend: (event: FormEvent) => void;
  skills?: SkillView[];
};

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
  interruptVisible,
  sendHint,
  sending,
  turnState,
  onDraftChange,
  onConfigureInference,
  onInterruptClick,
  onSend,
  skills = [],
}: ChatComposerProps) {
  const inputRef = useRef<HTMLTextAreaElement | null>(null);
  const [caret, setCaret] = useState(0);
  const [menuIndex, setMenuIndex] = useState(0);
  const [menuDismissed, setMenuDismissed] = useState(false);

  const suggestion = useMemo(
    () => (menuDismissed ? null : slashSkillSuggestion(draft, caret, skills)),
    [draft, caret, skills, menuDismissed],
  );

  useEffect(() => {
    setMenuIndex(0);
  }, [suggestion?.query, suggestion?.items.length]);

  function acceptSuggestion(skillId: string) {
    if (!suggestion) {
      return;
    }
    const next = applySkillSelection(draft, suggestion, skillId);
    onDraftChange(next.draft);
    setCaret(next.caret);
    const input = inputRef.current;
    if (input) {
      window.requestAnimationFrame(() => {
        input.focus();
        input.setSelectionRange(next.caret, next.caret);
      });
    }
  }

  useEffect(() => {
    const input = inputRef.current;
    if (!input) {
      return;
    }
    input.style.height = "auto";
    input.style.height = `${Math.min(input.scrollHeight, COMPOSER_MAX_HEIGHT_PX)}px`;
  }, [draft]);

  function onComposerKeyDown(event: KeyboardEvent<HTMLTextAreaElement>) {
    if (event.nativeEvent.isComposing) {
      return;
    }

    if (suggestion) {
      if (event.key === "ArrowDown" || event.key === "ArrowUp") {
        event.preventDefault();
        const delta = event.key === "ArrowDown" ? 1 : -1;
        setMenuIndex(
          (index) =>
            (index + delta + suggestion.items.length) % suggestion.items.length,
        );
        return;
      }
      if (event.key === "Enter" || event.key === "Tab") {
        event.preventDefault();
        acceptSuggestion(suggestion.items[menuIndex].skillId);
        return;
      }
      if (event.key === "Escape") {
        event.preventDefault();
        setMenuDismissed(true);
        return;
      }
    }

    if (
      event.key !== "Enter" ||
      event.shiftKey ||
      event.altKey ||
      event.ctrlKey ||
      event.metaKey ||
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
      <div className="composer-input-wrap">
        {suggestion ? (
          <ul
            className="slash-skill-menu"
            data-testid="slash-skill-menu"
            role="listbox"
            aria-label="Skills"
          >
            {suggestion.items.map((skill, index) => (
              <li key={skill.skillId} role="presentation">
                <button
                  type="button"
                  role="option"
                  aria-selected={index === menuIndex}
                  className={index === menuIndex ? "is-active" : ""}
                  data-testid={`slash-skill-${skill.skillId}`}
                  onMouseDown={(event) => {
                    event.preventDefault();
                    acceptSuggestion(skill.skillId);
                  }}
                >
                  <span className="slash-skill-id">/{skill.skillId}</span>
                  <span className="slash-skill-name">
                    {skill.displayName ?? skill.name ?? ""}
                  </span>
                </button>
              </li>
            ))}
          </ul>
        ) : null}
        <textarea
          aria-label="Message the selected agent"
          className="composer-input"
          data-testid="composer-input"
          ref={inputRef}
          rows={1}
          onChange={(event) => {
            onDraftChange(event.currentTarget.value);
            setCaret(event.currentTarget.selectionStart ?? 0);
            setMenuDismissed(false);
          }}
          onKeyDown={onComposerKeyDown}
          onKeyUp={(event) => setCaret(event.currentTarget.selectionStart ?? 0)}
          onClick={(event) => setCaret(event.currentTarget.selectionStart ?? 0)}
          placeholder="Message the selected agent"
          value={draft}
        />
      </div>

      <div className="composer-footer">
        <div className="muted small" data-testid="composer-status">
          {sendHint ??
            turnStatusLabel(turnState) ??
            (skills.length > 0
              ? "⏎ send · ⇧⏎ new line · / skills"
              : "⏎ send · ⇧⏎ new line")}
        </div>
        <div className="composer-actions">
          {onConfigureInference ? (
            <button
              className="ghost-button"
              data-testid="composer-configure-inference"
              onClick={onConfigureInference}
              type="button"
            >
              Configure inference
            </button>
          ) : null}
          <CancelButton
            activeRequestId={activeRequestId}
            forceVisible={interruptVisible}
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
