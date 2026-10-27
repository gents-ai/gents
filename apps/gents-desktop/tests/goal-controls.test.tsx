import { act, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { appliedGoalResult } from "./goal-delivery-fixture";
import { goalDeliveryStorage } from "./goal-delivery-storage";
import type { GoalControlRequest } from "@source-inc/gents-desktop-client";

import { SessionScreen } from "../src/ui/screens/SessionScreen";
import type { DesktopSessionSnapshot } from "@source-inc/gents-desktop-client";
import type { DesktopApp } from "../src/hooks/desktopApp";
import { node, testApp, withApp } from "./app-fixture";
import { deployment } from "./config-panel-wiring/fixtures";
import { MemoryNavProvider } from "@gents/shell";

vi.mock("../src/ui/screens/Markdown", () => ({
  CopyButton: () => null,
  Markdown: ({ children }: { children: string }) => <div>{children}</div>,
}));
vi.mock("@/lib/router", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../src/ui/lib/router")>()),
  navigate: vi.fn(),
}));
vi.mock("../src/ui/screens/BehaviorPicker", () => ({
  AgentPicker: () => null,
}));

const NODE = deployment.nodeDid;
const REVISION = { document: "goal-doc", composite: "cid-1" };
beforeEach(() => {
  vi.stubGlobal(
    "IntersectionObserver",
    class {
      observe() {}
      disconnect() {}
    },
  );
  vi.stubGlobal("localStorage", goalDeliveryStorage());
});
afterEach(() => vi.unstubAllGlobals());

const goal = (status: string) => ({
  goalId: "g",
  status,
  objective: "Ship the release",
  wrapupRequested: false,
  wrapupCompleted: status === "complete",
  tokensUsed: 0,
  tokenBudget: null,
  consecutiveBlockedAudits: 0,
  activeTimeSeconds: 0,
  continuationSequence: 0,
  lastBlockedReason: null,
  lastFailure: null,
  completionEvidence: null,
});

function session(over: Partial<DesktopSessionSnapshot> = {}) {
  return {
    sessionId: "session",
    nodeDid: NODE,
    agentId: "default",
    title: "Session",
    turnState: "interrupted",
    timelineItems: [],
    timelinePage: { hasOlder: false },
    latestRequestId: "request",
    latestRequestOutcome: null,
    goal: null,
    context: null,
    queuedTurns: [],
    foldedInputs: [],
    ...over,
  } as unknown as DesktopSessionSnapshot;
}

function setup(held: DesktopSessionSnapshot) {
  const controlGoal = vi.fn(async (request: GoalControlRequest) =>
    appliedGoalResult(request),
  );
  const fetchSessionGoal = vi.fn().mockResolvedValue({
    nodeDid: NODE,
    sessionId: "session",
    sessionDocId: "session-doc",
    observedAt: "2026-10-26T00:00:00Z",
    goal: held.goal ? { goalId: "g", revision: REVISION } : null,
  });
  const app = testApp({
    api: {
      controlGoal,
      acknowledgeGoalSubmission: vi.fn().mockResolvedValue(undefined),
      fetchSessionGoal,
      sendChatMessage: vi.fn(),
      sessionProvenance: vi.fn().mockResolvedValue(null),
      fetchOperationsSnapshot: vi.fn().mockResolvedValue(null),
    },
    deployments: [node()],
    session: held,
    selection: { nodeDid: NODE, sessionId: held.sessionId },
  });
  app.stores.client.setState((state) => ({
    snapshot: {
      ...state.snapshot!,
      client: { ...state.snapshot!.client!, clientDid: "test-client" },
    },
  }));
  render(
    <MemoryNavProvider initial={{ name: "session", sessionId: held.sessionId }}>
      <SessionScreen />
    </MemoryNavProvider>,
    { wrapper: withApp(app as DesktopApp) },
  );
  return { controlGoal, message: screen.getByLabelText("Message") };
}

const type = (box: HTMLElement, value: string) =>
  fireEvent.change(box, { target: { value } });
const press = (box: HTMLElement, key: string) =>
  act(async () => {
    fireEvent.keyDown(box, { key });
  });

describe("the slash menu", () => {
  it("opens with its commands when the agent has no skills, and says when nothing matches", () => {
    const { message } = setup(session());
    type(message, "/");
    expect(screen.getByRole("option", { name: /\/goal/ })).toHaveTextContent(
      "Set a goal the agent works toward until it's met",
    );
    type(message, "/zz");
    expect(screen.getByText("No commands or skills match /zz")).toBeInTheDocument();
  });

  it("offers editing the goal while one is going", () => {
    const { message } = setup(session({ goal: goal("active") as never }));
    type(message, "/");
    expect(screen.getByRole("option", { name: /\/goal/ })).toHaveTextContent(
      "Edit this session's goal",
    );
  });

  it("holds /goal while a blocked goal waits to be resumed or removed", () => {
    const { message } = setup(session({ goal: goal("blocked") as never }));
    type(message, "/");
    const option = screen.getByRole("option", { name: /\/goal/ });
    expect(option).toHaveAttribute("aria-disabled", "true");
    expect(option).toHaveTextContent("Resume or remove this session's goal first");
  });

  it("holds /goal until a running turn finishes", async () => {
    const { message, controlGoal } = setup(session({ turnState: "running" }));
    type(message, "/");
    const option = screen.getByRole("option", { name: /\/goal/ });
    expect(option).toHaveAttribute("aria-disabled", "true");
    expect(option).toHaveTextContent("Available when the agent finishes");
    await press(message, "Enter");
    expect(screen.queryByTestId("goal-mode")).toBeNull();
    expect(controlGoal).not.toHaveBeenCalled();
  });
});

describe("setting a goal", () => {
  it("turns the composer into the goal's objective and starts it", async () => {
    const { message, controlGoal } = setup(session());
    type(message, "/");
    await press(message, "Enter");
    expect(screen.getByTestId("goal-mode")).toHaveTextContent("Goal");
    expect(message).toHaveValue("");
    expect(message).toHaveAttribute("placeholder", "Describe the goal");
    type(message, "Ship the release");
    await press(message, "Enter");
    await waitFor(() =>
      expect(controlGoal).toHaveBeenCalledWith(
        expect.objectContaining({
          sessionId: "session",
          expected: null,
          action: { kind: "create", objective: "Ship the release", tokenBudget: null },
        }),
      ),
    );
    await waitFor(() => expect(screen.queryByTestId("goal-mode")).toBeNull());
  });

  it("replaces a met goal, fenced on the revision it saw", async () => {
    const { message, controlGoal } = setup(
      session({ goal: goal("complete") as never }),
    );
    type(message, "/goal Write the docs");
    await press(message, "Enter");
    await waitFor(() =>
      expect(controlGoal).toHaveBeenCalledWith(
        expect.objectContaining({
          sessionDocId: "session-doc",
          expected: REVISION,
          action: { kind: "create", objective: "Write the docs", tokenBudget: null },
        }),
      ),
    );
  });

  it("drops a /goal typed again in goal mode from the objective", async () => {
    const { message, controlGoal } = setup(session());
    type(message, "/");
    await press(message, "Enter");
    type(message, "/goal Write hello.txt");
    await press(message, "Enter");
    await waitFor(() =>
      expect(controlGoal).toHaveBeenCalledWith(
        expect.objectContaining({
          action: { kind: "create", objective: "Write hello.txt", tokenBudget: null },
        }),
      ),
    );
  });

  it("leaves goal mode with Escape or Backspace on an empty composer", async () => {
    const { message } = setup(session());
    type(message, "/");
    await press(message, "Enter");
    await press(message, "Backspace");
    expect(screen.queryByTestId("goal-mode")).toBeNull();
    type(message, "/");
    await press(message, "Enter");
    await press(message, "Escape");
    expect(screen.queryByTestId("goal-mode")).toBeNull();
  });
});

describe("the goal banner", () => {
  it("offers the controls the goal's status allows", () => {
    setup(session({ goal: goal("paused") as never }));
    expect(screen.getByRole("button", { name: "Resume goal" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Pause goal" })).toBeNull();
    expect(screen.getByRole("button", { name: "Edit goal" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Remove goal" })).toBeInTheDocument();
  });

  it("offers none once the goal is met", () => {
    setup(session({ goal: goal("complete") as never }));
    expect(screen.queryByRole("button", { name: /goal$/ })).toBeNull();
  });

  it("pauses and removes the goal at the revision it saw", async () => {
    const { controlGoal } = setup(session({ goal: goal("active") as never }));
    fireEvent.click(screen.getByRole("button", { name: "Pause goal" }));
    await waitFor(() =>
      expect(controlGoal).toHaveBeenCalledWith(
        expect.objectContaining({ expected: REVISION, action: { kind: "pause" } }),
      ),
    );
    fireEvent.click(screen.getByRole("button", { name: "Remove goal" }));
    await waitFor(() =>
      expect(controlGoal).toHaveBeenCalledWith(
        expect.objectContaining({ action: { kind: "stop" } }),
      ),
    );
  });

  it("edits the objective in the composer and gives back the draft", async () => {
    const { message, controlGoal } = setup(session({ goal: goal("active") as never }));
    type(message, "half-written message");
    fireEvent.click(screen.getByRole("button", { name: "Edit goal" }));
    expect(screen.getByTestId("goal-mode")).toHaveTextContent("Editing goal");
    expect(message).toHaveValue("Ship the release");
    type(message, "Ship the release notes");
    await press(message, "Enter");
    await waitFor(() =>
      expect(controlGoal).toHaveBeenCalledWith(
        expect.objectContaining({
          expected: REVISION,
          action: {
            kind: "edit",
            objective: "Ship the release notes",
            tokenBudget: null,
          },
        }),
      ),
    );
    await waitFor(() => expect(message).toHaveValue("half-written message"));
    expect(screen.queryByTestId("goal-mode")).toBeNull();
  });
});
