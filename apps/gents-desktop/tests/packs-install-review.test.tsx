import {
  act,
  cleanup,
  fireEvent,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { DeploymentView, InstalledPack } from "@source-inc/gents-desktop-client";
import { packPreviewKey, type PackReviewRequest } from "../src/ui/lib/packReview";
import { navigate, type Route } from "../src/ui/lib/router";
import { resetPreferences } from "../src/ui/preferences";
import { PacksScreen } from "../src/ui/screens/PacksScreen";
import {
  rememberPacksAuthor,
  rememberPacksQuery,
  rememberPacksView,
  packsListView,
  resetInstalledPacks,
} from "../src/ui/screens/packs/model";
import {
  resetPackPreviews,
  resetSetAsideReview,
} from "../src/ui/screens/packs/preview";
import { renderRouted, testApp, withFlags } from "./app-fixture";
import { bootstrap, deployment } from "./config-panel-wiring/fixtures";

const shell = vi.hoisted(() => ({
  picked: null as string | null,
  drop: null as
    null | ((event: { type: string; paths?: string[]; x: number; y: number }) => void),
}));
vi.mock("../src/lib/nativeShell", async (original) => ({
  ...(await original<typeof import("../src/lib/nativeShell")>()),
  inNativeShell: () => true,
  pickPackFile: async () => shell.picked,
  listenFileDrop: async (handler: NonNullable<typeof shell.drop>) => {
    shell.drop = handler;
    return () => {
      if (shell.drop === handler) shell.drop = null;
    };
  },
}));
vi.mock("../src/ui/contrib/runtime-door", () => ({
  refreshRuntimePackUxPlugins: vi.fn(async () => {}),
}));
vi.mock("sonner", () => ({
  toast: Object.assign(vi.fn(), { error: vi.fn(), success: vi.fn() }),
}));

const DIGEST = `sha256:${"e".repeat(64)}`;
const node: DeploymentView = deployment;
const research: InstalledPack = {
  pack: "demo/research",
  installed: "1.0.0",
  latest: "1.1.0",
  outdated: true,
  error: null,
};

function preview(overrides: Record<string, unknown> = {}) {
  return {
    pack: "docs",
    coordinate: "acme/docs",
    version: "2.0.0",
    description: "A documents page",
    digest: DIGEST,
    source: "registry",
    kind: "documents",
    documents: [
      { collection: "Agent", id: "writer", name: "Writer" },
      { collection: "Task", id: "nightly", name: "Nightly" },
      { collection: "Trigger", id: "nightly", name: null },
    ],
    collections: ["DocsPage"],
    authority: [],
    would_write: false,
    ...overrides,
  };
}

type Request = {
  package: string;
  preview?: boolean;
  expectDigest?: string | null;
  edited?: string;
  grantAuthority?: boolean;
  inferenceSlots?: string[];
};

/* the bridge's install: a preview reports `previewed`, an install runs
   `install` (which may refuse) */
function bridge(
  previewed: (request: Request) => unknown = () => preview(),
  install: (request: Request) => unknown = () => ({
    pack: "docs",
    digest: DIGEST,
    source: "registry",
  }),
) {
  return vi.fn(async (request: Request) =>
    request.preview ? previewed(request) : install(request),
  );
}

function show(
  installPack: ReturnType<typeof bridge>,
  item?: string | PackReviewRequest,
  packInfo: unknown = { latest: "2.0.0", versions: [] },
  on: DeploymentView = node,
  api: Record<string, unknown> = {},
) {
  withFlags({ packs: true });
  rememberPacksView("installed");
  rememberPacksQuery("");
  rememberPacksAuthor(null);
  const app = testApp({
    api: {
      listInstalledPacks: async () => ({ packs: [research] }),
      packInfo: async () => packInfo,
      searchPacks: async () => ({
        packs: [{ namespace: "acme", name: "docs", description: "A documents page" }],
        has_more: false,
      }),
      installPack,
      ...api,
    },
    snapshot: { bootstrap, client: { deployments: [on] } },
  });
  const initial: Route =
    typeof item === "object"
      ? { name: "packs", review: packPreviewKey(item) }
      : item
        ? { name: "packs", item }
        : { name: "packs" };
  const view = renderRouted(app, initial, routed);
  return Object.assign(app, { nav: view.nav });
}

/* the Packs screen on whatever route it's showing */
const routed = (route: Route) =>
  route.name === "packs" ? (
    <PacksScreen item={route.item} review={route.review} doc={route.doc} />
  ) : null;

/* the pack's page offers Install once it shows what the pack would add */
async function reviewFromPage(user = userEvent.setup()) {
  const page = await screen.findByTestId("pack-page");
  await user.click(await within(page).findByRole("button", { name: "Install" }));
  return { user, review: await screen.findByTestId("pack-install-review") };
}

const installs = (installPack: ReturnType<typeof bridge>) =>
  installPack.mock.calls.map(([request]) => request).filter((r) => !r.preview);

async function openRegistryReview() {
  const user = userEvent.setup();
  await user.click(await screen.findByRole("tab", { name: "Registry" }));
  const registry = await screen.findByRole("list", { name: "Registry packs" });
  await user.click(await within(registry).findByRole("button", { name: "Install" }));
  return { user, review: await screen.findByTestId("pack-install-review") };
}

const installButton = (review: HTMLElement) =>
  within(review).getByRole("button", { name: "Install" });

const previews = (installPack: ReturnType<typeof bridge>) =>
  installPack.mock.calls.map(([request]) => request).filter((r) => r.preview);

function profile(id: string, name: string, usable = true) {
  return {
    profile_id: id,
    display_name: name,
    model_name: id,
    reasoning_effort: null,
    backend_id: `${id}:backend`,
    backend_name: null,
    provider_kind: null,
    usable,
    unavailable_reason: usable ? null : "referenced backend is disabled",
  };
}

const SLOTS = [
  { name: "coordinator", description: "Plans the review" },
  { name: "worker", description: "Reads the changes" },
  { name: "verifier", description: "Checks the findings" },
];

/* a preview of a pack with `slots`, each bound by the request's
   `inferenceSlots` or else unbound, on a node with `profiles` */
function slotted(slots = SLOTS, profiles = [profile("large", "Large")]) {
  return (request: Request) => {
    const bound = Object.fromEntries(
      (request.inferenceSlots ?? []).map((binding) => binding.split("=")),
    );
    return preview({
      inference_needs: {
        slots: slots.map((slot) => ({
          ...slot,
          optional: false,
          packs: ["acme/docs"],
          profile: bound[slot.name] ?? null,
          automatic: false,
        })),
        profiles,
        unbound: slots.map((slot) => slot.name).filter((name) => !bound[name]),
      },
    });
  };
}

beforeEach(() => {
  resetInstalledPacks();
  resetPackPreviews();
  resetSetAsideReview();
  shell.picked = null;
});
afterEach(() => {
  cleanup();
  resetPreferences();
});

describe("the install review", () => {
  it("previews a registry install and installs exactly what it showed", async () => {
    const installPack = bridge();
    show(installPack);
    const { user, review } = await openRegistryReview();
    expect(
      await within(review).findByRole("heading", { name: "Install docs?" }),
    ).toBeInTheDocument();
    expect(installPack).toHaveBeenCalledWith(
      expect.objectContaining({ package: "acme/docs", preview: true }),
    );
    expect(
      within(review).getByText("Version 2.0.0 · from the registry"),
    ).toBeInTheDocument();
    await user.click(within(review).getByRole("button", { name: "Details" }));
    expect(within(review).getByText("DocsPage")).toBeInTheDocument();
    expect(within(review).getByText(DIGEST)).toBeInTheDocument();
    const adds = within(review).getByRole("region", { name: "What it adds" });
    for (const kind of ["1 agent", "1 task", "1 other setting"])
      expect(within(adds).getByRole("button", { name: kind })).toBeInTheDocument();
    await user.click(within(adds).getByRole("button", { name: "1 agent" }));
    expect(within(adds).getByText("Writer")).toBeInTheDocument();
    expect(within(review).queryByRole("region", { name: "Your edits" })).toBeNull();
    await user.click(installButton(review));
    await waitFor(() => expect(installs(installPack)).toHaveLength(1));
    expect(installs(installPack)[0]).toMatchObject({
      package: "acme/docs",
      expectDigest: DIGEST,
      edited: "refuse",
      grantAuthority: false,
    });
    await waitFor(() => expect(screen.queryByTestId("pack-install-review")).toBeNull());
  });

  it("writes nothing when cancelled", async () => {
    const installPack = bridge();
    show(installPack);
    const { user, review } = await openRegistryReview();
    await within(review).findByRole("heading", { name: "Install docs?" });
    await user.click(within(review).getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(screen.queryByTestId("pack-install-review")).toBeNull());
    expect(installs(installPack)).toEqual([]);
  });

  it("installs only once the person allows the access its plugins ask for", async () => {
    const installPack = bridge(() =>
      preview({ authority: [{ plugin: "fetch", asks: "HTTP to any host" }] }),
    );
    show(installPack);
    const { user, review } = await openRegistryReview();
    const access = await within(review).findByRole("region", {
      name: "Access it asks for",
    });
    expect(within(access).getByText("HTTP to any host")).toBeInTheDocument();
    expect(installButton(review)).toBeDisabled();
    await user.click(
      within(access).getByRole("checkbox", { name: "Allow this access" }),
    );
    await user.click(installButton(review));
    await waitFor(() => expect(installs(installPack)).toHaveLength(1));
    expect(installs(installPack)[0]).toMatchObject({ grantAuthority: true });
  });

  it("asks there when the install finds authority the preview didn't list", async () => {
    let refused = false;
    const installPack = bridge(undefined, () => {
      if (!refused) {
        refused = true;
        throw new Error(
          "plugin fetch asks for HTTP; install it with --grant-authority",
        );
      }
      return { pack: "docs", digest: DIGEST, source: "registry" };
    });
    show(installPack);
    const { user, review } = await openRegistryReview();
    await waitFor(() => expect(installButton(review)).toBeEnabled());
    await user.click(installButton(review));
    expect(await within(review).findByRole("alert")).toHaveTextContent(
      "It needs the access above to install",
    );
    expect(installButton(review)).toBeDisabled();
    await user.click(
      within(review).getByRole("checkbox", { name: "Allow this access" }),
    );
    await user.click(installButton(review));
    await waitFor(() => expect(installs(installPack)).toHaveLength(2));
    expect(installs(installPack)[1]).toMatchObject({ grantAuthority: true });
  });

  it("lets the person keep or replace edits to a pack it already installed", async () => {
    const installPack = bridge(() =>
      preview({ coordinate: "demo/research", pack: "research", version: "1.1.0" }),
    );
    show(installPack, { package: "demo/research@1.1.0", source: "link" });
    const { review } = await reviewFromPage();
    const edits = await within(review).findByRole("region", { name: "Your edits" });
    expect(
      within(review).getByText("Version 1.1.0 · replaces 1.0.0 · from a link"),
    ).toBeInTheDocument();
    const user = userEvent.setup();
    await user.click(
      within(edits).getByRole("radio", { name: "Replace them with the pack's" }),
    );
    await user.click(installButton(review));
    await waitFor(() => expect(installs(installPack)).toHaveLength(1));
    expect(installs(installPack)[0]).toMatchObject({
      package: "demo/research@1.1.0",
      edited: "overwrite",
      expectDigest: DIGEST,
    });
  });

  it("asks about edits there when the install finds some", async () => {
    let refused = false;
    const installPack = bridge(undefined, () => {
      if (!refused) {
        refused = true;
        throw new Error(
          "1 of acme/docs's documents were edited since it was installed: Agent/writer; use --overwrite or --keep",
        );
      }
      return { pack: "docs", digest: DIGEST, source: "registry" };
    });
    show(installPack);
    const { user, review } = await openRegistryReview();
    await waitFor(() => expect(installButton(review)).toBeEnabled());
    await user.click(installButton(review));
    const edits = await within(review).findByRole("region", { name: "Your edits" });
    expect(within(edits).getByRole("radio", { name: "Keep my edits" })).toBeChecked();
    await user.click(installButton(review));
    await waitFor(() => expect(installs(installPack)).toHaveLength(2));
    expect(installs(installPack)[1]).toMatchObject({ edited: "keep" });
  });

  it("names the pack when its preview fails", async () => {
    const installPack = bridge(() => {
      throw new Error("pack docs could not be read");
    });
    show(installPack);
    const { review } = await openRegistryReview();
    expect(await within(review).findByRole("alert")).toHaveTextContent(
      "Couldn't read this pack.",
    );
    expect(
      within(review).getByRole("heading", { name: "Install docs?" }),
    ).toBeInTheDocument();
    expect(installButton(review)).toBeDisabled();
  });

  it("names a data tool set and its tools", async () => {
    const installPack = bridge(() =>
      preview({
        documents: [
          {
            collection: "DatastoreToolSurface",
            id: "board",
            name: "Board",
            tools: ["list_tasks", "list_moves", "add_task", "move_task"],
          },
        ],
      }),
    );
    show(installPack);
    const { user, review } = await openRegistryReview();
    const adds = await within(review).findByRole("region", { name: "What it adds" });
    await user.click(within(adds).getByRole("button", { name: "1 data tool set" }));
    expect(within(adds).getByText("Board")).toBeInTheDocument();
    expect(
      within(adds).getByText("4 tools: list_tasks, list_moves, add_task, move_task"),
    ).toBeInTheDocument();
    expect(within(adds).queryByText(/other setting/i)).toBeNull();
  });

  it("shows the size the registry lists, and no size when it isn't known", async () => {
    show(bridge(), undefined, {
      latest: "2.0.0",
      versions: [{ version: "2.0.0", size_bytes: 2048 }],
    });
    const { user, review } = await openRegistryReview();
    await user.click(await within(review).findByRole("button", { name: "Details" }));
    expect(await within(review).findByText("2.0 KB")).toBeInTheDocument();
    cleanup();
    resetInstalledPacks();
    show(bridge());
    const again = await openRegistryReview();
    await within(again.review).findByRole("heading", { name: "Install docs?" });
    await again.user.click(
      within(again.review).getByRole("button", { name: "Details" }),
    );
    expect(within(again.review).queryByText("Size")).toBeNull();
  });

  it("refuses a pack that changed since it was reviewed, and reviews it again", async () => {
    const installPack = bridge(undefined, () => {
      throw new Error(
        `the pack's digest is sha256:${"f".repeat(64)}, not the expected ${DIGEST}. Nothing was installed.`,
      );
    });
    show(installPack);
    const { user, review } = await openRegistryReview();
    await waitFor(() => expect(installButton(review)).toBeEnabled());
    await user.click(installButton(review));
    expect(await within(review).findByRole("alert")).toHaveTextContent(
      "The pack changed since you reviewed it. Nothing was installed.",
    );
    const previews = () => installPack.mock.calls.filter(([r]) => r.preview).length;
    expect(previews()).toBe(1);
    await user.click(within(review).getByRole("button", { name: "Review again" }));
    await waitFor(() => expect(previews()).toBe(2));
    await waitFor(() => expect(installButton(review)).toBeEnabled());
  });
});

describe("the models a pack needs", () => {
  it("asks for a model for each slot, previews each choice, and installs with them", async () => {
    const installPack = bridge(
      slotted(SLOTS, [
        profile("large", "Large"),
        profile("glm", "GLM"),
        profile("down", "Down", false),
      ]),
    );
    show(installPack);
    const { user, review } = await openRegistryReview();
    const models = await within(review).findByRole("region", { name: "Models" });
    expect(within(models).getByText("Plans the review")).toBeInTheDocument();
    const coordinator = within(models).getByRole("combobox", { name: "coordinator" });
    expect(coordinator).toHaveTextContent("Choose a model");
    expect(installButton(review)).toBeDisabled();
    for (const [slot, model] of [
      ["coordinator", "Large"],
      ["worker", "GLM"],
      ["verifier", "Large"],
    ] as const) {
      await user.click(within(models).getByRole("combobox", { name: slot }));
      expect(screen.queryByRole("option", { name: "Down" })).toBeNull();
      await user.click(await screen.findByRole("option", { name: model }));
    }
    await waitFor(() =>
      expect(previews(installPack).at(-1)?.inferenceSlots).toEqual([
        "coordinator=large",
        "verifier=large",
        "worker=glm",
      ]),
    );
    expect(previews(installPack).at(-1)).toMatchObject({ expectDigest: null });
    await waitFor(() => expect(installButton(review)).toBeEnabled());
    expect(within(models).getByRole("combobox", { name: "worker" })).toHaveTextContent(
      "GLM",
    );
    await user.click(installButton(review));
    await waitFor(() => expect(installs(installPack)).toHaveLength(1));
    expect(installs(installPack)[0]).toMatchObject({
      expectDigest: DIGEST,
      inferenceSlots: ["coordinator=large", "worker=glm", "verifier=large"],
    });
  });

  it("shows the model the install chose by itself and installs with it", async () => {
    const installPack = bridge(() =>
      preview({
        inference_needs: {
          slots: [
            {
              name: "worker",
              description: "Reads the changes",
              optional: false,
              packs: ["acme/docs"],
              profile: "large",
              automatic: true,
            },
          ],
          profiles: [profile("large", "Large")],
          unbound: [],
        },
      }),
    );
    show(installPack);
    const { user, review } = await openRegistryReview();
    const models = await within(review).findByRole("region", { name: "Models" });
    expect(within(models).getByRole("combobox", { name: "worker" })).toHaveTextContent(
      "Large",
    );
    await waitFor(() => expect(installButton(review)).toBeEnabled());
    await user.click(installButton(review));
    await waitFor(() => expect(installs(installPack)).toHaveLength(1));
    expect(installs(installPack)[0]).toMatchObject({
      inferenceSlots: ["worker=large"],
    });
    expect(previews(installPack)).toHaveLength(1);
  });

  it("chooses the only model for every slot and previews with it", async () => {
    const installPack = bridge(slotted());
    show(installPack);
    const { review } = await openRegistryReview();
    await waitFor(() => expect(previews(installPack)).toHaveLength(2));
    expect(previews(installPack)[1]!.inferenceSlots).toEqual([
      "coordinator=large",
      "verifier=large",
      "worker=large",
    ]);
    await waitFor(() => expect(installButton(review)).toBeEnabled());
    const models = within(review).getByRole("region", { name: "Models" });
    expect(
      within(models).getByRole("combobox", { name: "verifier" }),
    ).toHaveTextContent("Large");
  });

  it("says plainly when the node has no model to run it", async () => {
    const installPack = bridge(slotted(SLOTS, [profile("down", "Down", false)]));
    show(installPack);
    const { review } = await openRegistryReview();
    const models = await within(review).findByRole("region", { name: "Models" });
    expect(models).toHaveTextContent(
      "This node has no model to run it yet. Set up a model first, then install this pack.",
    );
    expect(within(models).queryByRole("combobox")).toBeNull();
    expect(within(review).queryByRole("alert")).toBeNull();
    expect(installButton(review)).toBeDisabled();
    expect(previews(installPack)).toHaveLength(1);
  });
});

describe("what's already on the node", () => {
  const writes = (documents: unknown[], overrides: Record<string, unknown> = {}) =>
    bridge(() => preview({ documents, ...overrides }));

  it("asks before replacing a document the person made with the same id", async () => {
    const installPack = writes([
      { collection: "Agent", id: "ops", name: "Pack ops" },
      { collection: "Task", id: "nightly", name: "Nightly" },
    ]);
    show(installPack);
    const { user, review } = await openRegistryReview();
    const already = await within(review).findByRole("region", {
      name: "Already on this node",
    });
    expect(
      within(already).getByText("Replaces your own agent “Ops”"),
    ).toBeInTheDocument();
    expect(within(already).queryByText(/Nightly/)).toBeNull();
    await user.click(within(review).getByRole("button", { name: "Details" }));
    await waitFor(() =>
      expect(within(review).getByText("DocsPage")).toBeInTheDocument(),
    );
    expect(installButton(review)).toBeDisabled();
    await user.click(within(already).getByRole("checkbox", { name: "Replace them" }));
    await user.click(installButton(review));
    await waitFor(() => expect(installs(installPack)).toHaveLength(1));
  });

  it("says the install is refused when another pack installed a document with the same id", async () => {
    const installPack = writes([{ collection: "Agent", id: "scout", name: "Scout" }]);
    show(installPack, undefined, undefined, {
      ...node,
      agents: [
        ...node.agents,
        {
          ...node.agents[1]!,
          agentId: "scout",
          displayName: "Scout",
          tags: ["gents:pack:research"],
        },
      ],
    });
    const { review } = await openRegistryReview();
    const already = await within(review).findByRole("region", {
      name: "Already on this node",
    });
    expect(within(already).getByText("Agent “Scout”")).toBeInTheDocument();
    expect(within(already).getByText("Already from demo/research")).toBeInTheDocument();
    expect(already).toHaveTextContent(
      "It can't install: a pack can't replace what another pack installed. Remove that pack first.",
    );
    expect(within(already).queryByRole("checkbox")).toBeNull();
    expect(installButton(review)).toBeDisabled();
  });

  it("leaves out a document the pack itself installed", async () => {
    show(
      writes([{ collection: "Agent", id: "scout", name: "Scout" }]),
      undefined,
      undefined,
      {
        ...node,
        agents: [
          ...node.agents,
          {
            ...node.agents[1]!,
            agentId: "scout",
            displayName: "Scout",
            tags: ["gents:pack:docs"],
          },
        ],
      },
    );
    const { review } = await openRegistryReview();
    await waitFor(() => expect(installButton(review)).toBeEnabled());
    expect(
      within(review).queryByRole("region", { name: "Already on this node" }),
    ).toBeNull();
  });

  it("says which collections it shares with the node", async () => {
    show(writes([], { shared_collections: ["DocsPage"] }));
    const { review } = await openRegistryReview();
    const already = await within(review).findByRole("region", {
      name: "Already on this node",
    });
    expect(already).toHaveTextContent(
      "Shares DocsPage with what's already on this node.",
    );
    await waitFor(() => expect(installButton(review)).toBeEnabled());
  });
});

describe("a pack link", () => {
  it("opens the review pinned to the link's digest and installs nothing until confirmed", async () => {
    const linked = `sha256:${"1".repeat(64)}`;
    const installPack = bridge(() => preview({ digest: linked }));
    show(installPack, { package: "acme/docs@2.0.0", source: "link", digest: linked });
    const { review } = await reviewFromPage();
    await within(review).findByRole("heading", { name: "Install docs?" });
    expect(within(review).getByText(/from a link$/)).toBeInTheDocument();
    expect(installPack).toHaveBeenCalledWith(
      expect.objectContaining({
        package: "acme/docs@2.0.0",
        preview: true,
        expectDigest: linked,
      }),
    );
    expect(installs(installPack)).toEqual([]);
    await userEvent.setup().click(installButton(review));
    await waitFor(() => expect(installs(installPack)).toHaveLength(1));
    expect(installs(installPack)[0]).toMatchObject({ expectDigest: linked });
  });

  it("says when the pack isn't the one the link names, and offers no install", async () => {
    const linked = `sha256:${"1".repeat(64)}`;
    const installPack = bridge(() => {
      throw new Error(
        `the pack's digest is ${DIGEST}, not the expected ${linked}. Nothing was installed.`,
      );
    });
    show(installPack, { package: "acme/docs", source: "link", digest: linked });
    const { review } = await reviewFromPage();
    expect(await within(review).findByRole("alert")).toHaveTextContent(
      "This isn't the pack the link names",
    );
    expect(installButton(review)).toBeDisabled();
    expect(installs(installPack)).toEqual([]);
  });
});

describe("sharing a pack", () => {
  it("copies a link from an installed pack's page that pins its version and digest", async () => {
    const writeText = vi.fn(async () => {});
    Object.defineProperty(navigator, "clipboard", {
      value: { writeText },
      configurable: true,
    });
    const installPack = bridge();
    withFlags({ packs: true });
    const app = testApp({
      api: {
        listInstalledPacks: async () => ({ packs: [{ ...research, digest: DIGEST }] }),
        packInfo: async () => ({ versions: [] }),
        installPack,
      },
      snapshot: { bootstrap, client: { deployments: [node] } },
    });
    renderRouted(app, { name: "packs", item: "demo/research" }, routed);
    const page = await screen.findByTestId("pack-page");
    await act(async () => {
      fireEvent.click(
        await within(page).findByRole("button", { name: "More for demo/research" }),
      );
    });
    /* user-event stands in its own clipboard, so the click is a plain one */
    await act(async () => {
      fireEvent.click(await screen.findByRole("menuitem", { name: "Copy a link" }));
    });
    expect(writeText).toHaveBeenCalledWith(
      `gents://pack/demo/research?version=1.0.0&digest=${encodeURIComponent(DIGEST)}`,
    );
  });
});

describe("installing from a file", () => {
  it("opens the .pack file the person picks on its page, and reviews it from there", async () => {
    shell.picked = "/Users/me/Downloads/docs.pack";
    const installPack = bridge(() => preview({ source: "local" }));
    const app = show(installPack);
    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: "Install from file…" }));
    const page = await screen.findByTestId("pack-page");
    expect(
      await within(page).findByRole("heading", { name: "docs" }),
    ).toBeInTheDocument();
    expect(JSON.stringify(app.nav().route)).not.toContain("Downloads");
    expect(screen.queryByTestId("pack-install-review")).toBeNull();
    const { review } = await reviewFromPage(user);
    await within(review).findByRole("heading", { name: "Install docs?" });
    expect(within(review).getByText(/from docs\.pack$/)).toBeInTheDocument();
    await user.click(within(review).getByRole("button", { name: "Details" }));
    expect(within(review).queryByText("Size")).toBeNull();
    expect(installPack).toHaveBeenCalledWith(
      expect.objectContaining({ package: shell.picked, preview: true }),
    );
    await user.click(installButton(review));
    await waitFor(() => expect(installs(installPack)).toHaveLength(1));
    expect(installs(installPack)[0]).toMatchObject({
      package: shell.picked,
      expectDigest: DIGEST,
    });
  });

  it("does nothing when the picker is cancelled", async () => {
    const installPack = bridge();
    show(installPack);
    await userEvent
      .setup()
      .click(await screen.findByRole("button", { name: "Install from file…" }));
    await act(async () => {});
    expect(screen.queryByTestId("pack-install-review")).toBeNull();
    expect(installPack).not.toHaveBeenCalled();
  });

  it("opens a .pack file dropped on the page on its page", async () => {
    const installPack = bridge(() => preview({ source: "local" }));
    show(installPack);
    await waitFor(() => expect(shell.drop).not.toBeNull());
    act(() => shell.drop!({ type: "over", x: 0, y: 0 }));
    expect(await screen.findByText(/Drop a .pack file/)).toBeInTheDocument();
    act(() =>
      shell.drop!({
        type: "drop",
        paths: ["/tmp/notes.txt", "/tmp/docs.pack"],
        x: 0,
        y: 0,
      }),
    );
    const page = await screen.findByTestId("pack-page");
    expect(
      await within(page).findByRole("heading", { name: "docs" }),
    ).toBeInTheDocument();
    expect(installPack).toHaveBeenCalledWith(
      expect.objectContaining({ package: "/tmp/docs.pack", preview: true }),
    );
    expect(screen.queryByTestId("pack-install-review")).toBeNull();
    expect(installs(installPack)).toEqual([]);
  });
});

describe("coming back to the Packs list", () => {
  /* two pages of the registry: acme/docs is on the second */
  const searchPacks = () =>
    vi.fn(async (_query: string, page: number) =>
      page === 1
        ? {
            packs: [{ namespace: "acme", name: "first", description: "First" }],
            has_more: true,
          }
        : {
            packs: [
              { namespace: "acme", name: "docs", description: "A documents page" },
            ],
            has_more: false,
          },
    );

  async function onSecondPage(search: ReturnType<typeof searchPacks>) {
    const asks = () =>
      preview({ authority: [{ plugin: "fetcher", asks: "HTTP to example.com" }] });
    const app = show(bridge(asks), undefined, undefined, node, {
      searchPacks: search,
    });
    const user = userEvent.setup();
    await user.click(await screen.findByRole("tab", { name: "Registry" }));
    await screen.findByText("first");
    await user.click(screen.getByRole("button", { name: "Next" }));
    const registry = await screen.findByRole("list", { name: "Registry packs" });
    await within(registry).findByText("docs");
    const scroller = screen
      .getByTestId("packs-screen")
      .querySelector<HTMLElement>('[data-slot="scroll-area-viewport"]')!;
    scroller.scrollTop = 480;
    fireEvent.scroll(scroller);
    expect(packsListView().scroll.registry).toBe(480);
    return { app, user, registry, scroller };
  }

  it("shows the registry's page as left and opens the review again on going back", async () => {
    const search = searchPacks();
    const { app, user, registry, scroller } = await onSecondPage(search);
    await user.click(within(registry).getByRole("button", { name: "Install" }));
    const review = await screen.findByTestId("pack-install-review");
    await within(review).findByRole("heading", { name: "Install docs?" });
    await user.click(
      within(review).getByRole("checkbox", { name: "Allow this access" }),
    );
    await user.click(within(review).getByText("1 agent"));
    await user.click(within(review).getByRole("button", { name: /Writer/ }));
    await screen.findByTestId("pack-item-view");
    scroller.scrollTop = 0;
    const searches = search.mock.calls.length;
    act(() => app.nav().back());
    const again = await screen.findByTestId("pack-install-review");
    expect(
      within(again).getByRole("heading", { name: "Install docs?" }),
    ).toBeInTheDocument();
    expect(
      within(
        screen.getByRole("list", { name: "Registry packs", hidden: true }),
      ).getByText("docs"),
    ).toBeInTheDocument();
    expect(screen.getByText("Page 2")).toBeInTheDocument();
    expect(search.mock.calls.length).toBe(searches);
    expect(scroller.scrollTop).toBe(480);
    expect(packsListView().registry).toMatchObject({ page: 2 });
  });

  it("puts the list back where it was after a pack's page", async () => {
    const search = searchPacks();
    const { app, scroller } = await onSecondPage(search);
    act(() => navigate({ name: "packs", item: "acme/docs" }));
    await screen.findByTestId("pack-page");
    scroller.scrollTop = 0;
    const searches = search.mock.calls.length;
    act(() => app.nav().back());
    await screen.findByRole("list", { name: "Registry packs" });
    expect(screen.getByText("Page 2")).toBeInTheDocument();
    expect(search.mock.calls.length).toBe(searches);
    expect(scroller.scrollTop).toBe(480);
    expect(screen.queryByTestId("pack-install-review")).toBeNull();
  });
});
