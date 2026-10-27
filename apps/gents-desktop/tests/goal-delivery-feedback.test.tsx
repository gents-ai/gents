import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type {
  GoalControlRequest,
  GoalSubmissionRecovery,
} from "@source-inc/gents-desktop-client";
import { GoalDeliveryFeedback } from "../src/ui/screens/session/GoalDeliveryFeedback";
import { retainGoalDraft, saveGoalDraft } from "../src/hooks/goalDrafts";
import { appliedGoalResult } from "./goal-delivery-fixture";
import { goalDeliveryStorage } from "./goal-delivery-storage";
import { loadGoalDrafts } from "../src/hooks/goalDrafts";
import { navigate } from "../src/ui/lib/router";
import { node, testApp, withApp } from "./app-fixture";

vi.mock("@/lib/router", async (original) => ({
  ...(await original<typeof import("../src/ui/lib/router")>()),
  navigate: vi.fn(),
}));
beforeEach(() => {
  vi.mocked(navigate).mockClear();
  vi.stubGlobal("localStorage", goalDeliveryStorage());
});
afterEach(() => vi.unstubAllGlobals());

function fixture() {
  const nodeDid = node().nodeDid;
  const request: GoalControlRequest = {
    clientDid: "client",
    nodeDid,
    commandId: "original-command",
    sessionId: "original-session",
    sessionDocId: null,
    expected: null,
    agentId: "default",
    action: { kind: "create", objective: "Original objective", tokenBudget: null },
    cwd: "/original",
  };
  const read = vi
    .fn<() => Promise<GoalSubmissionRecovery>>()
    .mockResolvedValue({ kind: "unconfirmed", nodeDid, commandId: "original-command" });
  const control = vi.fn(async (request: GoalControlRequest) =>
    appliedGoalResult(request),
  );
  const ack = vi.fn().mockRejectedValue(new Error("Lost acknowledgement"));
  const app = testApp({
    api: {
      recoverGoalSubmission: read,
      controlGoal: control,
      acknowledgeGoalSubmission: ack,
      fetchSessionSnapshot: vi.fn().mockResolvedValue(null),
    },
    deployments: [node()],
    selection: { nodeDid, sessionId: null },
  });
  app.stores.client.setState((state) => ({
    snapshot: {
      ...state.snapshot!,
      client: { ...state.snapshot!.client!, clientDid: "client" },
    },
  }));
  app.drafts.setState({ drafts: { newer: "Keep this newer draft" } });
  saveGoalDraft(app.stores.chat, retainGoalDraft(request, null));
  const view = render(<GoalDeliveryFeedback />, { wrapper: withApp(app) });
  return { app, request, read, control, ack, view };
}

it("checks without dispatch and explicitly resumes the retained Goal while preserving newer text", async () => {
  const f = fixture();
  const check = screen.getByRole("button", { name: "Check Goal delivery" });
  check.focus();
  expect(check).toHaveFocus();
  fireEvent.click(check);
  await waitFor(() => expect(f.read).toHaveBeenCalledTimes(1));
  expect(f.control).not.toHaveBeenCalled();
  await screen.findByText(
    "This change is not confirmed yet. Check again or resume the saved change.",
  );
  fireEvent.click(screen.getByRole("button", { name: "Resume saved Goal change" }));
  await screen.findByText("Goal change saved");
  expect(f.control).toHaveBeenCalledWith(f.request);
  expect(f.app.drafts.getState().drafts.newer).toBe("Keep this newer draft");
  expect(screen.queryByRole("button", { name: "Resume saved Goal change" })).toBeNull();
});

it("shows a verified refusal and dismisses only that saved change", async () => {
  const f = fixture();
  f.read.mockResolvedValue({
    kind: "prepared",
    receipt: appliedGoalResult(f.request).submissionReceipt!,
    acknowledged: false,
    outcome: { kind: "rejected", reason: "The Goal was replaced" },
  });
  f.ack.mockResolvedValue(undefined);
  fireEvent.click(screen.getByRole("button", { name: "Check Goal delivery" }));
  await screen.findByText("The Goal was replaced");
  expect(screen.queryByRole("button", { name: "Resume saved Goal change" })).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Dismiss saved change" }));
  await waitFor(() =>
    expect(screen.queryByRole("status", { name: "Saved Goal change" })).toBeNull(),
  );
  expect(f.control).not.toHaveBeenCalled();
  expect(f.app.drafts.getState().drafts.newer).toBe("Keep this newer draft");
});

it("disables recovery while the original call is still in flight", () => {
  const f = fixture();
  f.app.stores.chat.setState({ goalCommandsInFlight: new Set(["original-command"]) });
  return waitFor(() => {
    expect(screen.getByRole("button", { name: "Check Goal delivery" })).toBeDisabled();
    expect(
      screen.getByRole("button", { name: "Resume saved Goal change" }),
    ).toBeDisabled();
  });
});

it("retains a confirmed result after its recovery view closes without navigating", async () => {
  const f = fixture();
  let finish!: (value: GoalSubmissionRecovery) => void;
  f.read.mockImplementationOnce(
    () =>
      new Promise((resolve) => {
        finish = resolve;
      }),
  );
  f.ack.mockResolvedValue(undefined);
  fireEvent.click(screen.getByRole("button", { name: "Check Goal delivery" }));
  await waitFor(() => expect(f.read).toHaveBeenCalledTimes(1));
  f.view.unmount();
  finish({
    kind: "prepared",
    receipt: appliedGoalResult(f.request).submissionReceipt!,
    acknowledged: false,
    outcome: { kind: "applied", result: appliedGoalResult(f.request) },
  });
  await waitFor(() => expect(f.ack).toHaveBeenCalledTimes(1));
  expect(f.app.stores.selection.getState().sessionId).toBeNull();
  expect(navigate).not.toHaveBeenCalled();
  expect(loadGoalDrafts().goalDrafts[0].result).toEqual(appliedGoalResult(f.request));
  expect(f.app.drafts.getState().drafts.newer).toBe("Keep this newer draft");
});
