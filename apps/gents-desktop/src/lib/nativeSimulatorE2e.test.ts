import { describe, expect, it } from "vitest";

import { findAssistantResponseMarker } from "./nativeSimulatorE2e";

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
