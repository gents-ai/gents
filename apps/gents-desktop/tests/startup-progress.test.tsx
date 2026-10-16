import { act, render, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { LoadingStepState } from "../src/lib/loadingStatus";
import {
  STEP_DWELL_MS,
  StartupProgress,
  type StartupProgressStep,
} from "../src/ui/screens/setup/StartupProgress";

const steps = (
  node: LoadingStepState,
  connections: LoadingStepState,
  client: LoadingStepState,
): StartupProgressStep[] => [
  {
    key: "node",
    label: "Start local node",
    doing: "Starting your local node",
    state: node,
  },
  {
    key: "connections",
    label: "Load saved connections",
    doing: "Reading saved connections",
    state: connections,
  },
  {
    key: "client",
    label: "Start secure client",
    doing: "Starting the secure client",
    state: client,
  },
];

const progress = (s: StartupProgressStep[]) => (
  <StartupProgress
    title="Starting Gents"
    steps={s}
    failedLabel="Saved connections could not be read"
    waitTestId="wait"
    loginItemsTestId="login-items"
  />
);

const shown = () => screen.getByTestId("startup-step");

describe("startup progress", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("shows only the step under way", () => {
    render(progress(steps("active", "pending", "pending")));
    expect(screen.getAllByTestId("startup-step")).toHaveLength(1);
    expect(shown()).toHaveTextContent("Starting your local node");
    expect(screen.queryByText("Reading saved connections")).toBeNull();
  });

  it("holds a step that finished fast until it has had its time", () => {
    const { rerender } = render(progress(steps("active", "pending", "pending")));
    rerender(progress(steps("complete", "active", "pending")));
    /* the finished step reads as done while it is held */
    expect(shown()).toHaveTextContent("Start local node");
    act(() => vi.advanceTimersByTime(STEP_DWELL_MS - 50));
    expect(shown()).toHaveTextContent("Start local node");
    act(() => vi.advanceTimersByTime(100));
    expect(shown()).toHaveTextContent("Reading saved connections");
  });

  it("shows a failure at once", () => {
    const { rerender } = render(progress(steps("active", "pending", "pending")));
    rerender(progress(steps("complete", "error", "pending")));
    expect(shown()).toHaveTextContent("Saved connections could not be read");
  });
});
