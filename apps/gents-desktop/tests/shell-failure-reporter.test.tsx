import { render } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type { DesktopBridge } from "../src/hooks/desktopApp";

const toast = vi.hoisted(() => vi.fn());
vi.mock("sonner", async (original) => ({
  ...(await original<typeof import("sonner")>()),
  toast,
}));

const received = vi.hoisted(() => ({ reportFailure: undefined as unknown }));
vi.mock("../src/hooks/desktopApp", async (original) => {
  const actual = await original<typeof import("../src/hooks/desktopApp")>();
  return {
    ...actual,
    createDesktopApp: (params: Parameters<typeof actual.createDesktopApp>[0]) => {
      received.reportFailure = params.reportFailure;
      return actual.createDesktopApp(params);
    },
  };
});

import App from "../src/App";

describe("the app's failure reporter", () => {
  it("is the app's toast", () => {
    const bridge = {
      api: { fetchDesktopSnapshot: () => new Promise(() => {}) },
      listenToUpdates: async () => () => {},
    } as unknown as DesktopBridge;
    render(<App bridge={bridge} />);
    expect(received.reportFailure).toBe(toast);
  });
});
