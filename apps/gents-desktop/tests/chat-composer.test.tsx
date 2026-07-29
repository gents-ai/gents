import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { ChatComposer } from "@source-inc/gents-desktop-chat";

function renderComposer(
  overrides: {
    activeRequestId?: string | null;
    interruptVisible?: boolean;
    turnState?: string | null;
    sendHint?: string | null;
  } = {},
) {
  return render(
    <ChatComposer
      activeRequestId={overrides.activeRequestId ?? null}
      approxSerializedBytes={21000}
      behaviorLabel="default"
      canSend
      configuredPeerCount={1}
      dialedPeerCount={1}
      draft=""
      interruptVisible={overrides.interruptVisible ?? false}
      rowCount={42}
      sendHint={overrides.sendHint ?? null}
      sending={false}
      turnState={overrides.turnState ?? null}
      onDraftChange={vi.fn()}
      onInterruptClick={vi.fn()}
      onSend={vi.fn()}
    />,
  );
}

describe("ChatComposer chrome", () => {
  it("shows the Enter-to-send affordance when idle", () => {
    renderComposer();
    expect(screen.getByTestId("composer-status")).toHaveTextContent(
      "⏎ send · ⇧⏎ new line",
    );
  });

  it("translates turn states into operator language, never raw enums", () => {
    renderComposer({ turnState: "waitingForClaim" });
    const status = screen.getByTestId("composer-status");
    expect(status).toHaveTextContent("Working…");
    expect(status).not.toHaveTextContent("waitingForClaim");
  });

  it("shows Responding… while streaming", () => {
    renderComposer({ turnState: "streaming" });
    expect(screen.getByTestId("composer-status")).toHaveTextContent("Responding…");
  });

  it("shows Interrupt while a submitted request awaits remote observation", () => {
    renderComposer({
      activeRequestId: "req-remote",
      interruptVisible: true,
      turnState: null,
    });
    expect(screen.getByRole("button", { name: "Interrupt" })).toBeEnabled();
  });

  it("lets a disabled-send hint take precedence", () => {
    renderComposer({ sendHint: "Connect an agent first" });
    expect(screen.getByTestId("composer-status")).toHaveTextContent(
      "Connect an agent first",
    );
  });

  it("no longer renders store internals or permanent behavior chrome", () => {
    renderComposer();
    expect(screen.queryByText(/Selected behavior/)).not.toBeInTheDocument();
    expect(screen.queryByText(/rows \//)).not.toBeInTheDocument();
  });
});
