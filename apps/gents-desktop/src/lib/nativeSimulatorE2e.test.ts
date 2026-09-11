import { invoke } from "@tauri-apps/api/core";
import { beforeEach, describe, expect, it, vi } from "vitest";

import {
  sessionRowCount,
  findAgentChatButton,
  findAgentDeploymentControl,
  findAssistantResponseMarker,
  findNewChatButton,
  isSessionTurnSettled,
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

describe("findAgentDeploymentControl", () => {
  it("finds the current fleet detail control after enrollment materializes a deployment", () => {
    document.body.innerHTML = `
      <button data-testid="fleet-detail-name-peer-a">iPhone E2E</button>
    `;

    expect(findAgentDeploymentControl("iPhone E2E")).not.toBeNull();
  });

  it("uses an exact label instead of colliding with a similarly named deployment", () => {
    document.body.innerHTML = `
      <button data-testid="fleet-detail-name-peer-a">iPhone E2E staging</button>
      <button data-testid="fleet-detail-name-peer-b">iPhone E2E</button>
    `;

    expect(findAgentDeploymentControl("iPhone E2E")?.dataset.testid).toBe(
      "fleet-detail-name-peer-b",
    );
  });
});

describe("findAgentChatButton", () => {
  it("waits for the matching deployment's signed-ready chat control", () => {
    document.body.innerHTML = `
      <button aria-label="Open iPhone E2E staging chat" data-testid="fleet-chat-peer-a"></button>
      <button aria-label="Open iPhone E2E chat" data-testid="fleet-chat-peer-b" disabled></button>
    `;

    expect(findAgentDeploymentControl("iPhone E2E")).not.toBeNull();
    expect(findAgentChatButton("iPhone E2E")).toBeNull();

    document
      .querySelector<HTMLButtonElement>('[data-testid="fleet-chat-peer-b"]')
      ?.removeAttribute("disabled");

    expect(findAgentChatButton("iPhone E2E")?.dataset.testid).toBe("fleet-chat-peer-b");
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

describe("isSessionTurnSettled", () => {
  it("waits for the interrupt control to clear after the response arrives", () => {
    document.body.innerHTML = `
      <article data-testid="assistant-message">
        <div class="message-content">UNIQUE_MARKER</div>
      </article>
      <button data-testid="cancel-button">Interrupt</button>
    `;

    expect(isSessionTurnSettled(document, "UNIQUE_MARKER")).toBe(false);

    document.querySelector('[data-testid="cancel-button"]')?.remove();

    expect(isSessionTurnSettled(document, "UNIQUE_MARKER")).toBe(true);
  });

  it("does not declare a turn settled before the expected response arrives", () => {
    document.body.innerHTML = `
      <article data-testid="assistant-message">
        <div class="message-content">some other response</div>
      </article>
    `;

    expect(isSessionTurnSettled(document, "UNIQUE_MARKER")).toBe(false);
  });
});

describe("sessionRowCount", () => {
  it("counts session rows without mistaking filters for sessions", () => {
    document.body.innerHTML = `
      <input data-testid="session-search" />
      <div class="session-list">
        <span class="session-row">
          <button data-testid="session-session-1">first</button>
        </span>
        <span class="session-row">
          <button data-testid="session-session-2">second</button>
        </span>
      </div>
    `;

    expect(sessionRowCount(document)).toBe(2);
  });
});
