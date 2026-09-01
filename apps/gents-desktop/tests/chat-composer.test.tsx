import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { ChatComposer } from "@source-inc/gents-desktop-chat";

function renderComposer(
  overrides: {
    activeRequestId?: string | null;
    interruptVisible?: boolean;
    turnState?: string | null;
    sendHint?: string | null;
    onConfigureInference?: () => void;
    onReconnect?: () => void;
    reconnecting?: boolean;
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
      onConfigureInference={overrides.onConfigureInference}
      onReconnect={overrides.onReconnect}
      reconnecting={overrides.reconnecting}
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

  it("offers a direct inference configuration recovery action", () => {
    const onConfigureInference = vi.fn();
    renderComposer({
      sendHint: "Behavior backend is unavailable",
      onConfigureInference,
    });

    screen.getByTestId("composer-configure-inference").click();
    expect(onConfigureInference).toHaveBeenCalledOnce();
  });

  it("offers a distinct connection recovery action", () => {
    const onReconnect = vi.fn();
    const { rerender } = renderComposer({
      sendHint: "The agent stopped reporting readiness",
      onReconnect,
    });

    screen.getByTestId("composer-reconnect").click();
    expect(onReconnect).toHaveBeenCalledOnce();

    rerender(
      <ChatComposer
        activeRequestId={null}
        approxSerializedBytes={0}
        behaviorLabel="default"
        canSend={false}
        configuredPeerCount={1}
        dialedPeerCount={0}
        draft=""
        interruptVisible={false}
        rowCount={0}
        sendHint="Reconnecting"
        sending={false}
        turnState={null}
        onDraftChange={vi.fn()}
        onReconnect={onReconnect}
        reconnecting
        onInterruptClick={vi.fn()}
        onSend={vi.fn()}
      />,
    );
    expect(screen.getByTestId("composer-reconnect")).toBeDisabled();
    expect(screen.getByTestId("composer-reconnect")).toHaveTextContent("Reconnecting…");
  });

  it("no longer renders store internals or permanent behavior chrome", () => {
    renderComposer();
    expect(screen.queryByText(/Selected behavior/)).not.toBeInTheDocument();
    expect(screen.queryByText(/rows \//)).not.toBeInTheDocument();
  });
});
