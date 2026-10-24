import {
  act,
  cleanup,
  fireEvent,
  render,
  screen,
  waitFor,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ProviderRecoveryPlan } from "@source-inc/gents-desktop-client";
import {
  ProviderRecovery,
  resetDescription,
} from "../src/ui/screens/session/ProviderRecovery";

const api = vi.hoisted(() => ({
  fetchProviderRecovery: vi.fn(),
  applyProviderRecovery: vi.fn(),
}));
vi.mock("@/app/AppContext", () => ({ useApp: () => ({ api }) }));

function plan(overrides: Partial<ProviderRecoveryPlan> = {}): ProviderRecoveryPlan {
  return {
    reason: "usage_limit",
    accountLabel: "Work",
    profileId: "profile-main",
    affectedAgents: ["Engineer", "Reviewer"],
    resetsAt: null,
    candidates: [{ backendId: "backend-backup", label: "Backup" }],
    companionProfiles: ["compact", "summary"],
    switchCost: "The new account's prompt cache starts empty.",
    unavailableReason: null,
    goal: { goalId: "goal-1", status: "usage_limited", autoResumeAtReset: false },
    ...overrides,
  };
}
function mount() {
  return render(
    <ProviderRecovery
      nodeDid="node-1"
      sessionId="session-1"
      requestId="request-1"
      turnState="failed"
    />,
  );
}

beforeEach(() => {
  api.fetchProviderRecovery.mockReset().mockResolvedValue(plan());
  api.applyProviderRecovery
    .mockReset()
    .mockResolvedValue({ outcome: "applied", message: "Saved", requestId: null });
});
afterEach(cleanup);

describe("provider account recovery", () => {
  it.each([
    ["usage_limit", "has reached its usage limit"],
    ["account_disabled", "is disabled"],
    ["account_removed", "is no longer on this node"],
    ["account_signed_out", "needs to sign in again"],
  ])("shows the native %s state without switching", async (reason, text) => {
    api.fetchProviderRecovery.mockResolvedValue(plan({ reason }));
    mount();
    expect(await screen.findByText(`Work ${text}.`)).toBeInTheDocument();
    expect(api.applyProviderRecovery).not.toHaveBeenCalled();
  });

  it("shows unknown resets, affected Agents, cache cost, and a default-off Goal opt-in", async () => {
    mount();
    expect(await screen.findByText("Reset time not reported.")).toBeInTheDocument();
    expect(
      screen.getByText("Moving this profile affects 2 Agents."),
    ).toBeInTheDocument();
    expect(
      screen.getByText("The new account's prompt cache starts empty."),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("checkbox", { name: /Automatically resume/ }),
    ).not.toBeChecked();
    expect(
      screen.getByRole("checkbox", { name: /companion profiles: compact, summary/ }),
    ).not.toBeChecked();
  });

  it("moves the profile and named companion profiles exactly once per click", async () => {
    const user = userEvent.setup();
    let finish!: (result: unknown) => void;
    api.applyProviderRecovery.mockImplementation(
      () =>
        new Promise((resolve) => {
          finish = resolve;
        }),
    );
    mount();
    await screen.findByText(/Work has reached/);
    await user.click(
      screen.getByRole("checkbox", { name: /companion profiles: compact, summary/ }),
    );
    await user.dblClick(screen.getByRole("button", { name: "Move profile" }));
    expect(api.applyProviderRecovery).toHaveBeenCalledTimes(1);
    expect(api.applyProviderRecovery).toHaveBeenCalledWith({
      scope: { nodeDid: "node-1", sessionId: "session-1", requestId: "request-1" },
      action: {
        kind: "move_profile",
        profileId: "profile-main",
        backendId: "backend-backup",
        withCompaction: true,
      },
    });
    await act(async () => {
      finish({ outcome: "applied", message: "Moved", requestId: null });
    });
  });

  it("resumes a usage-limited Goal through the account recovery operation", async () => {
    mount();
    fireEvent.click(await screen.findByRole("button", { name: "Resume on Backup" }));
    await waitFor(() => expect(api.applyProviderRecovery).toHaveBeenCalledTimes(1));
    expect(api.applyProviderRecovery.mock.calls[0][0].action).toEqual({
      kind: "resume_goal",
      goalId: "goal-1",
      backendId: "backend-backup",
      withCompaction: false,
    });
  });

  it("keeps reset opt-in off after a native refusal and shows its reason", async () => {
    api.applyProviderRecovery.mockResolvedValue({
      outcome: "refused",
      reason: "The Goal changed.",
    });
    mount();
    const checkbox = await screen.findByRole("checkbox", {
      name: /Automatically resume/,
    });
    fireEvent.click(checkbox);
    expect(await screen.findByRole("alert")).toHaveTextContent("The Goal changed.");
    expect(checkbox).not.toBeChecked();
    expect(api.applyProviderRecovery.mock.calls[0][0].action).toEqual({
      kind: "set_auto_resume",
      goalId: "goal-1",
      enabled: true,
    });
  });

  it("refreshes a stale observation without retrying its action", async () => {
    api.applyProviderRecovery.mockResolvedValue({
      outcome: "stale",
      reason: "Refresh the stopped profile.",
    });
    mount();
    fireEvent.click(await screen.findByRole("button", { name: "Move profile" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Refresh the stopped profile.",
    );
    await waitFor(() => expect(api.fetchProviderRecovery).toHaveBeenCalledTimes(2));
    expect(api.applyProviderRecovery).toHaveBeenCalledTimes(1);
  });

  it("does not offer a move when no enabled account advertises the model", async () => {
    api.fetchProviderRecovery.mockResolvedValue(
      plan({ candidates: [], unavailableReason: "No eligible account." }),
    );
    mount();
    expect(await screen.findByText("No eligible account.")).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Move profile" }),
    ).not.toBeInTheDocument();
  });

  it("shows a countdown only for a reported future reset", () => {
    const now = Date.parse("2026-10-23T18:00:00Z");
    expect(resetDescription("2026-10-23T19:10:00Z", now)).toContain("Reset in 1h 10m");
    expect(resetDescription("2026-10-23T17:59:00Z", now)).toBe(
      "The reported reset time has passed.",
    );
    expect(resetDescription(null, now)).toBe("Reset time not reported.");
  });
});
