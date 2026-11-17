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
  expect(screen.getByRole("region", { name: "Usage limits" })).toBeInTheDocument();
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
  const panel = screen.getByRole("region", { name: "Usage limits" });
  expect(panel.querySelector('[aria-busy="true"]')).not.toBeNull();
  expect(screen.queryByText("No account limits have been reported.")).toBeNull();
  rerender(<AllProviderLimits views={[]} onRefresh={vi.fn()} />);
  expect(panel.querySelector('[aria-busy="true"]')).toBeNull();
  expect(screen.getByText("No account limits have been reported.")).toBeInTheDocument();
});

/* A refusal is the provider's own statement that the account is out, so it must
   show above the meters — an exhausted account can have no window to fill. */
it("shows a provider refusal above the meters, with only the fields it stated", () => {
  const refusal = {
    at: new Date().toISOString(),
    window: "rolling 24-hour",
    used: 691_791,
    cap: 600_000,
    resetsAt: null,
    consumedByNode: 1200,
    detail: "you have used all the included free usage for this account",
    windowProvenance: "provider_reported",
    evidenceWindowHours: 24,
  };
  const views = [
    {
      label: "Grok",
      provider: "XaiGrokOAuth",
      backendIds: ["grok"],
      /* an exhausted account with no window at all: the percentages say
         nothing, which is exactly why the banner cannot be a percentage */
      windows: [],
      plan: null,
      note: "unknown",
      readAt: null,
      readError: null,
      read: "read",
      refusal,
    },
  ];
  render(<AllProviderLimits views={views} onRefresh={async () => {}} />);
  expect(screen.getByText("Out of quota")).toBeTruthy();
  /* the stated counter and cap, never a derived percentage */
  expect(screen.getByText(/691,791 of 600,000/)).toBeTruthy();
  expect(screen.queryByText("100%")).toBeNull();
  /* the bounded excerpt is what the provider said, not a computed figure */
  expect(
    screen.getByText(/you have used all the included free usage/),
  ).toBeTruthy();
});

it("never shows a window the provider did not state as its own", () => {
  /* xAI's text names "rolling 24-hour" -> reported. A provider that names
     none leaves the window null, and the node's own count says it is the
     node's, labelled "locally estimated". */
  const refusal = (provenance: string) => ({
    at: new Date().toISOString(),
    window: provenance === "provider_reported" ? "rolling 24-hour" : null,
    used: 691_791,
    cap: 600_000,
    resetsAt: null,
    consumedByNode: 1200,
    detail: "usage limit reached",
    windowProvenance: provenance,
    evidenceWindowHours: 24,
  });
  const view = (provenance: string) => [
    {
      label: "Grok",
      provider: "XaiGrokOAuth",
      backendIds: ["grok"],
      windows: [],
      plan: null,
      note: null,
      readAt: null,
      readError: null,
      read: "read",
      refusal: refusal(provenance),
    },
  ];

  const stated = render(
    <AllProviderLimits views={view("provider_reported")} onRefresh={async () => {}} />,
  );
  expect(screen.getByText(/rolling 24-hour window/)).toBeTruthy();
  /* the node's own count is always labelled as the node's */
  expect(screen.getByText(/1,200 counted on this node/)).toBeTruthy();
  stated.unmount();

  const unknown = render(
    <AllProviderLimits views={view("unknown")} onRefresh={async () => {}} />,
  );
  /* no window at all, so none is presented as the provider's */
  expect(screen.queryByText(/rolling 24-hour/)).toBeNull();
  expect(screen.queryByText(/, locally estimated/)).toBeNull();
  expect(screen.getByText(/1,200 counted on this node/)).toBeTruthy();
  unknown.unmount();
});

it("says the reset when the provider stated one, and that none was stated otherwise", () => {
  const refusal = (resetsAt: string | null) => ({
    at: new Date().toISOString(),
    window: null,
    used: null,
    cap: null,
    resetsAt,
    consumedByNode: null,
    detail: "free usage exhausted",
    windowProvenance: "unknown",
    evidenceWindowHours: 24,
  });
  const view = (resetsAt: string | null) => [
    {
      label: "Grok",
      provider: "XaiGrokOAuth",
      backendIds: [],
      windows: [],
      plan: null,
      note: null,
      readAt: null,
      readError: null,
      read: "read",
      refusal: refusal(resetsAt),
    },
  ];
  const { unmount } = render(
    <AllProviderLimits
      views={view(new Date(Date.now() + 3_600_000).toISOString())}
      onRefresh={async () => {}}
    />,
  );
  expect(screen.getByText(/Resets/)).toBeTruthy();
  unmount();
  render(<AllProviderLimits views={view(null)} onRefresh={async () => {}} />);
  expect(screen.getByText(/No reset stated/)).toBeTruthy();
});
