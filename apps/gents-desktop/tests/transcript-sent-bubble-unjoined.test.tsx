import { act, render, screen, within } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { DesktopClientSnapshot } from "@source-inc/gents-desktop-client";

vi.mock("../src/ui/screens/Markdown", () => ({
  CopyButton: () => null,
  Markdown: ({ children }: { children: string }) => <div>{children}</div>,
}));

const rowsView = vi.hoisted(() => ({
  mode: "joined" as "joined" | "apart" | "denied",
}));
vi.mock("../src/ui/lib/useHeldRows", async (original) => {
  const actual = await original<typeof import("../src/ui/lib/useHeldRows")>();
  return {
    useHeldRows: (...args: Parameters<typeof actual.useHeldRows>) => {
      const rows = actual.useHeldRows(...args);
      if (rowsView.mode === "apart") return { ...rows, joined: false };
      if (rowsView.mode === "denied")
        return {
          ...rows,
          failure: {
            message: "This conversation is not shared with this device.",
            denied: true,
          },
        };
      return rows;
    },
  };
});

import { TranscriptPanel } from "../src/ui/screens/Transcript";
import { ChatDeliveryFeedback } from "../src/ui/screens/session/ChatDeliveryFeedback";
import { NO_PARENT } from "../src/ui/screens/parentWork";
import { NO_WORKERS } from "../src/ui/screens/workers";
import { useSelectedSession } from "../src/ui/hooks/useSelectedSession";
import { chatDraftOf, saveChatDraft } from "../src/hooks/chatDrafts";
import { node, testApp, withApp } from "./app-fixture";
import { goalDeliveryStorage } from "./goal-delivery-storage";
import { sessionSnapshot } from "./timeline-fixture";

beforeEach(() => vi.stubGlobal("localStorage", goalDeliveryStorage()));
afterEach(() => {
  rowsView.mode = "joined";
  vi.unstubAllGlobals();
});

const NODE = node().nodeDid;
const SESSION = "session-open";

function Screen() {
  return (
    <>
      <TranscriptPanel
        inFlight={false}
        scroller={null}
        workers={NO_WORKERS}
        parentWork={NO_PARENT}
        workerActions={{ interrupt: () => {} }}
        session={useSelectedSession()}
      />
      <div data-testid="composer-feedback">
        <ChatDeliveryFeedback />
      </div>
    </>
  );
}

function held(failed: boolean) {
  const app = testApp({
    api: { fetchSessionSnapshot: vi.fn().mockResolvedValue(null) },
    deployments: [node()],
    session: sessionSnapshot({ sessionId: SESSION, nodeDid: NODE }),
  });
  app.stores.client.setState((state) => ({
    snapshot: {
      ...state.snapshot!,
      client: { ...state.snapshot!.client!, clientDid: "client" },
    } as DesktopClientSnapshot,
  }));
  render(<Screen />, { wrapper: withApp(app) });
  const draft = chatDraftOf(
    {
      clientDid: "client",
      nodeDid: NODE,
      requestId: "sent-request",
      sessionId: SESSION,
      agentDid: "default",
      content: "Sent from here",
      cwd: null,
    },
    SESSION,
  );
  act(() =>
    saveChatDraft(
      app.stores.chat,
      failed
        ? {
            ...draft,
            failure: { message: "The runtime connection was lost", error: null },
          }
        : { ...draft, pendingUntil: Date.now() + 60_000 },
    ),
  );
  return app;
}

const transcript = () => screen.queryByTestId("transcript-panel");
const feedback = () => screen.getByTestId("composer-feedback");

it.each(["apart", "denied"] as const)(
  "shows a failed send's Retry and Discard once, by the composer, while the transcript's rows are %s",
  (mode) => {
    rowsView.mode = mode;
    held(true);
    expect(screen.getAllByRole("button", { name: "Retry message" })).toHaveLength(1);
    expect(screen.getAllByRole("button", { name: "Discard message" })).toHaveLength(1);
    within(feedback()).getByRole("button", { name: "Retry message" });
    within(feedback()).getByText("The message wasn't sent");
    if (transcript())
      expect(within(transcript()!).queryByText("Sent from here")).toBeNull();
  },
);

it.each(["apart", "denied"] as const)(
  "shows a pending send once, by the composer, while the transcript's rows are %s",
  (mode) => {
    rowsView.mode = mode;
    held(false);
    expect(screen.getAllByRole("status", { name: "Saved message" })).toHaveLength(1);
    within(feedback()).getByText("Sending the message…");
  },
);

it("moves a failed send into its transcript once the rows join, and back when they part", () => {
  rowsView.mode = "joined";
  const app = held(true);
  expect(screen.getAllByRole("button", { name: "Retry message" })).toHaveLength(1);
  within(transcript()!).getByRole("button", { name: "Retry message" });
  expect(
    within(feedback()).queryByRole("button", { name: "Retry message" }),
  ).toBeNull();
  rowsView.mode = "apart";
  act(() => app.stores.chat.setState({ sendDeadlineTick: Date.now() }));
  expect(screen.getAllByRole("button", { name: "Retry message" })).toHaveLength(1);
  within(feedback()).getByRole("button", { name: "Retry message" });
});
