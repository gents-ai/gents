import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type { DesktopSessionSnapshot } from "@source-inc/gents-desktop-client";
import { MemoryNavProvider } from "@gents/shell";
import { SessionScreen } from "../src/ui/screens/SessionScreen";
import { chat } from "../src/hooks/chatStore";
import { node, testApp, withApp } from "./app-fixture";
import { deployment } from "./config-panel-wiring/fixtures";
import { composerText } from "./composer-field";

vi.mock("sonner", () => ({ toast: vi.fn() }));
vi.mock("../src/ui/screens/Markdown", () => ({
  CopyButton: () => null,
  Markdown: ({ children }: { children: string }) => <div>{children}</div>,
}));
vi.mock("../src/ui/screens/BehaviorPicker", () => ({ AgentPicker: () => null }));
beforeEach(() => {
  vi.stubGlobal(
    "IntersectionObserver",
    class {
      observe() {}
      disconnect() {}
    },
  );
});

const NODE = deployment.nodeDid;

/* the session as the node last answered it: the first message, its turn done */
const held = {
  sessionId: "session",
  nodeDid: NODE,
  agentDid: "default",
  title: "Session",
  turnState: "completed",
  timelineItems: [
    {
      kind: "userMessage",
      itemKey: "user:first",
      requestId: "first",
      inputRequestId: "first",
      ownsTurn: true,
      sequence: 1,
      content: "first message",
      selectedSkillIds: [],
      timestamp: null,
    },
  ],
  timelinePage: { hasOlder: false },
  latestRequestId: "first",
  latestRequestOutcome: null,
  goal: null,
  context: null,
  queuedTurns: [],
  pendingQueue: [],
  foldedInputs: [],
} as unknown as DesktopSessionSnapshot;

describe("↑ right after a send", () => {
  /* the transcript draws a message the moment it is sent; the session's read
     brings it later, and over a slow link ↑ can come first */
  it("brings back the message just sent, before the session's read holds it", async () => {
    const user = userEvent.setup();
    const app = testApp({
      api: {
        fetchSessionSnapshot: vi.fn().mockResolvedValue(held),
        sessionProvenance: vi.fn().mockResolvedValue(null),
        fetchOperationsSnapshot: vi.fn().mockResolvedValue(null),
      },
      deployments: [node({})],
      session: held,
      selection: { nodeDid: NODE, sessionId: "session" },
    });
    chat.showPendingTurn(app.stores.chat, {
      sessionId: "session",
      requestId: "second",
      content: "second message",
      selectedSkillIds: [],
      lifecycleState: "pending",
      foldedIntoRequestId: null,
      origin: null,
      createdAt: null,
    });
    render(
      <MemoryNavProvider initial={{ name: "session", sessionId: "session" }}>
        <SessionScreen />
      </MemoryNavProvider>,
      { wrapper: withApp(app) },
    );
    const composer = within(screen.getByTestId("composer")).getByRole("textbox");
    await waitFor(() =>
      expect(screen.getByTestId("transcript-panel")).toHaveTextContent(
        "second message",
      ),
    );

    await user.click(composer);
    await user.keyboard("{ArrowUp}");

    expect(screen.getByTestId("composer-history")).toHaveTextContent("History 2/2");
    expect(composerText(composer)).toBe("second message");
    await user.keyboard("{ArrowUp}");
    expect(screen.getByTestId("composer-history")).toHaveTextContent("History 1/2");
    expect(composerText(composer)).toBe("first message");
  });
});
