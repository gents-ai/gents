import { renderHook } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

const toast = vi.hoisted(() => vi.fn());
vi.mock("sonner", () => ({ toast }));

const received = vi.hoisted(() => ({ reportFailure: undefined as unknown }));
vi.mock("../src/hooks/useDesktopShell", () => ({
  useDesktopShell: (bridge: { reportFailure?: unknown }) => {
    received.reportFailure = bridge.reportFailure;
    return {
      deployments: [],
      selectedDeployment: null,
      behaviorReadiness: { behaviorId: null },
      followRoute: vi.fn(),
    };
  },
}));

import { useShell } from "../src/ui/hooks/useShell";

/* #2043: a failed action reaches the person as one toast, reported by the
   shell's action, with no state for the app to copy and clear */
describe("the shell's failure reporter", () => {
  it("is the app's toast", () => {
    renderHook(() =>
      useShell({ api: {}, listenToUpdates: vi.fn() } as never, undefined),
    );
    expect(received.reportFailure).toBe(toast);
  });
});
