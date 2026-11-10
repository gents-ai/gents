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
import { composerText, typeInComposer } from "./composer-field";
import { GOAL_REVISION, goalControl, observedGoal } from "./goal-state-fixture";
import { writeSessionGoalRead } from "../src/hooks/sessionStore";

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
const REVISION = GOAL_REVISION;
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

const goal = (status: string) => observedGoal(goalControl(status), { nodeDid: NODE });

function session(over: Partial<DesktopSessionSnapshot> = {}) {
  return {
    sessionId: "session",
    nodeDid: NODE,
    agentDid: "default",
    title: "Session",
    turnState: "interrupted",
    timelineItems: [],
    timelinePage: { hasOlder: false },
    latestRequestId: "request",
    latestRequestOutcome: null,
    goal: observedGoal(null, { nodeDid: NODE }),
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
  const fetchSessionGoal = vi.fn().mockResolvedValue(held.goal);
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
  return { app, controlGoal, message: screen.getByLabelText("Message") };
}

const type = typeInComposer;
const press = (box: HTMLElement, key: string) =>
  act(async () => {
    fireEvent.keyDown(box, { key });
  });

describe("the slash menu", () => {
  it("opens with its commands when the agent has no skills, and says when nothing matches", () => {
    const { message } = setup(session());
    type(message, "/");
    expect(screen.getByRole("option", { name: /\/goal/ })).toHaveTextContent(
      "Set a Goal the Agent works toward until it's met",
    );
    type(message, "/zz");
    expect(screen.getByText("No commands or skills match /zz")).toBeInTheDocument();
  });

  it("offers editing the goal while one is going", () => {
    const { message } = setup(session({ goal: goal("active") }));
    type(message, "/");
    expect(screen.getByRole("option", { name: /\/goal/ })).toHaveTextContent(
      "Edit this session's Goal",
    );
  });

  it("holds /goal while a blocked goal waits to be resumed or removed", () => {
    const { message } = setup(session({ goal: goal("blocked") }));
    type(message, "/");
    const option = screen.getByRole("option", { name: /\/goal/ });
    expect(option).toHaveAttribute("aria-disabled", "true");
    expect(option).toHaveTextContent("Resume or remove this session's Goal first");
  });

  it("holds /goal until a running turn finishes", async () => {
    const { message, controlGoal } = setup(session({ turnState: "running" }));
    type(message, "/");
    const option = screen.getByRole("option", { name: /\/goal/ });
    expect(option).toHaveAttribute("aria-disabled", "true");
    expect(option).toHaveTextContent("Available when the Agent finishes");
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
    expect(composerText(message)).toBe("");
    expect(message).toHaveAttribute("aria-placeholder", "Describe the Goal");
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
    const { message, controlGoal } = setup(session({ goal: goal("complete") }));
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
    setup(session({ goal: goal("paused") }));
    expect(screen.getByRole("button", { name: "Resume Goal" })).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: "Pause Goal" })).toBeNull();
    expect(screen.getByRole("button", { name: "Edit Goal" })).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Remove Goal" })).toBeInTheDocument();
  });

  it("offers none once the goal is met", () => {
    setup(session({ goal: goal("complete") }));
    expect(screen.queryByRole("button", { name: /Goal$/ })).toBeNull();
  });

  it("pauses and removes the goal at the revision it saw", async () => {
    const { controlGoal } = setup(session({ goal: goal("active") }));
    fireEvent.click(screen.getByRole("button", { name: "Pause Goal" }));
    await waitFor(() =>
      expect(controlGoal).toHaveBeenCalledWith(
        expect.objectContaining({ expected: REVISION, action: { kind: "pause" } }),
      ),
    );
    fireEvent.click(screen.getByRole("button", { name: "Remove Goal" }));
    await waitFor(() =>
      expect(controlGoal).toHaveBeenCalledWith(
        expect.objectContaining({ action: { kind: "stop" } }),
      ),
    );
  });

  it("edits the objective in the composer and gives back the draft", async () => {
    const { message, controlGoal } = setup(session({ goal: goal("active") }));
    type(message, "half-written message");
    fireEvent.click(screen.getByRole("button", { name: "Edit Goal" }));
    expect(screen.getByTestId("goal-mode")).toHaveTextContent("Editing Goal");
    expect(composerText(message)).toBe("Ship the release");
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
    await waitFor(() => expect(composerText(message)).toBe("half-written message"));
    expect(screen.queryByTestId("goal-mode")).toBeNull();
  });
});

describe("a Goal state that is not a verified fact", () => {
  const unknown = {
    kind: "unknown",
    cause: "projection_missing",
    next: "await_publication",
    reason:
      "This chat's Goal state is unknown: its Agent has not published it yet. The Agent's runtime publishes it while the Agent runs; try again then.",
  } as const;

  it("names an unknown state's cause and offers no Goal action", async () => {
    const { message, controlGoal } = setup(session({ goal: unknown }));
    expect(screen.getByTestId("goal-unavailable")).toHaveTextContent(
      `Goal unknown${unknown.reason}`,
    );
    expect(screen.queryByRole("button", { name: /Goal$/ })).toBeNull();
    type(message, "/");
    const option = screen.getByRole("option", { name: /\/goal/ });
    expect(option).toHaveAttribute("aria-disabled", "true");
    expect(option).toHaveTextContent(
      "Available once the Agent publishes this chat's Goal",
    );
    await press(message, "Enter");
    expect(screen.queryByTestId("goal-mode")).toBeNull();
    expect(controlGoal).not.toHaveBeenCalled();
  });

  it("says a chat its runtime does not hold opens with a message", () => {
    const { message } = setup(
      session({ goal: { kind: "no_readable_session", next: "send" } }),
    );
    expect(screen.getByTestId("goal-unavailable")).toHaveTextContent(
      "This chat is not open on its runtime yet. Send a message to open it, then change its Goal.",
    );
    type(message, "/");
    const option = screen.getByRole("option", { name: /\/goal/ });
    expect(option).toHaveAttribute("aria-disabled", "true");
    expect(option).toHaveTextContent("Available after you send a message");
  });

  it("does not show a Goal read that did not complete as no Goal", () => {
    const { message } = setup(session({ goal: null }));
    expect(screen.getByTestId("goal-unavailable")).toHaveTextContent(
      "This chat's Goal could not be read from its runtime.",
    );
    type(message, "/");
    expect(screen.getByRole("option", { name: /\/goal/ })).toHaveAttribute(
      "aria-disabled",
      "true",
    );
  });

  it("shows nothing for verified absence", () => {
    setup(session());
    expect(screen.queryByTestId("goal-unavailable")).toBeNull();
    expect(screen.queryByRole("button", { name: /Goal$/ })).toBeNull();
  });
});

describe("a Goal whose refresh did not complete", () => {
  it("stays shown, marked not current, with no Goal action", () => {
    const { app, message, controlGoal } = setup(session({ goal: goal("active") }));
    act(() =>
      writeSessionGoalRead(app.stores.session, {
        kind: "incomplete",
        sessionId: "session",
        nodeDid: NODE,
      }),
    );
    expect(screen.getByTestId("goal-not-current")).toHaveTextContent(
      "Not current · couldn't refresh",
    );
    expect(screen.getByText("Ship the release")).toBeInTheDocument();
    expect(screen.queryByRole("button", { name: /Goal$/ })).toBeNull();
    type(message, "/");
    const option = screen.getByRole("option", { name: /\/goal/ });
    expect(option).toHaveAttribute("aria-disabled", "true");
    expect(option).toHaveTextContent("Available once this chat's Goal is refreshed");
    expect(controlGoal).not.toHaveBeenCalled();
  });

  it("says absence is not current rather than showing no Goal", () => {
    const { app } = setup(session());
    act(() =>
      writeSessionGoalRead(app.stores.session, {
        kind: "incomplete",
        sessionId: "session",
        nodeDid: NODE,
      }),
    );
    expect(screen.getByTestId("goal-unavailable")).toHaveTextContent(
      "Goal not currentThis chat's Goal could not be refreshed from its runtime. When last read, it had no Goal.",
    );
  });
});
