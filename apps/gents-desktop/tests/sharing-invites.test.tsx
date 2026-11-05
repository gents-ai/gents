import { act, cleanup, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { SharingAdapter } from "../src/hooks/sharing";
import { resetJoins } from "../src/ui/lib/joinInvite";
import { resetPreferences } from "../src/ui/preferences";
import { AgentScreen } from "../src/ui/screens/agent/AgentScreen";
import { resetSharingTab } from "../src/ui/screens/sharing/SharingTabs";
import { resetInstalledPacks } from "../src/ui/screens/packs/model";
import { node, renderIn, testApp, withFlags } from "./app-fixture";
import { bootstrap, deployment } from "./config-panel-wiring/fixtures";
import {
  HARNESS_GAME_PACK,
  createHarnessSharing,
  harnessInstalledPacks,
  type SharingScenario,
} from "./ui-harness/sharingHarness";

const DAY = 24 * 60 * 60;

function show(scenario: SharingScenario = "sharing") {
  withFlags({ sharing: true });
  const sharing = createHarnessSharing(scenario);
  const spied = {
    createInvite: vi.spyOn(sharing, "createInvite"),
    cancelInvite: vi.spyOn(sharing, "cancelInvite"),
    profiles: vi.spyOn(sharing, "profiles"),
  };
  const app = testApp({
    sharing,
    api: {
      listInstalledPacks: async () => ({ packs: harnessInstalledPacks("sharing") }),
    },
    snapshot: { bootstrap, client: { deployments: [node()] } },
  });
  renderIn(app, <AgentScreen nodeDid={deployment.nodeDid} section="sharing" />);
  return { sharing: sharing as SharingAdapter, ...spied };
}

async function choose(
  user: ReturnType<typeof userEvent.setup>,
  field: string,
  option: string,
) {
  const dialog = screen.getByTestId("invite-dialog");
  await user.click(within(dialog).getByRole("combobox", { name: field }));
  await user.click(await screen.findByRole("option", { name: option }));
}

beforeEach(() => resetInstalledPacks());
afterEach(() => {
  cleanup();
  resetJoins();
  resetSharingTab();
  resetPreferences();
});

describe("inviting to the node", () => {
  it("offers the whole node, each installed pack and each agent, with the profiles each allows", async () => {
    const user = userEvent.setup();
    const { profiles } = show();
    await user.click(await screen.findByRole("button", { name: "Invite…" }));
    const dialog = await screen.findByTestId("invite-dialog");
    expect(
      await within(dialog).findByText("Everything on this node, as you"),
    ).toBeTruthy();
    await user.click(within(dialog).getByRole("combobox", { name: "What they get" }));
    const options = (await screen.findAllByRole("option")).map((o) => o.textContent);
    expect(options).toEqual([
      "Whole node",
      "tic_tac_toe pack",
      "Default agent",
      "Ops agent",
    ]);
    await user.click(screen.getByRole("option", { name: "Ops agent" }));
    expect(
      await within(dialog).findByText("Start sessions with this agent"),
    ).toBeTruthy();
    expect(profiles).toHaveBeenLastCalledWith(expect.any(String), {
      kind: "agent",
      agent: "ops",
      name: "Ops",
    });
  });

  it("makes the invite chosen and shows its link, code and QR code", async () => {
    const user = userEvent.setup();
    const { createInvite } = show("sharing-empty");
    await user.click(await screen.findByRole("button", { name: "Invite…" }));
    const dialog = await screen.findByTestId("invite-dialog");
    await within(dialog).findByText("Everything on this node, as you");
    await choose(user, "What they get", "tic_tac_toe pack");
    await within(dialog).findByText("Make moves as themselves");
    await choose(user, "Profile", "Viewer");
    await choose(user, "Expires after", "7 days");
    await choose(user, "Can be used", "Any number of times");
    await user.click(
      within(dialog).getByRole("checkbox", { name: /Let them in without asking me/ }),
    );
    await user.click(within(dialog).getByRole("button", { name: "Make invite" }));
    expect(createInvite).toHaveBeenCalledWith(expect.any(String), {
      kind: "person",
      scope: { kind: "pack", pack: HARNESS_GAME_PACK, name: "tic_tac_toe" },
      profileId: "viewer",
      expiresInSeconds: 7 * DAY,
      uses: null,
      preApproved: true,
    });
    expect(await within(dialog).findByText("Invite ready")).toBeTruthy();
    expect(within(dialog).getByText("gents://invite/invite-harness-1")).toBeTruthy();
    expect(within(dialog).getByText("HRN-0001")).toBeTruthy();
    expect(within(dialog).getByRole("button", { name: "Copy the link" })).toBeTruthy();
    expect(within(dialog).getByRole("button", { name: "Copy the code" })).toBeTruthy();
    const qr = within(dialog).getByRole("img", { name: "QR code for the invite" });
    expect(qr.querySelector("path")?.getAttribute("d")).toMatch(/^M\d+ \d+h1v1h-1z/);
    await user.click(within(dialog).getByRole("button", { name: "Done" }));
    const open = await screen.findByRole("list", { name: "Open invites" });
    expect(within(open).getByText(/Any number of times/)).toBeTruthy();
    expect(within(open).getByText("Pre-approved")).toBeTruthy();
  });

  it("makes a device's invite for the whole node, once and needing approval, by default", async () => {
    const user = userEvent.setup();
    const { createInvite } = show("sharing-empty");
    await user.click(await screen.findByRole("button", { name: "Invite…" }));
    const dialog = await screen.findByTestId("invite-dialog");
    await within(dialog).findByText("Everything on this node, as you");
    await user.click(
      within(dialog).getByRole("radio", { name: /One of your devices/ }),
    );
    await user.click(within(dialog).getByRole("button", { name: "Make invite" }));
    expect(createInvite).toHaveBeenCalledWith(expect.any(String), {
      kind: "device",
      scope: { kind: "node" },
      profileId: "device",
      expiresInSeconds: DAY,
      uses: 1,
      preApproved: false,
    });
  });

  it("shows a refused invite in the dialog", async () => {
    const user = userEvent.setup();
    const { createInvite } = show("sharing-empty");
    createInvite.mockRejectedValueOnce(new Error("invites are paused"));
    await user.click(await screen.findByRole("button", { name: "Invite…" }));
    const dialog = await screen.findByTestId("invite-dialog");
    await within(dialog).findByText("Everything on this node, as you");
    await user.click(within(dialog).getByRole("button", { name: "Make invite" }));
    expect((await within(dialog).findByRole("alert")).textContent).toContain(
      "invites are paused",
    );
  });

  it("lists open invites with what they share, expiry and uses left, and cancels one", async () => {
    const user = userEvent.setup();
    const { cancelInvite } = show();
    await user.click(await screen.findByRole("tab", { name: /^Invites/ }));
    const open = await screen.findByRole("list", { name: "Open invites" });
    const game = within(open).getByRole("listitem", {
      name: "Invite: Tic-tac-toe pack",
    });
    expect(game.textContent).toContain("Player");
    expect(game.textContent).toContain("Expires in 3d");
    expect(game.textContent).toContain("2 uses left");
    const more = within(game).getByRole("button", {
      name: "More for the invite: Tic-tac-toe pack",
    });
    await user.click(more);
    await user.click(
      await screen.findByRole("menuitem", { name: "Show link and code" }),
    );
    expect(within(game).getByText("GAME-7Q4K")).toBeTruthy();
    /* the closing menu ignores the pointer until it is gone */
    await waitFor(() => expect(screen.queryByRole("menu")).toBeNull());
    await user.click(more);
    await user.click(await screen.findByRole("menuitem", { name: "Cancel invite" }));
    expect(cancelInvite).toHaveBeenCalledWith(
      expect.any(String),
      "invite-harness-game",
    );
    await waitFor(() =>
      expect(
        screen.queryByRole("listitem", { name: "Invite: Tic-tac-toe pack" }),
      ).toBeNull(),
    );
    expect(screen.getByRole("listitem", { name: "Invite: Coach agent" })).toBeTruthy();
  });

  it("says so in its tab when no invite is open", async () => {
    const user = userEvent.setup();
    const { sharing } = show();
    await user.click(await screen.findByRole("tab", { name: /^Invites/ }));
    await act(async () => {
      await sharing.cancelInvite("node", "invite-harness-game");
      await sharing.cancelInvite("node", "invite-harness-coach");
    });
    expect(await screen.findByText("No open invites.")).toBeTruthy();
  });

  it("offers no invite where the runtime can't make them, and says so", async () => {
    show("sharing-requests-only");
    expect(
      await screen.findByText(
        "Access, invites and history need a newer runtime on Local Agent.",
      ),
    ).toBeTruthy();
    expect(screen.queryByRole("button", { name: "Invite…" })).toBeNull();
  });
});
