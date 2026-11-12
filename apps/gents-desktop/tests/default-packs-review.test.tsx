import { act, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";
import type {
  DefaultPack,
  DefaultPacksApproval,
} from "@source-inc/gents-desktop-client";
import { resetPreferences } from "../src/ui/preferences";
import { PacksScreen } from "../src/ui/screens/PacksScreen";
import { StarterPacksCard } from "../src/ui/screens/packs/DefaultPacksReview";
import { rememberPacksView, resetInstalledPacks } from "../src/ui/screens/packs/model";
import { resetDefaultPacks } from "../src/ui/screens/packs/useDefaultPacks";
import { renderIn, testApp, withFlags } from "./app-fixture";
import { bootstrap, deployment } from "./config-panel-wiring/fixtures";

vi.mock("sonner", () => ({
  toast: Object.assign(vi.fn(), { error: vi.fn(), success: vi.fn() }),
}));

const pack = (name: string, over: Partial<DefaultPack> = {}): DefaultPack => ({
  pack: `gents/${name}`,
  purpose: `doing ${name}`,
  without: `Without it the Engineer cannot do ${name}.`,
  state: "needs_approval",
  version: "1.0.0",
  digest: `sha256:${name}`,
  requested: [],
  turns_on: [],
  reason: null,
  ...over,
});

const web = pack("web", {
  requested: [{ plugin: "web_search", asks: "network access to search providers" }],
});
const workflows = pack("dynamic_workflows", {
  turns_on: ["Workflow scripts the model writes can start agents."],
});
const notes = pack("notes", { state: "ready" });
const ocr = pack("ocr", {
  state: "unavailable",
  digest: null,
  version: null,
  reason: "The registry could not be reached.",
});
const git = pack("git", { state: "installed" });
const ALL = [web, workflows, notes, ocr, git];

const installed = (packs: DefaultPack[], names: string[]) =>
  packs.map((p) =>
    names.includes(p.pack) ? { ...p, state: "installed" as const } : p,
  );

function harness(
  packs: DefaultPack[],
  approval?: (asked: string[]) => DefaultPacksApproval,
) {
  const listDefaultPacks = vi.fn(async () => ({ packs }));
  const approveDefaultPacks = vi.fn(async (asked: { pack: string }[]) =>
    approval
      ? approval(asked.map((a) => a.pack))
      : {
          packs: installed(
            packs,
            asked.map((a) => a.pack),
          ),
          failed: [],
        },
  );
  const api = {
    listDefaultPacks,
    approveDefaultPacks,
    listInstalledPacks: async () => ({ packs: [] }),
    readPackAccount: async () => ({ account: { username: "me" } }),
    searchPacks: async () => ({ packs: [], has_more: false }),
    fetchDesktopSnapshot: async () => ({
      bootstrap,
      client: { deployments: [deployment] },
    }),
  };
  return { api, listDefaultPacks, approveDefaultPacks };
}

const local = (api: object) =>
  testApp({ api, snapshot: { bootstrap, client: { deployments: [deployment] } } });

const card = () => screen.findByTestId("starter-packs-card");
const click = (element: HTMLElement) => act(async () => void fireEvent.click(element));

beforeEach(() => {
  resetDefaultPacks();
  resetInstalledPacks();
  resetPreferences();
});

describe("the starter packs card", () => {
  it("lists each pending pack with its permissions, what approving turns on and the cost of declining", async () => {
    const { api, listDefaultPacks } = harness(ALL);
    renderIn(local(api), <StarterPacksCard />);
    const dialog = await card();
    expect(within(dialog).getByText("Starter packs")).toBeInTheDocument();
    expect(within(dialog).getByText(/Installing them needs the network/)).toBeTruthy();
    const rows = within(within(dialog).getByRole("list", { name: "Starter packs" }))
      .getAllByRole("listitem")
      .filter((row) => row.parentElement?.getAttribute("aria-label"));
    expect(
      rows.map((row) => within(row).getAllByText(/^doing /)[0]!.textContent),
    ).toEqual(["doing web", "doing dynamic_workflows", "doing notes", "doing ocr"]);
    expect(within(rows[0]!).getByText("web_search")).toBeInTheDocument();
    expect(
      within(rows[0]!).getByText("network access to search providers"),
    ).toBeInTheDocument();
    expect(
      within(rows[0]!).getByText("Without it the Engineer cannot do web."),
    ).toBeInTheDocument();
    expect(
      within(rows[1]!).getByText("Workflow scripts the model writes can start agents."),
    ).toBeInTheDocument();
    expect(
      within(rows[2]!).getByRole("button", { name: "Install gents/notes" }),
    ).toBeEnabled();
    expect(
      within(rows[3]!).getByText("The registry could not be reached."),
    ).toBeTruthy();
    expect(within(dialog).queryByText(/authority/i)).toBeNull();
    expect(listDefaultPacks).toHaveBeenCalledTimes(1);
  });

  it("approves one pack with the digest it showed", async () => {
    const { api, approveDefaultPacks } = harness(ALL);
    renderIn(local(api), <StarterPacksCard />);
    const dialog = await card();
    await click(within(dialog).getByRole("button", { name: "Approve gents/web" }));
    expect(approveDefaultPacks).toHaveBeenCalledWith([
      { pack: "gents/web", digest: "sha256:web" },
    ]);
    await waitFor(() =>
      expect(screen.queryByRole("button", { name: "Approve gents/web" })).toBeNull(),
    );
    expect(
      screen.getByRole("button", { name: "Approve gents/dynamic_workflows" }),
    ).toBeEnabled();
  });

  it("approves every pending pack at once, and closes once they are installed", async () => {
    const { api, approveDefaultPacks, listDefaultPacks } = harness([
      web,
      workflows,
      notes,
      git,
      ocr,
    ]);
    renderIn(local(api), <StarterPacksCard />);
    await click(within(await card()).getByRole("button", { name: "Approve all" }));
    expect(approveDefaultPacks).toHaveBeenCalledWith([
      { pack: "gents/web", digest: "sha256:web" },
      { pack: "gents/dynamic_workflows", digest: "sha256:dynamic_workflows" },
      { pack: "gents/notes", digest: "sha256:notes" },
    ]);
    await waitFor(() => expect(screen.queryByTestId("starter-packs-card")).toBeNull());
    expect(listDefaultPacks).toHaveBeenCalledTimes(1);
  });

  it("says the node restarts while an approval runs, and holds the buttons", async () => {
    let finish!: (result: DefaultPacksApproval) => void;
    const { api } = harness(ALL);
    api.approveDefaultPacks = vi.fn(
      () => new Promise<DefaultPacksApproval>((resolve) => (finish = resolve)),
    );
    renderIn(local(api), <StarterPacksCard />);
    const dialog = await card();
    await click(within(dialog).getByRole("button", { name: "Approve all" }));
    const restarting = () =>
      within(dialog).queryByText(
        "The local node restarts for a moment while the packs install.",
      );
    expect(restarting()).toBeInTheDocument();
    for (const button of within(dialog).getAllByRole("button", {
      name: /^(Approve|Install|Retry|Not now)/,
    }))
      expect(button).toBeDisabled();
    await act(async () => finish({ packs: ALL, failed: [] }));
    expect(restarting()).toBeNull();
  });

  it("says a pack that installed without finishing is installed", async () => {
    const { api } = harness(ALL, (asked) => ({
      packs: installed(
        ALL,
        asked.filter((p) => p !== "gents/web"),
      ),
      failed: [
        {
          pack: "gents/web",
          error: "gents/web is installed, but it was not wired into the Engineer",
          installed: true,
        },
      ],
    }));
    renderIn(local(api), <StarterPacksCard />);
    const dialog = await card();
    await click(within(dialog).getByRole("button", { name: "Approve all" }));
    const alert = await within(dialog).findByRole("alert");
    expect(alert).toHaveTextContent("web is installed with its permissions granted");
    expect(alert).toHaveTextContent("Approve it again to finish");
    expect(alert).not.toHaveTextContent("Nothing changed");
  });

  it("shows what could not be installed and keeps the pack", async () => {
    const { api } = harness(ALL, (asked) => ({
      packs: installed(
        ALL,
        asked.filter((p) => p !== "gents/web"),
      ),
      failed: [
        { pack: "gents/web", error: "digest mismatch for gents/web", installed: false },
      ],
    }));
    renderIn(local(api), <StarterPacksCard />);
    const dialog = await card();
    await click(within(dialog).getByRole("button", { name: "Approve all" }));
    const alert = await within(dialog).findByRole("alert");
    expect(alert).toHaveTextContent("Couldn't install web");
    await click(within(alert).getByText("Details"));
    expect(
      within(alert).getByText("digest mismatch for gents/web"),
    ).toBeInTheDocument();
    expect(
      within(dialog).getByRole("button", { name: "Approve gents/web" }),
    ).toBeEnabled();
    expect(
      within(dialog).queryByRole("button", { name: "Approve gents/dynamic_workflows" }),
    ).toBeNull();
  });

  it("stays closed after Not now, on this device and for that node only", async () => {
    const { api, approveDefaultPacks } = harness(ALL);
    const first = renderIn(local(api), <StarterPacksCard />);
    await click(within(await card()).getByRole("button", { name: "Not now" }));
    await waitFor(() => expect(screen.queryByTestId("starter-packs-card")).toBeNull());
    expect(approveDefaultPacks).not.toHaveBeenCalled();
    expect(
      JSON.parse(localStorage.getItem("gents-preferences")!).state
        .starterPacksDismissed,
    ).toEqual([deployment.nodeDid]);
    first.unmount();

    resetDefaultPacks();
    renderIn(local(api), <StarterPacksCard />);
    await waitFor(() => expect(api.listDefaultPacks).toHaveBeenCalledTimes(2));
    await act(async () => {});
    expect(screen.queryByTestId("starter-packs-card")).toBeNull();
  });

  it("reads nothing and shows nothing on a node that isn't the desktop's own", async () => {
    const { api, listDefaultPacks } = harness(ALL);
    const app = testApp({
      api,
      snapshot: {
        bootstrap,
        client: {
          deployments: [{ ...deployment, nodeDid: "did:key:remote", source: "peer" }],
        },
      },
    });
    renderIn(app, <StarterPacksCard />);
    await act(async () => {});
    expect(listDefaultPacks).not.toHaveBeenCalled();
    expect(screen.queryByTestId("starter-packs-card")).toBeNull();
  });

  it("doesn't open for packs that could only be retried", async () => {
    const { api, listDefaultPacks } = harness([ocr, git]);
    renderIn(local(api), <StarterPacksCard />);
    await waitFor(() => expect(listDefaultPacks).toHaveBeenCalled());
    await act(async () => {});
    expect(screen.queryByTestId("starter-packs-card")).toBeNull();
  });
});

describe("starter packs on the Packs page", () => {
  function page(api: object) {
    withFlags({ packs: true });
    rememberPacksView("installed");
    return renderIn(local(api), <PacksScreen />);
  }

  it("offers them while any is pending and stops once all are installed", async () => {
    const { api, approveDefaultPacks } = harness([web, notes, git]);
    page(api);
    const section = await screen.findByRole("region", { name: "Starter packs" });
    expect(within(section).queryByRole("button", { name: "Not now" })).toBeNull();
    await click(within(section).getByRole("button", { name: "Approve all" }));
    expect(approveDefaultPacks).toHaveBeenCalledWith([
      { pack: "gents/web", digest: "sha256:web" },
      { pack: "gents/notes", digest: "sha256:notes" },
    ]);
    await waitFor(() =>
      expect(screen.queryByRole("region", { name: "Starter packs" })).toBeNull(),
    );
  });

  it("shares the list with the card, read once", async () => {
    const { api, listDefaultPacks } = harness([web, git]);
    renderIn(
      local(api),
      <>
        <StarterPacksCard />
        <PacksScreen />
      </>,
    );
    await click(
      within(await card()).getByRole("button", { name: "Approve gents/web" }),
    );
    await waitFor(() => expect(screen.queryByTestId("starter-packs-card")).toBeNull());
    expect(screen.queryByRole("region", { name: "Starter packs" })).toBeNull();
    expect(listDefaultPacks).toHaveBeenCalledTimes(1);
  });

  it("re-reads the list from an unavailable pack's Retry", async () => {
    const { api, listDefaultPacks } = harness([ocr, git]);
    page(api);
    const section = await screen.findByRole("region", { name: "Starter packs" });
    expect(within(section).queryByRole("button", { name: "Approve all" })).toBeNull();
    await click(within(section).getByRole("button", { name: "Retry gents/ocr" }));
    expect(listDefaultPacks).toHaveBeenCalledTimes(2);
  });

  it("shows nothing when every starter pack is installed", async () => {
    const { api, listDefaultPacks } = harness([git]);
    page(api);
    await waitFor(() => expect(listDefaultPacks).toHaveBeenCalled());
    await act(async () => {});
    expect(screen.queryByRole("region", { name: "Starter packs" })).toBeNull();
  });
});
