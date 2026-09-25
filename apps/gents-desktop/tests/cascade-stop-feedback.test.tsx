import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import type {
  CascadeCancelPreview,
  InterruptRequestResult,
} from "@source-inc/gents-desktop-client";
import type { Shell } from "../src/ui/hooks/useShell";
import { CascadeDialog } from "../src/ui/screens/CascadeDialog";
import { activityStatus, isStopping } from "../src/ui/screens/activity-status";

const preview: CascadeCancelPreview = {
  rootRequestId: "req_root",
  previewSignature: "sig-1",
  rootState: "processing",
  willInterrupt: [
    {
      requestId: "req_child",
      lifecycleState: "processing",
      parentRequestId: "req_root",
      parentToolCallId: "tc_1",
      awaitMode: "background",
      cancelPolicy: "cascade",
      toolName: "summarize",
    },
  ],
  willDetach: [],
  alreadyTerminal: [],
  unknownPolicy: [],
};

function result(overrides: Partial<InterruptRequestResult>): InterruptRequestResult {
  return {
    accepted: false,
    alreadyInterrupted: false,
    stalePreview: false,
    preview: null,
    ...overrides,
  } as InterruptRequestResult;
}

function renderDialog(interrupt: () => Promise<InterruptRequestResult>) {
  const onStopRequested = vi.fn();
  const onFailure = vi.fn();
  const onClose = vi.fn();
  const shell = {
    selectedAgentDid: "did:key:agent",
    api: {
      previewInterruptCascade: vi.fn(async () => preview),
      interruptRequest: vi.fn(interrupt),
    },
  } as unknown as Shell;
  render(
    <CascadeDialog
      shell={shell}
      requestId="req_root"
      onClose={onClose}
      onStopRequested={onStopRequested}
      onFailure={onFailure}
    />,
  );
  return { onStopRequested, onFailure, onClose };
}

describe("stopping a request with children", () => {
  it("hands an accepted stop to the lifecycle-bound Stopping state instead of a notification", async () => {
    const run = renderDialog(async () => result({ accepted: true }));
    await userEvent.click(await screen.findByRole("button", { name: /Stop all/ }));

    await waitFor(() => expect(run.onStopRequested).toHaveBeenCalledWith("req_root"));
    expect(run.onFailure).not.toHaveBeenCalled();
    expect(run.onClose).toHaveBeenCalled();

    const stopping = isStopping({
      inFlight: true,
      requestId: "req_root",
      latestRequestId: "req_root",
      interruptObserved: false,
      requestedStop: "req_root",
    });
    expect(activityStatus([], stopping)).toBe("Stopping…");
    expect(
      isStopping({
        inFlight: false,
        requestId: "req_root",
        latestRequestId: "req_root",
        interruptObserved: true,
        requestedStop: "req_root",
      }),
    ).toBe(false);
  });

  it("treats a stop already in progress the same way", async () => {
    const run = renderDialog(async () => result({ alreadyInterrupted: true }));
    await userEvent.click(await screen.findByRole("button", { name: /Stop all/ }));
    await waitFor(() => expect(run.onStopRequested).toHaveBeenCalledWith("req_root"));
    expect(run.onFailure).not.toHaveBeenCalled();
  });

  it("says plainly when the stop fails or the request had already finished", async () => {
    const failed = renderDialog(async () => {
      throw new Error("the agent is not reachable");
    });
    await userEvent.click(await screen.findByRole("button", { name: /Stop all/ }));
    await waitFor(() =>
      expect(failed.onFailure).toHaveBeenCalledWith(
        "Couldn't stop: the agent is not reachable",
      ),
    );
    expect(failed.onStopRequested).not.toHaveBeenCalled();
  });

  it("reports a request that finished before the stop", async () => {
    const finished = renderDialog(async () => result({}));
    await userEvent.click(await screen.findByRole("button", { name: /Stop all/ }));
    await waitFor(() =>
      expect(finished.onFailure).toHaveBeenCalledWith(
        "This response had already finished.",
      ),
    );
    expect(finished.onStopRequested).not.toHaveBeenCalled();
  });
});
