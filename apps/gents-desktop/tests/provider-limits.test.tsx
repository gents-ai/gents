import { cleanup, render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, expect, it, vi } from "vitest";
import { AllProviderLimits } from "../src/ui/screens/agent/ProviderUsage";

afterEach(cleanup);

it("shows every account together, including an account with no configured backend", async () => {
  const refresh = vi.fn().mockResolvedValue(undefined);
  const observed = {
    windowMinutes: 300,
    usedPct: 40,
    resetsAt: null,
    source: "header",
    observedAt: new Date().toISOString(),
    lastKnown: false,
  };
  const views = [
    {
      label: "Work",
      provider: "Claude",
      backendIds: ["claude"],
      windows: [{ ...observed, label: "5h" }],
      plan: null,
      note: null,
      readAt: null,
      readError: null,
      read: "read",
    },
    {
      label: "Unbound personal",
      provider: "Codex",
      backendIds: [],
      windows: [{ ...observed, label: "requests" }],
      plan: null,
      note: null,
      readAt: null,
      readError: null,
      read: "skipped_recent",
    },
  ];
  render(<AllProviderLimits views={views} onRefresh={refresh} />);
  expect(screen.getByRole("region", { name: "Your limits" })).toBeInTheDocument();
  expect(screen.getByText("Work")).toBeInTheDocument();
  expect(screen.getByText("Unbound personal")).toBeInTheDocument();
  expect(screen.getByText("Quota windows")).toBeInTheDocument();
  expect(screen.getByText("Request limits")).toBeInTheDocument();
  expect(refresh).not.toHaveBeenCalled();
  await userEvent.click(screen.getByRole("button", { name: "Refresh all limits" }));
  expect(refresh).toHaveBeenCalledTimes(1);
});
