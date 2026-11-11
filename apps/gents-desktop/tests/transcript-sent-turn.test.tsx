import { act, cleanup, screen, waitFor } from "@testing-library/react";
import { useState } from "react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type {
  DesktopSessionSnapshot,
  RenderedTimelineItem,
  SessionRowsView,
} from "@source-inc/gents-desktop-client";

vi.mock("../src/ui/screens/Markdown", () => ({
  CopyButton: () => null,
  Markdown: ({ children }: { children: string }) => <div>{children}</div>,
}));

import type { DesktopApp } from "../src/hooks/desktopApp";
import { chat } from "../src/hooks/chatStore";
import { resetSessionViews, sessionViews } from "../src/ui/app/sessionViews";
import { useSelectedSession } from "../src/ui/hooks/useSelectedSession";
import { TranscriptPanel } from "../src/ui/screens/Transcript";
import { useSentTurnKey } from "../src/ui/screens/session/OpenSession";
import { NO_PARENT } from "../src/ui/screens/parentWork";
import { NO_WORKERS } from "../src/ui/screens/workers";
import { renderIn, testApp } from "./app-fixture";
import {
  assistantMessage,
  sessionContext,
  sessionSnapshot,
  userMessage,
} from "./timeline-fixture";

const SESSION = "session-sent";

/* a scroller jsdom can hold a position in: it lays nothing out */
function scrollable(element: HTMLDivElement) {
  let top = 0;
  Object.defineProperties(element, {
    scrollTop: {
      configurable: true,
      get: () => top,
      set: (next: number) => {
        top = next;
      },
    },
    clientHeight: { configurable: true, get: () => 600 },
    scrollHeight: { configurable: true, get: () => 4_000 },
  });
}

/* the session screen's wiring: the turn to go to after a send, taken from
   the app's sent messages, for the transcript of the selected session. The
   transcript is drawn into a scroller already there, as when a session
   opens with its rows at hand, so it opens and takes a turn in one pass */
function OpenTranscript() {
  const session = useSelectedSession();
  const sentTurn = useSentTurnKey(
    session?.sessionId ?? null,
    session?.latestRequestId ?? null,
  );
  const [scroller, setScroller] = useState<HTMLDivElement | null>(null);
  return (
    <div
      data-testid="scroller"
      ref={(element) => {
        if (element && element !== scroller) {
          scrollable(element);
          setScroller(element);
        }
      }}
    >
      {scroller && (
        <TranscriptPanel
          inFlight={false}
          scroller={scroller}
          session={session}
          workers={NO_WORKERS}
          parentWork={NO_PARENT}
          workerActions={{ interrupt: () => {} }}
          sentTurn={sentTurn}
          anchorSentTurn
        />
      )}
    </div>
  );
}

const exchange = (sequence: number, requestId: string): RenderedTimelineItem[] => [
  userMessage({
    itemKey: `user-${sequence}`,
    requestId,
    sequence,
    content: `Question ${sequence}`,
  }),
  assistantMessage({
    itemKey: `assistant-${sequence + 1}`,
    sequence: sequence + 1,
    content: `Answer ${sequence + 1}`,
  }),
];

function snapshot(
  items: RenderedTimelineItem[],
  latestRequestId: string,
  hasOlder = false,
): DesktopSessionSnapshot {
  return sessionSnapshot({
    sessionId: SESSION,
    nodeDid: "did:test:node",
    turnState: "completed",
    latestRequestId,
    context: sessionContext(),
    timelineItems: items,
    timelinePage: { hasOlder },
  } as Partial<DesktopSessionSnapshot>);
}

function send(app: DesktopApp, requestId: string, content: string) {
  chat.showPendingTurn(app.stores.chat, {
    sessionId: SESSION,
    requestId,
    content,
    selectedSkillIds: [],
    lifecycleState: "pending",
    foldedIntoRequestId: null,
    origin: null,
    createdAt: null,
  });
}

const takenOf = (app: DesktopApp, requestId: string) =>
  app.stores.chat.getState().sentTurns.find((turn) => turn.requestId === requestId)
    ?.taken ?? false;

/* the reader's place as the transcript keeps it after a scroll */
async function placeKeptAfterScroll() {
  const scroller = screen.getByTestId("scroller");
  await act(async () => {
    scroller.dispatchEvent(new Event("scroll"));
    await new Promise((resolve) => setTimeout(resolve, 350));
  });
  return sessionViews.placeOf(SESSION);
}

beforeEach(resetSessionViews);
afterEach(cleanup);

describe("the view goes to a sent turn once per send", () => {
  it("reopens a session at the reader's place, not at the turn sent before", async () => {
    const items = [
      ...exchange(0, "r-0"),
      ...exchange(2, "r-1"),
      ...exchange(4, "r-sent"),
    ];
    const app = testApp({ session: snapshot(items, "r-sent") });
    send(app, "r-sent", "Question 4");

    const first = renderIn(app, <OpenTranscript />);
    await waitFor(() => expect(takenOf(app, "r-sent")).toBe(true));
    expect((await placeKeptAfterScroll())?.key).toBe("turn:r-sent");
    first.unmount();

    /* the reader went up to an older message and left */
    sessionViews.setPlace(SESSION, { key: "assistant-1", offset: 40, sequence: 1 });

    renderIn(app, <OpenTranscript />);
    await screen.findByText("Answer 1");
    expect(screen.getByTestId("scroller").dataset.following).toBe("false");
    expect(await placeKeptAfterScroll()).toMatchObject({ key: "assistant-1" });
  });

  it("goes to a turn sent while the transcript is drawn", async () => {
    const items = [...exchange(0, "r-0"), ...exchange(2, "r-1")];
    const app = testApp({ session: snapshot(items, "r-1") });
    sessionViews.setPlace(SESSION, { key: "assistant-1", offset: 40, sequence: 1 });

    renderIn(app, <OpenTranscript />);
    await screen.findByText("Answer 1");
    expect(await placeKeptAfterScroll()).toMatchObject({ key: "assistant-1" });

    act(() => send(app, "r-new", "Question 4"));
    await screen.findByText("Question 4");
    await waitFor(() => expect(takenOf(app, "r-new")).toBe(true));
    expect((await placeKeptAfterScroll())?.key).toBe("turn:r-new");
  });

  it("keeps a reading place beyond the newest page after the turn was taken", async () => {
    const newest = [...exchange(10, "r-5"), ...exchange(12, "r-sent")];
    const older: SessionRowsView = {
      sessionId: SESSION,
      nodeDid: "did:test:node",
      rows: [...exchange(0, "r-0"), ...exchange(2, "r-1")],
      coveredFrom: 0,
      coveredThrough: 3,
      hasOlder: false,
      hasNewer: true,
      complete: true,
      unreadable: null,
      queryCount: 1,
    };
    const fetchSessionRows = vi.fn(async ({ window }: { window: { kind: string } }) =>
      window.kind === "around" ? older : null,
    );
    const app = testApp({
      api: { fetchSessionRows },
      session: snapshot(newest, "r-sent", true),
    });
    send(app, "r-sent", "Question 12");

    const first = renderIn(app, <OpenTranscript />);
    await waitFor(() => expect(takenOf(app, "r-sent")).toBe(true));
    first.unmount();

    sessionViews.setPlace(SESSION, { key: "assistant-1", offset: 40, sequence: 1 });
    renderIn(app, <OpenTranscript />);
    await screen.findByText("Answer 1");
    expect(fetchSessionRows).toHaveBeenCalledWith(
      expect.objectContaining({ window: { kind: "around", sequence: 1 } }),
    );
    await act(() => new Promise((resolve) => setTimeout(resolve, 50)));
    expect(screen.getByText("Answer 1")).toBeInTheDocument();
    expect(screen.queryByText("Answer 13")).not.toBeInTheDocument();
    expect(await placeKeptAfterScroll()).toMatchObject({ key: "assistant-1" });

    /* a send from there returns to the newest page and goes to its turn */
    act(() => send(app, "r-new", "Question 14"));
    await screen.findByText("Answer 13");
    await waitFor(() => expect(takenOf(app, "r-new")).toBe(true));
    expect((await placeKeptAfterScroll())?.key).toBe("turn:r-new");
  });
});
