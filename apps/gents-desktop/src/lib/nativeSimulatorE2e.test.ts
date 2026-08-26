import { invoke } from "@tauri-apps/api/core";
import { beforeEach, describe, expect, it, vi } from "vitest";

import {
  conversationRowCount,
  findAgentChatButton,
  findAssistantResponseMarker,
  findNewChatButton,
  findPairingReadyStatus,
  isConversationTurnSettled,
  startNativeSimulatorE2e,
} from "./nativeSimulatorE2e";

vi.mock("@tauri-apps/api/core", () => ({ invoke: vi.fn() }));

beforeEach(() => {
  vi.mocked(invoke).mockReset();
});

describe("startNativeSimulatorE2e", () => {
  it("does not probe test-only bridge commands in an ordinary Tauri build", async () => {
    Object.defineProperty(window, "__TAURI_INTERNALS__", {
      configurable: true,
      value: { invoke: vi.fn() },
    });

    await startNativeSimulatorE2e(false);

    expect(invoke).not.toHaveBeenCalled();
  });
});

describe("findAgentChatButton", () => {
  it("finds the current fleet detail control after pairing materializes a deployment", () => {
    document.body.innerHTML = `
      <button data-testid="fleet-detail-name-peer-a">iPhone E2E</button>
    `;

    expect(findAgentChatButton("iPhone E2E")).not.toBeNull();
  });

  it("uses an exact label instead of colliding with a similarly named deployment", () => {
    document.body.innerHTML = `
      <button data-testid="fleet-detail-name-peer-a">iPhone E2E staging</button>
      <button data-testid="fleet-detail-name-peer-b">iPhone E2E</button>
    `;

    expect(findAgentChatButton("iPhone E2E")?.dataset.testid).toBe(
      "fleet-detail-name-peer-b",
    );
  });
});

describe("findNewChatButton", () => {
  it("finds the enabled environment action in the current sessions flow", () => {
    document.body.innerHTML = `
      <button data-testid="sidebar-new-chat-disabled" disabled>New session</button>
      <button data-testid="sidebar-new-chat-default">New session</button>
    `;

    expect(findNewChatButton("iPhone E2E")?.disabled).toBe(false);
  });

  it("does not silently choose between multiple enabled environments", () => {
    document.body.innerHTML = `
      <button data-testid="sidebar-new-chat-default">New session</button>
      <button data-testid="sidebar-new-chat-review">New session</button>
    `;

    expect(findNewChatButton("iPhone E2E")).toBeNull();
  });
});

describe("findPairingReadyStatus", () => {
  it("waits for the requested deployment's truthful signed readiness", () => {
    document.body.innerHTML = `
      <p data-testid="fleet-pair-status">
        iPhone E2E is ready. Signed membership and bidirectional replication were observed.
      </p>
    `;

    expect(findPairingReadyStatus("iPhone E2E")).not.toBeNull();
    expect(findPairingReadyStatus("iPhone E2E staging")).toBeNull();
  });
});

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
