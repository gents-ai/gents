import { render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { ChatComposer, type ChatActivityStatus } from "@source-inc/gents-desktop-chat";

function renderComposer(
  overrides: {
    activeRequestId?: string | null;
    activityStatus?: ChatActivityStatus | null;
    interruptVisible?: boolean;
    turnState?: string | null;
    onConfigureInference?: () => void;
  } = {},
) {
  return render(
    <ChatComposer
      activeRequestId={overrides.activeRequestId ?? null}
      activityStatus={overrides.activityStatus ?? null}
      approxSerializedBytes={21000}
      agentLabel="default"
      canSend
      draft=""
      interruptVisible={overrides.interruptVisible ?? false}
      rowCount={42}
      sending={false}
      turnState={overrides.turnState ?? null}
      onDraftChange={vi.fn()}
      onConfigureInference={overrides.onConfigureInference}
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

  it("shows why a queued turn is waiting", () => {
    renderComposer({
      turnState: "waitingForClaim",
      activityStatus: {
        kind: "waiting",
        label: "Waiting for the agent…",
        detail: "Your message is queued until the enrolled agent claims it.",
        animated: true,
      },
    });
    const status = screen.getByTestId("composer-status");
    expect(status).toHaveTextContent("Waiting for the agent…");
    expect(status).toHaveTextContent("Your message is queued");
    expect(status).toHaveAttribute("data-activity-kind", "waiting");
    expect(status).not.toHaveTextContent("waitingForClaim");
  });

  it("shows when the agent is working and why sending is blocked", () => {
    renderComposer({
      turnState: "running",
      activityStatus: {
        kind: "working",
        label: "Agent is working…",
        detail: "This turn must finish before another message can be sent.",
        animated: true,
      },
    });
    const status = screen.getByTestId("composer-status");
    expect(status).toHaveTextContent("Agent is working…");
    expect(status).toHaveTextContent("before another message can be sent");
    expect(status).toHaveAttribute("data-activity-kind", "working");
  });

  it("shows Interrupt while a submitted request awaits remote observation", () => {
    renderComposer({
      activeRequestId: "req-remote",
      interruptVisible: true,
      turnState: null,
    });
    expect(screen.getByRole("button", { name: "Interrupt" })).toBeEnabled();
  });

  it("announces message synchronization", () => {
    renderComposer({
      activityStatus: {
        kind: "syncing",
        label: "Syncing message…",
        detail: "Waiting for it to appear in the shared conversation.",
        animated: true,
      },
    });
    const status = screen.getByTestId("composer-status");
    expect(status).toHaveTextContent("Syncing message…");
    expect(status).toHaveAttribute("data-activity-kind", "syncing");
  });

  it("offers a direct inference configuration recovery action", () => {
    const onConfigureInference = vi.fn();
    renderComposer({
      activityStatus: {
        kind: "blocked",
        label: "Agent is unavailable",
        detail: "Agent backend is unavailable",
        animated: false,
      },
      onConfigureInference,
    });

    screen.getByTestId("composer-configure-inference").click();
    expect(onConfigureInference).toHaveBeenCalledOnce();
  });

  it("shows connection waiting without a manual reconnect button", () => {
    renderComposer({
      activityStatus: {
        kind: "waiting",
        label: "Waiting for the agent runtime…",
        detail: "The secure connection is recovering automatically.",
        animated: true,
      },
    });
    expect(screen.getByTestId("composer-status")).toHaveTextContent(
      "Waiting for the agent runtime…",
    );
    expect(
      screen.queryByRole("button", { name: /reconnect/i }),
    ).not.toBeInTheDocument();
  });

  it("no longer renders store internals or permanent agent chrome", () => {
    renderComposer();
    expect(screen.queryByText(/Selected agent/)).not.toBeInTheDocument();
    expect(screen.queryByText(/rows \//)).not.toBeInTheDocument();
  });
});
