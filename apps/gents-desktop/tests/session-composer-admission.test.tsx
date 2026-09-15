import { act, fireEvent, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import type { Shell } from "../src/ui/hooks/useShell";
import { SessionScreen } from "../src/ui/screens/SessionScreen";
import {
  projectDeploymentOperationalState,
  type DeploymentView,
} from "@source-inc/gents-desktop-client";
import { projectChatShell } from "@source-inc/gents-desktop-chat";
import { useDesktopChatProjectionState } from "../src/hooks/useDesktopChatProjectionState";

const navigate = vi.hoisted(() => vi.fn());
const markdownRender = vi.hoisted(() => vi.fn());
vi.mock("../src/ui/screens/Markdown", () => ({
  CopyButton: () => null,
  Markdown: ({ children }: { children: string }) => {
    markdownRender(children);
    return <div>{children}</div>;
  },
}));
vi.mock("@/lib/router", async (importOriginal) => ({
  ...(await importOriginal<typeof import("../src/ui/lib/router")>()),
  navigate,
}));

vi.stubGlobal(
  "IntersectionObserver",
  class {
    observe() {}
    disconnect() {}
  },
);

vi.mock("../src/ui/screens/BehaviorPicker", () => ({
  BehaviorPicker: () => null,
}));

function newSessionShell(
  status: Shell["nonEmptyContentSendStatus"],
  sendMessage = vi.fn().mockResolvedValue(null),
): Shell {
  let intentGeneration = 0;
  return {
    selectedSession: null,
    selectedSessionId: null,
    selectedBehaviorId: "behavior",
    selectedAgentDid: "did:key:agent",
    selectedDeployment: {
      agentDid: "did:key:agent",
      agentPrincipal: { displayName: "Agent" },
      behaviors: [{ behaviorId: "behavior", isDefault: true }],
      behaviorEnvironments: [],
      contexts: [],
      skills: [],
    },
    mailboxCause: null,
    sending: false,
    error: null,
    activityStatus: null,
    nonEmptyContentSendStatus: status,
    sendMessage,
    captureComposeIntent: () => intentGeneration,
    acceptsComposeIntent: (captured: number) => captured === intentGeneration,
    advanceComposeIntentForTest: () => {
      intentGeneration += 1;
    },
    selectBehavior: vi.fn(),
  } as unknown as Shell;
}

function existingSessionShell(status: Shell["nonEmptyContentSendStatus"]): Shell {
  return {
    ...newSessionShell(status),
    selectedSessionId: "session",
    selectedSession: {
      sessionId: "session",
      agentDid: "did:key:agent",
      behaviorId: "behavior",
      title: "Session",
      turnState: status.kind === "ready" ? "interrupted" : "streaming",
      timelineItems: [],
      timelinePage: { hasOlder: false },
      latestRequestId: "request",
      latestResponse: null,
      goal: null,
      context: null,
    },
    holds: [],
    interruptVisible: false,
    selectedTrackedRequestId: null,
    activeRequestId: null,
    sessionLoad: { phase: "ready" },
    loadOlderSessionTimeline: vi.fn(),
    retryMessage: vi.fn(),
    forkSession: vi.fn(),
    refreshSnapshot: vi.fn(),
    api: {},
  } as unknown as Shell;
}

// Exercise the real context-keyed draft owner as well as the real kit Composer.
function OwnedSessionScreen({ shell }: { shell: Shell }) {
  const { draft, setDraft } = useDesktopChatProjectionState({
    clientAvailable: true,
    selectedAgentDid: shell.selectedAgentDid,
    selectedBehaviorId: shell.selectedBehaviorId,
    selectedSessionId: shell.selectedSessionId,
    selectedSessionSummary: null,
    selectedDeployment: null,
    sending: false,
    session: null,
    syncHealth: null,
  });
  return <SessionScreen shell={{ ...shell, draft, setDraft }} />;
}

describe("SessionScreen canonical composer admission", () => {
  it("keeps transcript markdown out of real context-owned draft updates", async () => {
    const shell = existingSessionShell({ kind: "ready" });
    shell.selectedSession!.timelineItems = [
      {
        kind: "assistantMessage",
        itemKey: "assistant-stable",
        sequence: 1,
        content: "Stable transcript",
        reasoning: null,
        timestamp: null,
      },
    ];
    markdownRender.mockClear();
    render(<OwnedSessionScreen shell={shell} />);
    expect(markdownRender).toHaveBeenCalledTimes(1);
    await userEvent.type(screen.getByLabelText("Message"), "a real draft");
    expect(screen.getByLabelText("Message")).toHaveValue("a real draft");
    expect(markdownRender).toHaveBeenCalledTimes(1);
  });

  it("does not erase text edited while the prior draft is being accepted", async () => {
    let resolve!: (result: { sessionId: string; requestId: string }) => void;
    const pending = new Promise<{ sessionId: string; requestId: string }>((done) => {
      resolve = done;
    });
    const shell = {
      ...existingSessionShell({ kind: "ready" }),
      sendMessage: vi.fn(() => pending),
    } as Shell;
    render(<OwnedSessionScreen shell={shell} />);
    fireEvent.change(screen.getByLabelText("Message"), {
      target: { value: "send this" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    fireEvent.change(screen.getByLabelText("Message"), {
      target: { value: "keep this next message" },
    });
    await act(async () => {
      resolve({ sessionId: "session", requestId: "accepted" });
      await pending;
    });
    expect(screen.getByLabelText("Message")).toHaveValue("keep this next message");
  });
  it("restores each session draft without carrying text into another session", () => {
    const first = existingSessionShell({ kind: "ready" });
    const second = { ...first, selectedSessionId: "second" };
    const { rerender } = render(<OwnedSessionScreen shell={first} />);
    fireEvent.change(screen.getByLabelText("Message"), {
      target: { value: "first draft" },
    });
    rerender(<OwnedSessionScreen shell={second} />);
    expect(screen.getByLabelText("Message")).toHaveValue("");
    fireEvent.change(screen.getByLabelText("Message"), {
      target: { value: "second draft" },
    });
    rerender(<OwnedSessionScreen shell={first} />);
    expect(screen.getByLabelText("Message")).toHaveValue("first draft");
    rerender(
      <OwnedSessionScreen shell={{ ...first, selectedAgentDid: "other-agent" }} />,
    );
    expect(screen.getByLabelText("Message")).toHaveValue("");
    rerender(<OwnedSessionScreen shell={second} />);
    expect(screen.getByLabelText("Message")).toHaveValue("second draft");
  });
  it("uses the selected non-default behavior decision, never the default or retry path", () => {
    const selected = (defaultReady: boolean, selectedReady: boolean) => {
      const deployment = {
        agentDid: "did:key:agent",
        dialSucceeded: true,
        chatSafe: true,
        agentPrincipal: { agentDid: "did:key:agent", defaultBehaviorId: "default" },
        behaviors: [
          {
            behaviorId: "default",
            displayName: "Default",
            enabled: true,
            isDefault: true,
          },
          {
            behaviorId: "selected",
            displayName: "Selected",
            enabled: true,
            isDefault: false,
          },
        ],
        behaviorReadiness: {
          source: { state: "current" },
          activeGeneration: 1,
          routerGeneration: 1,
          updatedAt: "2026-09-15T00:00:00Z",
          behaviors: [
            defaultReady
              ? { state: "ready", behaviorId: "default" }
              : {
                  state: "unavailable",
                  behaviorId: "default",
                  reason: "backend_disabled",
                },
            selectedReady
              ? { state: "ready", behaviorId: "selected" }
              : {
                  state: "unavailable",
                  behaviorId: "selected",
                  reason: "backend_disabled",
                },
          ],
        },
        sessions: [],
      } as DeploymentView;
      return projectChatShell({
        clientAvailable: true,
        selectedAgentDid: deployment.agentDid,
        selectedSessionId: null,
        draft: "",
        sending: false,
        session: null,
        selectedSessionSummary: null,
        localWorkflow: { kind: "ready" },
        operationalState: projectDeploymentOperationalState(
          deployment,
          "selected",
          null,
        ),
      }).nonEmptyContentSendStatus;
    };

    expect(selected(false, true)).toEqual({ kind: "ready" });
    expect(selected(true, false)).toMatchObject({
      kind: "disabled",
      reason: "behaviorUnavailable",
    });
  });

  it("keeps an empty composer editable, then submits when canonical admission is ready", async () => {
    const user = userEvent.setup();
    const sendMessage = vi.fn().mockResolvedValue(null);
    render(
      <OwnedSessionScreen shell={newSessionShell({ kind: "ready" }, sendMessage)} />,
    );

    const message = screen.getByRole("textbox", { name: "Message" });
    const send = screen.getByRole("button", { name: "Send" });
    expect(message).toBeEnabled();
    expect(send).toBeDisabled();
    await user.type(message, "hello");
    expect(send).toBeEnabled();
    await user.click(send);
    expect(sendMessage).toHaveBeenCalledWith("hello", "behavior");
  });

  it("clears accepted text only in its origin after navigating away", async () => {
    navigate.mockClear();
    let resolve!: (value: { sessionId: string; requestId: string }) => void;
    const sendMessage = vi.fn(
      () =>
        new Promise<{ sessionId: string; requestId: string }>((next) => {
          resolve = next;
        }),
    );
    const origin = newSessionShell({ kind: "ready" }, sendMessage) as Shell & {
      advanceComposeIntentForTest: () => void;
    };
    const other = { ...origin, selectedAgentDid: "other-agent" };
    const { rerender } = render(<OwnedSessionScreen shell={origin} />);
    fireEvent.change(screen.getByLabelText("Message"), {
      target: { value: "keep this draft" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Send" }));
    origin.advanceComposeIntentForTest();
    rerender(<OwnedSessionScreen shell={other} />);
    fireEvent.change(screen.getByLabelText("Message"), {
      target: { value: "unrelated draft" },
    });
    await act(async () => {
      resolve({ sessionId: "old-session", requestId: "old-request" });
      await Promise.resolve();
    });

    expect(sendMessage).toHaveBeenCalledOnce();
    expect(screen.getByLabelText("Message")).toHaveValue("unrelated draft");
    expect(navigate).not.toHaveBeenCalled();
    rerender(<OwnedSessionScreen shell={origin} />);
    expect(screen.getByLabelText("Message")).toHaveValue("");
  });

  it("preserves a canonical blocker for a non-empty local draft", () => {
    const sendMessage = vi.fn().mockResolvedValue(null);
    render(
      <OwnedSessionScreen
        shell={newSessionShell(
          {
            kind: "disabled",
            reason: "routeNotReady",
            hint: "Secure route to the agent is not ready",
          },
          sendMessage,
        )}
      />,
    );

    fireEvent.change(screen.getByLabelText("Message"), { target: { value: "hello" } });
    const send = screen.getByRole("button", { name: "Send" });
    expect(screen.getByRole("textbox", { name: "Message" })).toBeDisabled();
    expect(send).toBeDisabled();
    expect(screen.getByLabelText("Message")).toHaveAttribute(
      "placeholder",
      "Secure route to the agent is not ready",
    );
    fireEvent.click(send);
    expect(sendMessage).not.toHaveBeenCalled();
  });

  it("re-enables the rendered existing-session composer after terminal interruption", () => {
    const blocked = {
      kind: "disabled",
      reason: "awaitingTurnTerminality",
      hint: "Turn still streaming",
    } as const;
    const { rerender } = render(
      <OwnedSessionScreen shell={existingSessionShell(blocked)} />,
    );
    fireEvent.change(screen.getByLabelText("Message"), {
      target: { value: "follow up" },
    });
    expect(screen.getByRole("button", { name: "Send" })).toBeDisabled();

    rerender(<OwnedSessionScreen shell={existingSessionShell({ kind: "ready" })} />);
    expect(screen.getByRole("button", { name: "Send" })).toBeEnabled();
  });
});
