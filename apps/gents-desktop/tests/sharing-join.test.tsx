import { act, cleanup, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { MemoryNavProvider, useNav, type Nav } from "@gents/shell";
import type { InstalledPack } from "@source-inc/gents-desktop-client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { resetJoins, startJoin } from "../src/ui/lib/joinInvite";
import { parseLink } from "../src/ui/lib/links";
import { bindHomeNode, bindNav, openLink } from "../src/ui/lib/router";
import { resetPreferences } from "../src/ui/preferences";
import { AgentScreen } from "../src/ui/screens/agent/AgentScreen";
import { resetInstalledPacks } from "../src/ui/screens/packs/model";
import { node, renderIn, testApp, withFlags } from "./app-fixture";
import { bootstrap, deployment } from "./config-panel-wiring/fixtures";
import { createDesktopUiHarness } from "./ui-harness/desktopHarness";
import {
  HARNESS_DEVICE_INVITE,
  HARNESS_GAME_PACK,
  HARNESS_INVITE,
  createHarnessSharing,
  seedHarnessJoining,
  type SharingScenario,
} from "./ui-harness/sharingHarness";

vi.mock("../src/ui/contrib/runtime-door", () => ({
  refreshRuntimePackUxPlugins: vi.fn(async () => {}),
}));
vi.mock("sonner", () => ({
  toast: Object.assign(vi.fn(), { error: vi.fn(), success: vi.fn() }),
}));

const DIGEST = `sha256:${"e".repeat(64)}`;

type Request = { package: string; preview?: boolean; expectDigest?: string | null };

function show({
  scenario = "sharing-invite-pack",
  installed = [],
  now,
}: {
  scenario?: SharingScenario;
  installed?: InstalledPack[];
  now?: () => number;
} = {}) {
  withFlags({ sharing: true });
  const sharing = createHarnessSharing(scenario, now);
  const spied = {
    acceptInvite: vi.spyOn(sharing, "acceptInvite"),
    cancelRequest: vi.spyOn(sharing, "cancelRequest"),
  };
  let packs = installed;
  const installPack = vi.fn(async (request: Request) => {
    const [pack, version] = request.package.split("@");
    if (!request.preview)
      packs = [
        {
          pack: pack!,
          installed: version!,
          latest: version!,
          outdated: false,
          error: null,
        },
      ];
    return {
      pack: pack!,
      coordinate: pack!,
      version: version ?? null,
      digest: DIGEST,
      source: "registry",
      documents: [],
      authority: [],
    };
  });
  const app = testApp({
    sharing,
    api: {
      listInstalledPacks: async () => ({ packs }),
      packInfo: async () => ({ versions: [] }),
      installPack,
    },
    snapshot: { bootstrap, client: { deployments: [node()] } },
  });
  renderIn(app, <AgentScreen nodeDid={deployment.nodeDid} section="sharing" />);
  return { installPack, ...spied };
}

const dialog = () => screen.findByTestId("join-dialog");
const preview = () => screen.findByTestId("invite-preview");
const field = () => screen.getByRole("textbox", { name: "Invite link or code" });

beforeEach(() => resetInstalledPacks());
afterEach(() => {
  cleanup();
  resetJoins();
  resetPreferences();
});

describe("joining from an invite", () => {
  it("reads a pasted code and shows who invites, what it shares and what it allows", async () => {
    const user = userEvent.setup();
    show({ scenario: "sharing-empty" });
    await user.click(
      await screen.findByRole("button", { name: "Join with an invite…" }),
    );
    const steps = await dialog();
    expect(
      within(steps).getByRole("heading", { name: "Join with an invite" }),
    ).toBeTruthy();
    expect(within(steps).getByRole("button", { name: "Continue" })).toBeDisabled();
    await user.type(field(), `${HARNESS_INVITE.code}{Enter}`);
    const card = await preview();
    expect(within(steps).queryByRole("textbox")).toBeNull();
    expect(within(card).getByText("did:key:z6MkHarnessDavid")).toBeTruthy();
    expect(
      within(card).getByText(
        "Says it's David. Check the DID with them before you join.",
      ),
    ).toBeTruthy();
    expect(within(card).getByText("A person")).toBeTruthy();
    expect(within(card).getByText("Tic-tac-toe pack")).toBeTruthy();
    expect(within(card).getByText("Make moves as themselves")).toBeTruthy();
    expect(within(card).getByText("The inviter approves your request")).toBeTruthy();
    expect(within(card).getByText(/^In 3d|^In 2d/)).toBeTruthy();
    await user.click(within(steps).getByRole("button", { name: "Back" }));
    expect((field() as HTMLInputElement).value).toBe(HARNESS_INVITE.code);
  });

  it("names an inviter by DID alone when the invite gives no name", async () => {
    show({ scenario: "sharing-invite-agent" });
    seedHarnessJoining("sharing-invite-agent");
    const card = await preview();
    expect(
      within(card).getByText("Check the DID with them before you join."),
    ).toBeTruthy();
    expect(within(card).getByText("Coach agent")).toBeTruthy();
    expect(within(card).queryByText(/Installs/)).toBeNull();
    expect(within(card).getByText("Not needed: you're in once you join")).toBeTruthy();
  });

  it("says under the field why an invite can't be read", async () => {
    const user = userEvent.setup();
    show({ scenario: "sharing-empty" });
    act(() => startJoin());
    await dialog();
    await user.type(field(), "nothing-here");
    await user.click(screen.getByRole("button", { name: "Continue" }));
    expect((await screen.findByRole("alert")).textContent).toBe(
      "Couldn't read this invite: This invite isn't valid, or it has expired.",
    );
    expect(screen.queryByTestId("invite-preview")).toBeNull();
    await user.type(field(), "x");
    expect(screen.queryByRole("alert")).toBeNull();
  });

  it("goes back to the field when a link it opened can't be read", async () => {
    show({ scenario: "sharing-empty" });
    act(() => startJoin("gents://invite/nothing-here"));
    expect((await screen.findByRole("alert")).textContent).toContain(
      "This invite isn't valid, or it has expired.",
    );
    expect((field() as HTMLInputElement).value).toBe("gents://invite/nothing-here");
  });

  it("installs the pack an invite brings in the same dialog, pinned to its version, before joining", async () => {
    const user = userEvent.setup();
    const { installPack, acceptInvite } = show();
    act(() => startJoin(HARNESS_INVITE.link));
    const steps = await dialog();
    let card = await preview();
    let packs = within(card).getByRole("list", { name: "Packs joining installs" });
    expect(await within(packs).findByText("Not installed")).toBeTruthy();
    expect(within(packs).getByText("1.2.0")).toBeTruthy();
    expect(within(steps).getByRole("button", { name: "Join" })).toBeDisabled();
    expect(within(steps).getByText("Install the pack first.")).toBeTruthy();

    await user.click(within(packs).getByRole("button", { name: "Review install" }));
    let review = within(steps).getByTestId("pack-install-review");
    expect(screen.getAllByRole("dialog")).toHaveLength(1);
    expect(screen.queryByTestId("invite-preview")).toBeNull();
    expect(installPack).toHaveBeenCalledWith(
      expect.objectContaining({
        package: `${HARNESS_GAME_PACK}@1.2.0`,
        preview: true,
      }),
    );
    await within(review).findByRole("heading", { name: "Install tic_tac_toe?" });
    expect(within(review).getByText(/from an invite$/)).toBeTruthy();
    await user.click(within(steps).getByRole("button", { name: "Back to invite" }));
    card = await preview();
    expect(within(card).getByText("Not installed")).toBeTruthy();

    await user.click(within(card).getByRole("button", { name: "Review install" }));
    review = within(steps).getByTestId("pack-install-review");
    await within(review).findByRole("heading", { name: "Install tic_tac_toe?" });
    await user.click(within(review).getByRole("button", { name: "Install" }));
    await waitFor(() =>
      expect(installPack).toHaveBeenCalledWith(
        expect.objectContaining({
          package: `${HARNESS_GAME_PACK}@1.2.0`,
          expectDigest: DIGEST,
        }),
      ),
    );
    card = await preview();
    packs = within(card).getByRole("list", { name: "Packs joining installs" });
    expect(await within(packs).findByText("Installed")).toBeTruthy();
    expect(within(steps).queryByText("Install the pack first.")).toBeNull();
    expect(acceptInvite).not.toHaveBeenCalled();
    await user.click(within(steps).getByRole("button", { name: "Join" }));
    expect(acceptInvite).toHaveBeenCalledWith(HARNESS_INVITE.link);
    await waitFor(() => expect(screen.queryByTestId("join-dialog")).toBeNull());
    const sent = screen.getByRole("list", { name: "Joining another node" });
    expect(
      within(sent).getByRole("listitem", { name: "Request to David" }).textContent,
    ).toContain("Waiting for approval");
  });

  it("offers to update a pack installed at another version", async () => {
    show({
      installed: [
        {
          pack: HARNESS_GAME_PACK,
          installed: "1.1.0",
          latest: "1.2.0",
          outdated: true,
          error: null,
        },
      ],
    });
    act(() => startJoin(HARNESS_INVITE.code));
    const card = await preview();
    expect(await within(card).findByText("You have 1.1.0")).toBeTruthy();
    expect(within(card).getByRole("button", { name: "Review update" })).toBeTruthy();
  });

  it("waits for approval after joining, and withdraws the request", async () => {
    const user = userEvent.setup();
    const { cancelRequest } = show({
      installed: [
        {
          pack: HARNESS_GAME_PACK,
          installed: "1.2.0",
          latest: "1.2.0",
          outdated: false,
          error: null,
        },
      ],
    });
    act(() => startJoin(HARNESS_INVITE.link));
    const steps = await dialog();
    await waitFor(() =>
      expect(within(steps).getByRole("button", { name: "Join" })).toBeEnabled(),
    );
    await user.click(within(steps).getByRole("button", { name: "Join" }));
    const sent = await screen.findByRole("list", { name: "Joining another node" });
    const row = within(sent).getByRole("listitem", { name: "Request to David" });
    expect(row.textContent).toContain("Waiting for approval");
    expect(row.textContent).toContain("says it's David");
    await waitFor(() => expect(screen.queryByTestId("join-dialog")).toBeNull());
    await user.click(within(row).getByRole("button", { name: "Withdraw" }));
    expect(cancelRequest).toHaveBeenCalledWith("request-harness-accepted-1");
    await waitFor(() =>
      expect(screen.queryByRole("listitem", { name: "Request to David" })).toBeNull(),
    );
    expect(screen.queryByRole("list", { name: "Joining another node" })).toBeNull();
    expect(screen.getByRole("button", { name: "Join with an invite…" })).toBeTruthy();
  });

  it("forgets the draft on Cancel or Esc, and starts again at the field", async () => {
    const user = userEvent.setup();
    show({ scenario: "sharing" });
    await screen.findByRole("region", { name: "Requests to join" });
    await user.click(screen.getByRole("button", { name: "Join with an invite…" }));
    await user.type(field(), `${HARNESS_INVITE.code}{Enter}`);
    let steps = await dialog();
    await preview();
    await user.click(within(steps).getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(screen.queryByTestId("join-dialog")).toBeNull());
    expect(screen.queryByRole("list", { name: "Joining another node" })).toBeNull();

    await user.click(screen.getByRole("button", { name: "Join with an invite…" }));
    steps = await dialog();
    expect((field() as HTMLInputElement).value).toBe("");
    await user.type(field(), "half a code");
    await user.keyboard("{Escape}");
    await waitFor(() => expect(screen.queryByTestId("join-dialog")).toBeNull());
    await user.click(screen.getByRole("button", { name: "Join with an invite…" }));
    await dialog();
    expect((field() as HTMLInputElement).value).toBe("");
  });

  it("is in at once from a pre-approved invite", async () => {
    const user = userEvent.setup();
    show({ scenario: "sharing-invite-node" });
    seedHarnessJoining("sharing-invite-node");
    const card = await preview();
    expect(within(card).getByText("One of their own devices")).toBeTruthy();
    expect(within(card).getByText("Whole node")).toBeTruthy();
    await user.click(screen.getByRole("button", { name: "Join" }));
    const row = await screen.findByRole("listitem", { name: "Request to David" });
    expect(row.textContent).toContain("Approved: you're in");
    expect(within(row).queryByRole("button", { name: "Withdraw" })).toBeNull();
  });

  it("shows a request sent earlier, waiting or approved, with the dialog closed", async () => {
    show({ scenario: "sharing-joining" });
    seedHarnessJoining("sharing-joining");
    expect(
      (await screen.findByRole("listitem", { name: "Request to David" })).textContent,
    ).toContain("Waiting for approval");
    expect(screen.queryByTestId("join-dialog")).toBeNull();
    cleanup();
    resetJoins();
    show({ scenario: "sharing-joined" });
    seedHarnessJoining("sharing-joined");
    const row = await screen.findByRole("listitem", { name: "Request to David" });
    expect(row.textContent).toContain("Approved: you're in");
    expect(screen.queryByTestId("join-dialog")).toBeNull();
    await userEvent.setup().click(within(row).getByRole("button", { name: "Dismiss" }));
    expect(screen.queryByRole("listitem", { name: "Request to David" })).toBeNull();
  });

  it("can't join an invite that has expired", async () => {
    show({
      scenario: "sharing-invite-node",
      now: () => Date.now() - 2 * 60 * 60 * 1000,
    });
    act(() => startJoin(HARNESS_DEVICE_INVITE.code));
    const card = await preview();
    const steps = await dialog();
    expect(within(card).getByText("Expired")).toBeTruthy();
    expect(within(steps).getByRole("button", { name: "Join" })).toBeDisabled();
    expect(within(steps).getByText("Ask for a new one.")).toBeTruthy();
  });
});

describe("the joining scenarios", () => {
  function showHarness(scenario: SharingScenario) {
    withFlags({ sharing: true });
    const harness = createDesktopUiHarness({ scenario });
    renderIn(
      testApp({
        sharing: harness.sharing,
        api: harness.adapter,
        snapshot: { bootstrap, client: { deployments: [node()] } },
      }),
      <AgentScreen nodeDid={deployment.nodeDid} section="sharing" />,
    );
    act(() => seedHarnessJoining(scenario));
  }

  it("open the dialog at each invite's preview", async () => {
    for (const [scenario, shares] of [
      ["sharing-invite-node", "Whole node"],
      ["sharing-invite-pack", "Tic-tac-toe pack"],
      ["sharing-invite-agent", "Coach agent"],
    ] as const) {
      showHarness(scenario);
      const card = await preview();
      expect(within(card).getByText(shares), scenario).toBeTruthy();
      cleanup();
      resetJoins();
    }
  });

  it("install the pack the invite brings and join from the dialog", async () => {
    const user = userEvent.setup();
    showHarness("sharing-invite-pack");
    const steps = await dialog();
    const card = await preview();
    await user.click(
      await within(card).findByRole("button", { name: "Review install" }),
    );
    const review = within(steps).getByTestId("pack-install-review");
    await user.click(await within(review).findByRole("button", { name: "Install" }));
    expect(await within(await preview()).findByText("Installed")).toBeTruthy();
    await user.click(within(steps).getByRole("button", { name: "Join" }));
    const row = await screen.findByRole("listitem", { name: "Request to David" });
    expect(row.textContent).toContain("Waiting for approval");
    await waitFor(() => expect(screen.queryByTestId("join-dialog")).toBeNull());
  });
});

describe("an invite link", () => {
  function mountNav() {
    let nav: Nav | null = null;
    function Bind() {
      nav = useNav();
      bindNav(nav);
      return null;
    }
    render(
      <MemoryNavProvider>
        <Bind />
      </MemoryNavProvider>,
    );
    return () => nav!;
  }
  afterEach(() => {
    bindNav(null);
    bindHomeNode(null);
  });

  it("is read from outside the app and from inside a message, kept whole", () => {
    expect(parseLink("gents://invite/harness-game")).toEqual({
      kind: "invite",
      invite: "gents://invite/harness-game",
    });
    expect(parseLink("gents:invite/abc?via=lan")).toEqual({
      kind: "invite",
      invite: "gents://invite/abc?via=lan",
    });
    for (const href of [
      "gents://invite/",
      "gents://invite/a/b",
      "gents://invite/a%20b",
    ])
      expect(parseLink(href), href).toBeNull();
  });

  it("opens nothing while Sharing is off, or before this desktop's node is known", () => {
    const warn = vi.spyOn(console, "warn").mockImplementation(() => {});
    const nav = mountNav();
    bindHomeNode(deployment.nodeDid);
    let opened = true;
    act(() => void (opened = openLink(HARNESS_INVITE.link)));
    expect(opened).toBe(false);
    withFlags({ sharing: true });
    bindHomeNode(null);
    act(() => void (opened = openLink(HARNESS_INVITE.link)));
    expect(opened).toBe(false);
    expect(nav().route).toEqual({ name: "sessions" });
    warn.mockRestore();
  });

  it("opens the Sharing section of this desktop's node on the invite's preview, joining nothing", async () => {
    withFlags({ sharing: true });
    const nav = mountNav();
    bindHomeNode(deployment.nodeDid);
    act(() => void openLink(HARNESS_INVITE.link));
    expect(nav().route).toEqual({
      name: "agent",
      nodeDid: deployment.nodeDid,
      section: "sharing",
    });
    cleanup();
    const { acceptInvite } = show({ scenario: "sharing-empty" });
    const card = await preview();
    expect(within(await dialog()).queryByRole("textbox")).toBeNull();
    expect(within(card).getByText("Tic-tac-toe pack")).toBeTruthy();
    expect(acceptInvite).not.toHaveBeenCalled();
  });
});
