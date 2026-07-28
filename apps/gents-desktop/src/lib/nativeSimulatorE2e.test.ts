import { describe, expect, it } from "vitest";

import {
  conversationRowCount,
  findAssistantResponseMarker,
  isConversationTurnSettled,
} from "./nativeSimulatorE2e";

describe("findAssistantResponseMarker", () => {
  it("does not mistake the user prompt for an assistant response", () => {
    document.body.innerHTML = `
      <section data-testid="transcript-panel">
        <article class="message-card user-card">
          <div class="message-role">user</div>
          <div class="message-content">Reply with only: UNIQUE_MARKER</div>
        </article>
      </section>
    `;

    expect(findAssistantResponseMarker(document, "UNIQUE_MARKER")).toBeNull();
  });

  it("matches the marker inside an assistant message", () => {
    document.body.innerHTML = `
      <section data-testid="transcript-panel">
        <article class="message-card" data-testid="assistant-message">
          <div class="message-role">assistant</div>
          <div class="message-content">UNIQUE_MARKER</div>
        </article>
      </section>
    `;

    expect(findAssistantResponseMarker(document, "UNIQUE_MARKER")).not.toBeNull();
  });
});

describe("isConversationTurnSettled", () => {
  it("waits for the interrupt control to clear after the response arrives", () => {
    document.body.innerHTML = `
      <article data-testid="assistant-message">
        <div class="message-content">UNIQUE_MARKER</div>
      </article>
      <button data-testid="cancel-button">Interrupt</button>
    `;

    expect(isConversationTurnSettled(document, "UNIQUE_MARKER")).toBe(false);

    document.querySelector('[data-testid="cancel-button"]')?.remove();

    expect(isConversationTurnSettled(document, "UNIQUE_MARKER")).toBe(true);
  });

  it("does not declare a turn settled before the expected response arrives", () => {
    document.body.innerHTML = `
      <article data-testid="assistant-message">
        <div class="message-content">some other response</div>
      </article>
    `;

    expect(isConversationTurnSettled(document, "UNIQUE_MARKER")).toBe(false);
  });
});

describe("conversationRowCount", () => {
  it("counts conversation rows without mistaking filters for conversations", () => {
    document.body.innerHTML = `
      <input data-testid="conversation-search" />
      <div class="conversation-list">
        <span class="conversation-row">
          <button data-testid="conversation-session-1">first</button>
        </span>
        <span class="conversation-row">
          <button data-testid="conversation-session-2">second</button>
        </span>
      </div>
    `;

    expect(conversationRowCount(document)).toBe(2);
  });
});
