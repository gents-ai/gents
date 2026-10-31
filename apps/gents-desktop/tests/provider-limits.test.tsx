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
  expect(screen.getByRole("region", { name: "Quota windows" })).toBeInTheDocument();
  expect(screen.getByRole("region", { name: "Request limits" })).toBeInTheDocument();
  expect(screen.queryByText("Quota windows")).toBeNull();
  expect(refresh).not.toHaveBeenCalled();
  await userEvent.click(screen.getByRole("button", { name: "Refresh all limits" }));
  expect(refresh).toHaveBeenCalledTimes(1);
});

it("draws each window as a meter, with its reset and source in a tooltip", () => {
  const now = Date.now();
  const views = [
    {
      label: "Claude",
      provider: "Claude",
      backendIds: ["claude"],
      windows: [
        {
          label: "5h",
          windowMinutes: 300,
          usedPct: 64.4,
          resetsAt: new Date(now + 24 * 60_000 + 30_000).toISOString(),
          source: "endpoint",
          observedAt: new Date(now - 2 * 60_000).toISOString(),
          lastKnown: false,
        },
        {
          label: "7d",
          windowMinutes: 10_080,
          usedPct: 93,
          resetsAt: null,
          source: "header",
          observedAt: new Date(now).toISOString(),
          lastKnown: true,
        },
      ],
      plan: null,
      note: null,
      readAt: null,
      readError: null,
      read: "read",
    },
  ];
  render(<AllProviderLimits views={views} onRefresh={vi.fn()} />);
  const hours = screen.getByRole("meter", { name: "5h used" });
  expect(hours).toHaveAttribute("aria-valuenow", "64");
  const row = hours.closest("li")!;
  expect(row).toHaveTextContent("64%");
  expect(row).toHaveTextContent("resets in 24m");
  expect(row.title).toMatch(/^Resets .+ · from the usage endpoint, 2m ago$/);
  const week = screen.getByRole("meter", { name: "7d used" });
  expect(week.firstElementChild).toHaveClass("bg-destructive");
  expect(week.closest("li")).toHaveTextContent("last known");
});

it("holds the panel's shape while the first read is out, not saying none were reported", () => {
  const { rerender } = render(
    <AllProviderLimits views={[]} loading onRefresh={vi.fn()} />,
  );
  const panel = screen.getByRole("region", { name: "Your limits" });
  expect(panel.querySelector('[aria-busy="true"]')).not.toBeNull();
  expect(screen.queryByText("No account limits have been reported.")).toBeNull();
  rerender(<AllProviderLimits views={[]} onRefresh={vi.fn()} />);
  expect(panel.querySelector('[aria-busy="true"]')).toBeNull();
  expect(screen.getByText("No account limits have been reported.")).toBeInTheDocument();
});
