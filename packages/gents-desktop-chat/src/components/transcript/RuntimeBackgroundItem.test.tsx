// @vitest-environment jsdom
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { afterEach, expect, it } from "vitest";
import type { RenderedTimelineItem } from "@source-inc/gents-desktop-client";
import { UserMessageItem } from "./MessageItems.js";
import { MessageList } from "../Transcript.js";

afterEach(cleanup);
const envelope =
  '<tool-completion tool_call_id="spawned:fixture" status="failed"><result>failure</result></tool-completion>';
const background: Extract<RenderedTimelineItem, { kind: "automatedInput" }> = {
  kind: "automatedInput",
  itemKey: "background",
  requestId: "request",
  inputRequestId: null,
  sequence: 1,
  origin: { kind: "backgroundCompletion" },
  content: envelope,
  timestamp: null,
  reconstruction: { state: "ready" },
};

it("renders background output as collapsed activity, not a user bubble", async () => {
  const { container } = render(<UserMessageItem item={background} />);
  expect(screen.getByText("Background activity")).toBeTruthy();
  expect(screen.queryByText(envelope)).toBeNull();
  expect(container.querySelector(".user-card")).toBeNull();
  fireEvent.click(screen.getByText("Background activity"));
  expect(await screen.findByText(envelope)).toBeTruthy();
});

it("does not expose stale output when reconstruction is denied", async () => {
  render(
    <UserMessageItem
      item={{ ...background, reconstruction: { state: "denied" } }}
    />,
  );
  fireEvent.click(screen.getByText("Background activity"));
  expect(screen.queryByText(envelope)).toBeNull();
  expect(
    await screen.findByText("Background details are unavailable."),
  ).toBeTruthy();
});

it("keeps expanded background details open while they load", async () => {
  const view = render(
    <UserMessageItem
      item={{ ...background, reconstruction: { state: "loading" } }}
    />,
  );
  fireEvent.click(screen.getByText("Background activity"));
  expect((await screen.findByRole("status")).textContent).toBe(
    "Loading background details…",
  );
  expect(screen.queryByText("Background details are unavailable.")).toBeNull();
  view.rerender(<UserMessageItem item={background} />);
  expect(await screen.findByText(envelope)).toBeTruthy();
  expect(screen.getByText("Background activity").closest("details")?.open).toBe(
    true,
  );
});

it("omits internal Goal continuations but preserves user-authored lookalikes", () => {
  const goal: RenderedTimelineItem = {
    ...background,
    itemKey: "goal",
    origin: {
      kind: "goalContinuation",
      goalId: null,
      sequence: null,
      wrapup: false,
    },
    content: "Internal continuation instructions",
  };
  const user: RenderedTimelineItem = {
    kind: "userMessage",
    itemKey: "user",
    requestId: "request-user",
    sequence: 3,
    content: envelope,
    timestamp: null,
    reconstruction: { state: "ready" },
  };
  const pendingGoal: RenderedTimelineItem = {
    kind: "pendingUserTurn",
    itemKey: "pending-goal",
    requestId: "pending-goal",
    content: "Internal pending goal",
    selectedSkillIds: [],
    lifecycleState: "processing",
    foldedIntoRequestId: null,
    origin: {
      kind: "goalContinuation",
      goalId: null,
      sequence: null,
      wrapup: false,
    },
    createdAt: null,
  };
  const pendingWake: RenderedTimelineItem = {
    ...pendingGoal,
    itemKey: "pending-wake",
    requestId: "pending-wake",
    content: "Internal background wake",
    origin: { kind: "backgroundCompletion" },
  };
  render(
    <MessageList
      timelineItems={[background, goal, pendingGoal, pendingWake, user]}
    />,
  );
  expect(screen.queryByText("Internal pending goal")).toBeNull();
  expect(screen.queryByText("Internal background wake")).toBeNull();
  expect(screen.queryByText("Internal continuation instructions")).toBeNull();
  expect(screen.getAllByText(envelope)).toHaveLength(1);
  expect(screen.getByText("Background activity")).toBeTruthy();
});

it("keeps internal pending input out of the chat through its durable handoff and remount", () => {
  const pending: RenderedTimelineItem = {
    kind: "pendingUserTurn",
    itemKey: "pending",
    requestId: "wake",
    content: "Review the background results",
    selectedSkillIds: [],
    lifecycleState: "processing",
    foldedIntoRequestId: null,
    origin: { kind: "backgroundCompletion" },
    createdAt: null,
  };
  const view = render(<MessageList timelineItems={[pending]} />);
  expect(screen.queryByText("Review the background results")).toBeNull();
  view.rerender(<MessageList timelineItems={[background]} />);
  expect(screen.queryByText(envelope)).toBeNull();
  expect(screen.getByText("Background activity")).toBeTruthy();
  view.unmount();
  render(<MessageList timelineItems={[background]} />);
  expect(screen.queryByText(envelope)).toBeNull();
  expect(screen.getByText("Background activity")).toBeTruthy();
});
