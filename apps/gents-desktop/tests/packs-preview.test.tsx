import { act, cleanup, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { DeploymentView } from "@source-inc/gents-desktop-client";
import { packPreviewKey, type PackReviewRequest } from "../src/ui/lib/packReview";
import { pathFor, parseRoute, type Route } from "../src/ui/lib/router";
import { resetPreferences } from "../src/ui/preferences";
import { PacksScreen } from "../src/ui/screens/PacksScreen";
import {
  rememberPacksAuthor,
  rememberPacksQuery,
  rememberPacksView,
  resetInstalledPacks,
} from "../src/ui/screens/packs/model";
import {
  resetPackPreviews,
  resetSetAsideReview,
} from "../src/ui/screens/packs/preview";
import { renderRouted, testApp, withFlags } from "./app-fixture";
import { bootstrap, deployment } from "./config-panel-wiring/fixtures";

vi.mock("../src/ui/contrib/runtime-door", () => ({
  refreshRuntimePackUxPlugins: vi.fn(async () => {}),
}));
vi.mock("sonner", () => ({
  toast: Object.assign(vi.fn(), { error: vi.fn(), success: vi.fn() }),
}));

const DIGEST = `sha256:${"f".repeat(64)}`;
const OWNER = deployment.nodeDid;
const node: DeploymentView = deployment;

const doc = (collection: string, id: string, name: string | null, content: object) => ({
  collection,
  id,
  name,
  content: { node_did: OWNER, ...content },
});

const DOCUMENTS = [
  doc("Agent", "writer", "Writer", {
    agent_id: "writer",
    display_name: "Writer",
    context_id: "writer-ctx",
    inference_profile_id: "default",
  }),
  doc("AgentContext", "writer-ctx", null, {
    context_id: "writer-ctx",
    system_prompt: "You write the docs.",
    tools_id: "writer-tools",
    skill_ids: ["cite"],
  }),
  doc("Tools", "writer-tools", "Writer tools", {
    tools_id: "writer-tools",
    display_name: "Writer tools",
  }),
  doc("Skill", "cite", "Cite sources", {
    skill_id: "cite",
    name: "cite",
    display_name: "Cite sources",
    instructions: "Cite every claim.",
  }),
  doc("Task", "nightly", "Nightly", {
    task_id: "nightly",
    display_name: "Nightly",
    agent_id: "writer",
    prompt_template: "Write tonight's notes",
    emit_outcome: false,
  }),
  doc("Schedule", "at-two", "At two", {
    schedule_id: "at-two",
    display_name: "At two",
    cadence: {
      kind: "cron",
      expression: "0 2 * * *",
      timezone: "UTC",
      missed_run_policy: "latest_only",
    },
  }),
  doc("EventSource", "doc-changes", "Doc changes", {
    event_source_id: "doc-changes",
    display_name: "Doc changes",
    source_collection: "DocsPage",
    event_kind: "created",
  }),
];

function preview(overrides: Record<string, unknown> = {}) {
  return {
    pack: "docs",
    coordinate: "acme/docs",
    version: "2.0.0",
    description: "A documents page",
    digest: DIGEST,
    source: "registry",
    kind: "documents",
    documents: DOCUMENTS,
    collections: [],
    authority: [{ plugin: "fetcher", asks: "HTTP to example.com" }],
    would_write: false,
    ...overrides,
  };
}

type Request = { package: string; preview?: boolean; inferenceSlots?: string[] };

function bridge(previewed: (request: Request) => unknown = () => preview()) {
  return vi.fn(async (request: Request) =>
    request.preview
      ? previewed(request)
      : { pack: "docs", digest: DIGEST, source: "x" },
  );
}

const routed = (route: Route) =>
  route.name === "packs" ? (
    <PacksScreen item={route.item} review={route.review} doc={route.doc} />
  ) : null;

function show(installPack: ReturnType<typeof bridge>, initial: Route) {
  withFlags({ packs: true });
  rememberPacksView("installed");
  rememberPacksQuery("");
  rememberPacksAuthor(null);
  const app = testApp({
    api: {
      listInstalledPacks: async () => ({ packs: [] }),
      packInfo: async () => ({ latest: "2.0.0", versions: [] }),
      installPack,
    },
    snapshot: { bootstrap, client: { deployments: [node] } },
  });
  return renderRouted(app, initial, routed);
}

const previews = (installPack: ReturnType<typeof bridge>) =>
  installPack.mock.calls.map(([r]) => r).filter((r) => r.preview);
const installs = (installPack: ReturnType<typeof bridge>) =>
  installPack.mock.calls.map(([r]) => r).filter((r) => !r.preview);

async function wouldAdd() {
  return screen.findByRole("region", { name: "What it would add" });
}

beforeEach(() => {
  resetInstalledPacks();
  resetPackPreviews();
  resetSetAsideReview();
});
afterEach(() => {
  cleanup();
  resetPreferences();
});

describe("a pack's page before it's installed", () => {
  it("shows what a registry pack would add, nested as an installed pack's is", async () => {
    const installPack = bridge();
    show(installPack, { name: "packs", item: "acme/docs" });
    const adds = await wouldAdd();
    await within(adds).findByText("Writer");
    const writer = within(adds).getByRole("link", { name: /Writer\s*Agent/ });
    const nested = within(writer.closest("li")!.querySelector("ul")!)
      .getAllByRole("link")
      .map((link) => link.textContent);
    expect(nested).toEqual([
      "NightlyTask · runs it",
      "Writer toolsTool set · it uses",
      "Cite sourcesSkill · it uses",
    ]);
    expect(
      within(adds).getByRole("link", { name: /At two\s*Schedule/ }),
    ).toHaveAttribute(
      "href",
      pathFor({ name: "packs", item: "acme/docs", doc: "Schedule:at-two" }),
    );
    expect(within(adds).queryByText("writer-ctx")).toBeNull();
    expect(previews(installPack)).toEqual([
      expect.objectContaining({ package: "acme/docs", inferenceSlots: [] }),
    ]);
    expect(screen.queryByRole("region", { name: "What it adds" })).toBeNull();
    expect(screen.getByText(/Not installed · 2.0.0/)).toBeInTheDocument();
  });

  it("opens a small pack's rows, and closes one to say what's under it", async () => {
    show(bridge(), { name: "packs", item: "acme/docs" });
    const adds = await wouldAdd();
    const opener = await within(adds).findByRole("button", {
      name: "What's under Writer",
    });
    expect(opener).toHaveAttribute("aria-expanded", "true");
    const writer = within(adds).getByRole("link", { name: /Writer\s*Agent/ });
    expect(writer.closest("li")!.querySelector("ul")).not.toBeNull();
    await userEvent.setup().click(opener);
    expect(opener).toHaveAttribute("aria-expanded", "false");
    expect(writer.closest("li")!.querySelector("ul")).toBeNull();
    expect(writer).toHaveTextContent("Agent · 1 task · 1 tool set · 1 skill");
    expect(writer).toHaveAttribute(
      "href",
      pathFor({ name: "packs", item: "acme/docs", doc: "Agent:writer" }),
    );
  });

  it("previews a link's pack pinned to its digest, and installs only from its review", async () => {
    const linked: PackReviewRequest = {
      package: "acme/docs@2.0.0",
      source: "link",
      digest: DIGEST,
    };
    const installPack = bridge();
    show(installPack, { name: "packs", review: packPreviewKey(linked) });
    const adds = await wouldAdd();
    await within(adds).findByText("Writer");
    expect(screen.getByText(/From a link/)).toBeInTheDocument();
    expect(previews(installPack)[0]).toMatchObject({
      package: "acme/docs@2.0.0",
      expectDigest: DIGEST,
    });
    expect(screen.queryByTestId("pack-install-review")).toBeNull();
    const page = screen.getByTestId("pack-page");
    await userEvent
      .setup()
      .click(within(page).getByRole("button", { name: "Install" }));
    expect(await screen.findByTestId("pack-install-review")).toBeInTheDocument();
    expect(installs(installPack)).toEqual([]);
  });

  it("previews a file by a key, not its path", async () => {
    const file: PackReviewRequest = { package: "/Users/me/docs.pack", source: "file" };
    const installPack = bridge(() => preview({ source: "local" }));
    const view = show(installPack, { name: "packs", review: packPreviewKey(file) });
    const adds = await wouldAdd();
    await within(adds).findByText("Writer");
    expect(screen.getByText(/From docs\.pack/)).toBeInTheDocument();
    expect(pathFor(view.nav().route)).not.toContain("docs.pack");
    const route: Route = { name: "packs", review: "7", doc: "Agent:writer" };
    expect(parseRoute(pathFor(route))).toEqual(route);
  });
});

const LENSES = [
  ["recon", "Review reconnaissance"],
  ["scan", "Review area scanner"],
  ["verify", "Adversarial review verification"],
  ["triage", "Review triage"],
] as const;

/* shaped as gents/code_review: four agents whose contexts share one
   compaction, and one caller tool set that reaches each through a target */
const CODE_REVIEW = [
  ...LENSES.flatMap(([lens, name]) => [
    doc("Agent", `review-${lens}`, name, {
      agent_id: `review-${lens}`,
      display_name: name,
      context_id: `review-${lens}-context`,
    }),
    doc("AgentContext", `review-${lens}-context`, null, {
      context_id: `review-${lens}-context`,
      system_prompt: `You do the ${lens}.`,
      tools_id: `review-${lens}-tools`,
      compaction_id: "review-compaction",
    }),
    doc("Tools", `review-${lens}-tools`, `${name} tools`, {
      tools_id: `review-${lens}-tools`,
      display_name: `${name} tools`,
    }),
    doc("AgentTarget", `code-review-${lens}`, `code-review-${lens}`, {
      target_id: `code-review-${lens}`,
      agent_id: `review-${lens}`,
      name: `code-review-${lens}`,
    }),
  ]),
  doc("CompactionConfig", "review-compaction", null, {
    compaction_id: "review-compaction",
    threshold: 0.85,
  }),
  doc("Tools", "review-caller", "Review caller", {
    tools_id: "review-caller",
    display_name: "Review caller",
    agents: { target_ids: LENSES.map(([lens]) => `code-review-${lens}`) },
  }),
];

/* a context no agent of the pack runs with */
const ORPHAN = doc("AgentContext", "spare-context", null, {
  context_id: "spare-context",
});

describe("what a pack would add, its settings under what owns them", () => {
  const codeReview = (documents: object[]) =>
    bridge(() =>
      preview({ coordinate: "gents/code_review", pack: "code_review", documents }),
    );

  it("lists agents and tool sets, the caller's targets nested under it", async () => {
    show(codeReview(CODE_REVIEW), { name: "packs", item: "gents/code_review" });
    const adds = await wouldAdd();
    await within(adds).findByText("Review caller");
    expect(within(adds).getByText("4 agents · 5 tool sets")).toBeInTheDocument();
    const top = within(adds)
      .getAllByRole("listitem")
      .filter((li) => li.parentElement === adds.querySelector("ul"));
    /* thirteen rows open: a large pack starts with its top rows closed,
       each saying what's under it */
    expect(top.map((li) => li.querySelector("a")!.textContent)).toEqual([
      ...LENSES.map(([, name]) =>
        expect.stringMatching(new RegExp(`${name}Agent · 1 tool set$`)),
      ),
      "Review callerTool set · 4 agent targets",
    ]);
    expect(top.map((li) => li.querySelector("ul"))).toEqual(top.map(() => null));
    expect(within(adds).queryByText(/Agent context|Compaction config/)).toBeNull();
    expect(within(adds).queryByText("Other settings")).toBeNull();
    const user = userEvent.setup();
    const opener = within(adds).getByRole("button", {
      name: "What's under Review caller",
    });
    expect(opener).toHaveAttribute("aria-expanded", "false");
    await user.click(opener);
    expect(opener).toHaveAttribute("aria-expanded", "true");
    const caller = within(adds).getByRole("link", { name: /Review caller\s*Tool set/ });
    expect(caller).toHaveTextContent(/^Review callerTool set$/);
    expect(
      within(caller.closest("li")!.querySelector("ul")!)
        .getAllByRole("link")
        .map((link) => link.textContent),
    ).toEqual(
      LENSES.map(([lens, name]) => `code-review-${lens}Agent target · calls ${name}`),
    );
    await user.click(
      within(adds).getByRole("button", { name: "What's under Review reconnaissance" }),
    );
    const recon = within(adds).getByRole("link", {
      name: /Review reconnaissance\s*Agent/,
    });
    expect(
      within(recon.closest("li")!.querySelector("ul")!)
        .getAllByRole("link")
        .map((link) => link.textContent),
    ).toEqual(["Review reconnaissance toolsTool set · it uses"]);
  });

  it("nests a target no tool set calls under the agent it reaches", async () => {
    const uncalled = CODE_REVIEW.filter((d) => d.id !== "review-caller");
    show(codeReview(uncalled), { name: "packs", item: "gents/code_review" });
    const adds = await wouldAdd();
    const recon = await within(adds).findByRole("link", {
      name: /Review reconnaissance\s*Agent/,
    });
    expect(
      within(recon.closest("li")!.querySelector("ul")!)
        .getAllByRole("link")
        .map((link) => link.textContent),
    ).toEqual([
      "Review reconnaissance toolsTool set · it uses",
      "code-review-reconAgent target · reaches it",
    ]);
    expect(within(adds).getByText("4 agents · 4 tool sets")).toBeInTheDocument();
  });

  it("keeps what belongs to nothing it adds in one collapsed group", async () => {
    show(codeReview([...CODE_REVIEW, ORPHAN]), {
      name: "packs",
      item: "gents/code_review",
    });
    const adds = await wouldAdd();
    await within(adds).findByText("Review caller");
    expect(
      within(adds).getByText("4 agents · 5 tool sets · 1 other setting"),
    ).toBeInTheDocument();
    expect(within(adds).queryByText("spare-context")).toBeNull();
    await userEvent
      .setup()
      .click(within(adds).getByRole("button", { name: /Other settings\s*1/ }));
    expect(
      within(adds).getByRole("link", { name: /spare-context\s*Agent context/ }),
    ).toHaveAttribute(
      "href",
      pathFor({
        name: "packs",
        item: "gents/code_review",
        doc: "AgentContext:spare-context",
      }),
    );
  });

  it("shows an agent's context and its compaction on the agent's own page", async () => {
    show(codeReview(CODE_REVIEW), {
      name: "packs",
      item: "gents/code_review",
      doc: "Agent:review-recon",
    });
    const view = await screen.findByTestId("pack-item-view");
    expect(await within(view).findByText("You do the recon.")).toBeInTheDocument();
    expect(within(view).getByText("Context")).toBeInTheDocument();
    expect(within(view).getByText("Compaction")).toBeInTheDocument();
    expect(within(view).getByText("0.85")).toBeInTheDocument();
  });

  it("counts only what people look for in the install review, the rest as other settings", async () => {
    show(codeReview([...CODE_REVIEW, ORPHAN]), {
      name: "packs",
      item: "gents/code_review",
    });
    await within(await wouldAdd()).findByText("Review caller");
    const user = userEvent.setup();
    await user.click(
      within(screen.getByTestId("pack-page")).getByRole("button", { name: "Install" }),
    );
    const review = await screen.findByTestId("pack-install-review");
    const adds = await within(review).findByRole("region", { name: "What it adds" });
    expect(
      within(adds)
        .getAllByRole("button")
        .map((b) => b.textContent),
    ).toEqual(["4 agents", "5 tool sets", "1 other setting"]);
    await user.click(within(adds).getByRole("button", { name: "1 other setting" }));
    expect(
      within(adds).getByRole("button", { name: /spare-context/ }),
    ).toHaveTextContent("Agent context");
  });
});

describe("one document a pack would add", () => {
  const at = (ref: string): Route => ({ name: "packs", item: "acme/docs", doc: ref });

  it("says it's not installed, and shows the whole document under All fields", async () => {
    show(bridge(), at("Agent:writer"));
    const view = await screen.findByTestId("pack-item-view");
    expect(
      await within(view).findByRole("heading", { name: "Writer" }),
    ).toBeInTheDocument();
    expect(
      within(view).getByText("Agent · From docs · not installed"),
    ).toBeInTheDocument();
    expect(within(view).queryByText(/"inference_profile_id": "default"/)).toBeNull();
    await userEvent
      .setup()
      .click(within(view).getByRole("button", { name: "All fields" }));
    expect(
      within(view).getByText(/"inference_profile_id": "default"/),
    ).toBeInTheDocument();
    expect(within(view).getByRole("link", { name: "docs" })).toHaveAttribute(
      "href",
      pathFor({ name: "packs", item: "acme/docs" }),
    );
  });

  it("lists an agent's fields and its context's, since its editor reads the node", async () => {
    show(bridge(), at("Agent:writer"));
    const view = await screen.findByTestId("pack-item-view");
    await within(view).findByText("Inference profile id");
    expect(within(view).getByText("default")).toBeInTheDocument();
    expect(within(view).getByText("Context")).toBeInTheDocument();
    expect(within(view).getByText("You write the docs.")).toBeInTheDocument();
    expect(within(view).queryByText(OWNER)).toBeNull();
  });

  it("lists a task's fields", async () => {
    show(bridge(), at("Task:nightly"));
    const view = await screen.findByTestId("pack-item-view");
    expect(await within(view).findByText("Prompt template")).toBeInTheDocument();
    expect(within(view).getByText("Write tonight's notes")).toBeInTheDocument();
    expect(within(view).getByText("Emit outcome").closest("div")).toBeInTheDocument();
  });

  it("shows a schedule in its own editor, read-only", async () => {
    show(bridge(), at("Schedule:at-two"));
    const view = await screen.findByTestId("pack-item-view");
    const cron = await within(view).findByLabelText("Cron");
    expect(cron).toHaveValue("0 2 * * *");
    expect(cron.closest("fieldset[data-readonly]")).toBeDisabled();
    expect(within(view).getByLabelText("Display name")).toHaveValue("At two");
  });

  it("shows an event source in its own editor, read-only", async () => {
    show(bridge(), at("EventSource:doc-changes"));
    const view = await screen.findByTestId("pack-item-view");
    const collection = await within(view).findByLabelText("Source collection");
    expect(collection).toHaveValue("DocsPage");
    expect(collection.closest("fieldset[data-readonly]")).toBeDisabled();
  });

  it("shows a skill in its own editor, used by the pack's own agent", async () => {
    show(bridge(), at("Skill:cite"));
    const view = await screen.findByTestId("pack-item-view");
    expect(await within(view).findByText("Cite every claim.")).toBeInTheDocument();
    expect(within(view).getByRole("link", { name: "Writer" })).toHaveAttribute(
      "href",
      pathFor(at("Agent:writer")),
    );
    expect(within(view).getByText("acme/docs")).toBeInTheDocument();
    expect(
      within(view).getByText("Enabled").closest("fieldset[data-readonly]"),
    ).toBeDisabled();
  });

  it("says so when the pack doesn't add it", async () => {
    show(bridge(), at("Agent:nobody"));
    expect(await screen.findByText("This pack doesn't add it.")).toBeInTheDocument();
  });
});

describe("stepping out of a review to look at what it adds", () => {
  async function openReview() {
    const installPack = bridge();
    const view = show(installPack, { name: "packs", item: "acme/docs" });
    await within(await wouldAdd()).findByText("Writer");
    const user = userEvent.setup();
    const page = screen.getByTestId("pack-page");
    await user.click(within(page).getByRole("button", { name: "Install" }));
    const review = await screen.findByTestId("pack-install-review");
    await within(review).findByRole("heading", { name: "Install docs?" });
    return { installPack, view, user, review };
  }

  it("keeps the review's choices and opens it again as it was left", async () => {
    const { installPack, view, user, review } = await openReview();
    await user.click(
      within(review).getByRole("checkbox", { name: "Allow this access" }),
    );
    await user.click(within(review).getByText("1 schedule"));
    const before = previews(installPack).length;
    await user.click(within(review).getByRole("button", { name: /At two/ }));
    await waitFor(() => expect(screen.queryByTestId("pack-install-review")).toBeNull());
    expect(view.nav().route).toEqual({
      name: "packs",
      item: "acme/docs",
      doc: "Schedule:at-two",
    });
    const item = await screen.findByTestId("pack-item-view");
    await user.click(within(item).getByRole("button", { name: "Back to install" }));
    const again = await screen.findByTestId("pack-install-review");
    expect(
      within(again).getByRole("checkbox", { name: "Allow this access" }),
    ).toBeChecked();
    expect(within(again).getByRole("button", { name: "Install" })).toBeEnabled();
    expect(previews(installPack).length).toBe(before);
    await user.click(within(again).getByRole("button", { name: "Install" }));
    await waitFor(() => expect(installs(installPack)).toHaveLength(1));
    expect(installs(installPack)[0]).toMatchObject({
      grantAuthority: true,
      expectDigest: DIGEST,
    });
  });

  it("keeps the models chosen, and reads nothing again on the way back", async () => {
    const profile = (id: string, name: string) => ({
      profile_id: id,
      display_name: name,
      model_name: id,
      reasoning_effort: null,
      backend_id: `${id}:backend`,
      backend_name: null,
      provider_kind: null,
      usable: true,
      unavailable_reason: null,
    });
    const installPack = bridge((request) => {
      const bound = Object.fromEntries(
        (request.inferenceSlots ?? []).map((binding) => binding.split("=")),
      );
      return preview({
        authority: [],
        inference_needs: {
          slots: [
            {
              name: "writer",
              description: "Writes the docs",
              optional: false,
              packs: ["acme/docs"],
              profile: bound.writer ?? null,
              automatic: false,
            },
          ],
          profiles: [profile("claude", "Claude"), profile("glm", "GLM")],
          unbound: bound.writer ? [] : ["writer"],
        },
      });
    });
    show(installPack, { name: "packs", item: "acme/docs" });
    await within(await wouldAdd()).findByText("Writer");
    const user = userEvent.setup();
    await user.click(
      within(screen.getByTestId("pack-page")).getByRole("button", { name: "Install" }),
    );
    const review = await screen.findByTestId("pack-install-review");
    await user.click(await within(review).findByRole("combobox", { name: "writer" }));
    await user.click(await screen.findByRole("option", { name: "GLM" }));
    await waitFor(() =>
      expect(within(review).getByRole("button", { name: "Install" })).toBeEnabled(),
    );
    await user.click(within(review).getByText("1 agent"));
    await user.click(within(review).getByRole("button", { name: /Writer/ }));
    const item = await screen.findByTestId("pack-item-view");
    const before = previews(installPack).length;
    await user.click(within(item).getByRole("button", { name: "Back to install" }));
    const again = await screen.findByTestId("pack-install-review");
    expect(within(again).getByRole("combobox", { name: "writer" })).toHaveTextContent(
      "GLM",
    );
    expect(within(again).getByRole("button", { name: "Install" })).toBeEnabled();
    expect(previews(installPack).length).toBe(before);
    await user.click(within(again).getByRole("button", { name: "Install" }));
    await waitFor(() => expect(installs(installPack)).toHaveLength(1));
    expect(installs(installPack)[0]).toMatchObject({ inferenceSlots: ["writer=glm"] });
  });

  it("opens it again on going back to the page it was open on, until it's cancelled", async () => {
    const { installPack, view, user, review } = await openReview();
    await user.click(
      within(review).getByRole("checkbox", { name: "Allow this access" }),
    );
    await user.click(within(review).getByText("1 skill"));
    await user.click(within(review).getByRole("button", { name: /Cite sources/ }));
    await screen.findByTestId("pack-item-view");
    const before = previews(installPack).length;
    act(() => view.nav().back());
    const again = await screen.findByTestId("pack-install-review");
    expect(
      within(again).getByRole("checkbox", { name: "Allow this access" }),
    ).toBeChecked();
    expect(previews(installPack).length).toBe(before);
    await user.click(within(again).getByRole("button", { name: "Cancel" }));
    await waitFor(() => expect(screen.queryByTestId("pack-install-review")).toBeNull());
    const page = screen.getByTestId("pack-page");
    expect(within(page).queryByRole("button", { name: "Back to install" })).toBeNull();
    expect(within(page).getByRole("button", { name: "Install" })).toBeInTheDocument();
  });
});
