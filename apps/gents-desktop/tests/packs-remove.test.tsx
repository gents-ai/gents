import { act, fireEvent, screen, waitFor, within } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { ReactElement } from "react";
import { toast } from "sonner";
import type {
  DeploymentView,
  InstalledPack,
  PackWireSources,
} from "@source-inc/gents-desktop-client";
import { resetPreferences } from "../src/ui/preferences";
import { PacksScreen } from "../src/ui/screens/PacksScreen";
import { PacksPanel } from "../src/ui/screens/agent/PacksPanel";
import {
  rememberPacksAuthor,
  rememberPacksQuery,
  rememberPacksView,
  resetInstalledPacks,
} from "../src/ui/screens/packs/model";
import {
  packUses,
  refusedRemoval,
  removalQuestion,
  editedQuestion,
  takenOut,
} from "../src/ui/screens/packs/removal";
import { resetDefaultPacks } from "../src/ui/screens/packs/useDefaultPacks";
import { renderIn, testApp } from "./app-fixture";
import { bootstrap, deployment } from "./config-panel-wiring/fixtures";

vi.mock("sonner", () => ({
  toast: Object.assign(vi.fn(), { error: vi.fn(), success: vi.fn() }),
}));
vi.mock("../src/ui/contrib/runtime-door", () => ({
  refreshRuntimePackUxPlugins: async () => {},
}));

const WEB = "gents/web";
const TOOLS = [
  "gents/web_search",
  "gents/web_fetch",
  "gents/web_crawl",
  "gents/browser_exec",
];
const QUESTION =
  "Remove gents/web and everything it installed?\n\nThe Engineer uses 4 tools from web: web_search, web_fetch, web_crawl, browser_exec. Removing web takes them out of its tools first.";

/* the Engineer's tools select the web pack's plugins beside one of its own */
const node: DeploymentView = {
  ...deployment,
  agents: deployment.agents.map((a) =>
    a.agentDid === "default" ? { ...a, displayName: "The Engineer" } : a,
  ),
  tools: deployment.tools.map((t) =>
    t.tools_id === "tools-a"
      ? {
          ...t,
          integrations: {
            plugins: [...TOOLS, "acme/notes"].map((plugin) => ({ plugin })),
          },
        }
      : /* the tool set the web pack installed for its own agent, Ops */
        {
          ...t,
          tags: [`gents:pack:${WEB}`],
          integrations: { plugins: TOOLS.map((plugin) => ({ plugin })) },
        },
  ),
};
const sources: PackWireSources = {
  plugins: [
    ...TOOLS.map((plugin) => ({ plugin, pack: WEB, job: false })),
    { plugin: "acme/notes", pack: "acme/notes", job: false },
    { plugin: "demo/lookup", pack: "demo/research", job: false },
  ],
  target_packs: [],
};
const pack = (name: string): InstalledPack => ({
  pack: name,
  installed: "1.0.0",
  latest: "1.0.0",
  outdated: false,
  error: null,
});

/* the runtime's refusal to remove a pack one of whose documents was edited */
const EDITED =
  "1 of gents/web's documents were edited since it was installed: Tools/web-researcher-tools. Overwrite replaces them with the pack's versions (a removal deletes them); keep leaves them as edited. Decide with --overwrite or --keep on `gents config pack install`, `gents config pack update` or `gents config pack remove`, or options.edited on the agent pack command";

/* the runtime's refusal of a pack another installed pack requires */
const REQUIRED = "gents/web is required by acme/research; remove it first";

const UNWIRES = TOOLS.map((plugin) => `unwire ${plugin} from default`);
const TAKEN_OUT =
  "web_search, web_fetch, web_crawl and browser_exec were taken out of The Engineer's tools.";

/* the runtime's refusal while the Engineer's tools still select a plugin */
const REFUSAL =
  'cannot remove plugin gents/web_search: agent "default" still selects it through Tools "tools-a"; remove that reference first with `gents config pack wire gents/web_search --agent default --remove --home <node-home>`';

beforeEach(() => {
  resetInstalledPacks();
  resetDefaultPacks();
  vi.mocked(toast.error).mockClear();
  vi.mocked(toast.success).mockClear();
});
afterEach(() => {
  resetPreferences();
  vi.restoreAllMocks();
});

describe("who uses a pack's tools", () => {
  it("names each agent whose tools select the pack's plugins, and only those", () => {
    expect(packUses(node, sources, WEB)).toEqual([
      { agentDid: "default", name: "The Engineer", plugins: TOOLS },
    ]);
    expect(packUses(node, sources, "demo/research")).toEqual([]);
    expect(removalQuestion(WEB, packUses(node, sources, WEB))).toBe(QUESTION);
  });

  it("leaves out the agents that use the pack's tools through a tool set the pack installed", () => {
    expect(packUses(node, sources, WEB).map((use) => use.agentDid)).toEqual([
      "default",
    ]);
  });

  it("asks about the pack's edited documents in the runtime's own words", () => {
    expect(editedQuestion(WEB, EDITED)).toBe(
      `${EDITED}\n\nOK deletes them with the rest of web, Cancel keeps web installed.`,
    );
  });

  it("says what a stopped removal already took out of each agent's tools", () => {
    expect(takenOut([])).toBe("");
    expect(
      takenOut([
        { agentDid: "default", name: "The Engineer", plugins: TOOLS.slice(0, 1) },
        { agentDid: "ops", name: "Ops", plugins: TOOLS.slice(1, 3) },
      ]),
    ).toBe(
      "web_search was taken out of The Engineer's tools. web_fetch and web_crawl were taken out of Ops's tools.",
    );
  });

  it("asks as it always did when no agent uses them", () => {
    expect(removalQuestion("demo/research", [])).toBe(
      "Remove demo/research and everything it installed?",
    );
  });

  it("names the agents in a refusal by the names they go by", () => {
    expect(refusedRemoval(WEB, REFUSAL, node)).toBe(
      "Couldn't remove web: The Engineer still uses tools from it. Take them out of The Engineer's tools, then remove web again.",
    );
    expect(refusedRemoval(WEB, "the registry is unreachable", node)).toBeNull();
  });
});

describe("removing a pack from the Packs page", () => {
  function show(installed: InstalledPack, api: Record<string, unknown>) {
    rememberPacksView("installed");
    rememberPacksQuery("");
    rememberPacksAuthor(null);
    renderIn(
      testApp({
        api: {
          listInstalledPacks: async () => ({ packs: [installed] }),
          listDefaultPacks: async () => ({ packs: [] }),
          listPackWireSources: async () => sources,
          searchPacks: async () => ({ packs: [], has_more: false }),
          ...api,
        },
        snapshot: { bootstrap, client: { deployments: [node] } },
      }),
      <PacksScreen />,
    );
  }

  async function chooseRemove(name: string) {
    const list = await screen.findByRole("list", { name: "Installed packs" });
    await act(async () => {
      fireEvent.click(within(list).getByRole("button", { name: `More for ${name}` }));
    });
    const remove = (await screen.findAllByRole("menuitem")).find(
      (item) => item.textContent === "Remove",
    )!;
    await act(async () => {
      fireEvent.click(remove);
    });
  }

  /* each call in the order it was made; each removal throws the next of
     `removals` until none is left, and the `failAt`th unwire throws `wire` */
  function recorder(
    fail: { wire?: string; failAt?: number; removals?: string[] } = {},
  ) {
    const calls: string[] = [];
    const removals = [...(fail.removals ?? [])];
    let unwires = 0;
    return {
      calls,
      wirePack: vi.fn(async (request: { plugin: string; agent: string }) => {
        calls.push(`unwire ${request.plugin} from ${request.agent}`);
        unwires += 1;
        if (fail.wire && unwires === (fail.failAt ?? 1)) throw new Error(fail.wire);
        return {};
      }),
      removePack: vi.fn(async (name: string, edited?: string) => {
        calls.push(`remove ${name} (${edited})`);
        const refusal = removals.shift();
        if (refusal === EDITED)
          throw Object.assign(new Error(EDITED), {
            packRefusal: {
              class: "documents_edited",
              coordinate: WEB,
              named: ["Tools/web-researcher-tools"],
              unnamed: 0,
              sentence: EDITED,
            },
          });
        if (refusal) throw new Error(refusal);
      }),
    };
  }

  it("names who uses its tools, and takes them out only when the runtime refuses for them", async () => {
    const confirm = vi.spyOn(window, "confirm").mockReturnValue(true);
    const api = recorder({ removals: [REFUSAL] });
    show(pack(WEB), api);
    await chooseRemove(WEB);
    await waitFor(() => expect(toast.success).toHaveBeenCalled());
    expect(confirm).toHaveBeenCalledWith(QUESTION);
    expect(api.calls).toEqual([
      `remove ${WEB} (refuse)`,
      ...UNWIRES,
      `remove ${WEB} (refuse)`,
    ]);
    for (const plugin of TOOLS) {
      expect(api.wirePack).toHaveBeenCalledWith({
        plugin,
        agent: "default",
        targetsFrom: [],
        remove: true,
        preview: false,
      });
    }
  });

  it("asks before deleting the pack's edited documents, before any tools come out", async () => {
    const confirm = vi.spyOn(window, "confirm").mockReturnValue(true);
    const api = recorder({ removals: [EDITED, REFUSAL] });
    show(pack(WEB), api);
    await chooseRemove(WEB);
    await waitFor(() => expect(toast.success).toHaveBeenCalled());
    expect(confirm).toHaveBeenLastCalledWith(editedQuestion(WEB, EDITED));
    expect(api.calls).toEqual([
      `remove ${WEB} (refuse)`,
      `remove ${WEB} (overwrite)`,
      ...UNWIRES,
      `remove ${WEB} (overwrite)`,
    ]);
  });

  it("keeps the pack and every agent's tools when deleting its edited documents is declined", async () => {
    vi.spyOn(window, "confirm").mockReturnValueOnce(true).mockReturnValueOnce(false);
    const api = recorder({ removals: [EDITED] });
    show(pack(WEB), api);
    await chooseRemove(WEB);
    await waitFor(() => expect(window.confirm).toHaveBeenCalledTimes(2));
    expect(api.calls).toEqual([`remove ${WEB} (refuse)`]);
    expect(api.wirePack).not.toHaveBeenCalled();
  });

  it("takes no tools out when the runtime refuses for another reason", async () => {
    vi.spyOn(window, "confirm").mockReturnValue(true);
    const api = recorder({ removals: [REQUIRED] });
    show(pack(WEB), api);
    await chooseRemove(WEB);
    await waitFor(() => expect(toast.error).toHaveBeenCalledWith(REQUIRED));
    expect(api.calls).toEqual([`remove ${WEB} (refuse)`]);
  });

  it("changes nothing when the question is declined", async () => {
    vi.spyOn(window, "confirm").mockReturnValue(false);
    const api = recorder();
    show(pack(WEB), api);
    await chooseRemove(WEB);
    await waitFor(() => expect(window.confirm).toHaveBeenCalled());
    expect(api.calls).toEqual([]);
  });

  it("removes a pack no agent uses with no step before it", async () => {
    const confirm = vi.spyOn(window, "confirm").mockReturnValue(true);
    const api = recorder();
    show(pack("demo/research"), api);
    await chooseRemove("demo/research");
    await waitFor(() => expect(api.removePack).toHaveBeenCalled());
    expect(confirm).toHaveBeenCalledWith(
      "Remove demo/research and everything it installed?",
    );
    expect(api.calls).toEqual(["remove demo/research (refuse)"]);
  });

  it("stops when a plugin can't be taken out, saying which already were", async () => {
    vi.spyOn(window, "confirm").mockReturnValue(true);
    const api = recorder({
      removals: [REFUSAL],
      wire: "the Tools document is locked",
      failAt: 2,
    });
    show(pack(WEB), api);
    await chooseRemove(WEB);
    await waitFor(() => expect(toast.error).toHaveBeenCalled());
    expect(api.calls).toEqual([`remove ${WEB} (refuse)`, ...UNWIRES.slice(0, 2)]);
    const [message, options] = vi.mocked(toast.error).mock.calls[0]!;
    expect(message).toBe(
      "Couldn't take web_fetch out of The Engineer's tools, so web wasn't removed. web_search was taken out of The Engineer's tools.",
    );
    expect(detailOf(options)).toBe("the Tools document is locked");
  });

  it("says which tools came out when the removal stops after taking them out", async () => {
    vi.spyOn(window, "confirm").mockReturnValue(true);
    const api = recorder({ removals: [REFUSAL, "the node is busy"] });
    show(pack(WEB), api);
    await chooseRemove(WEB);
    await waitFor(() => expect(toast.error).toHaveBeenCalled());
    expect(api.calls).toEqual([
      `remove ${WEB} (refuse)`,
      ...UNWIRES,
      `remove ${WEB} (refuse)`,
    ]);
    const [message, options] = vi.mocked(toast.error).mock.calls[0]!;
    expect(message).toBe(`Couldn't remove web. ${TAKEN_OUT}`);
    expect(detailOf(options)).toBe("the node is busy");
  });

  it("says who still uses its tools when the runtime refuses, its own words under Details", async () => {
    vi.spyOn(window, "confirm").mockReturnValue(true);
    const api = recorder({ removals: [REFUSAL] });
    show(pack(WEB), {
      ...api,
      listPackWireSources: async () => {
        throw new Error("the plugin store is busy");
      },
    });
    await chooseRemove(WEB);
    await waitFor(() => expect(toast.error).toHaveBeenCalled());
    expect(api.calls).toEqual([`remove ${WEB} (refuse)`]);
    const [message, options] = vi.mocked(toast.error).mock.calls[0]!;
    expect(message).toBe(
      "Couldn't remove web: The Engineer still uses tools from it. Take them out of The Engineer's tools, then remove web again.",
    );
    expect(message).not.toContain("did:");
    expect(detailOf(options)).toBe(REFUSAL);
    expect(screen.getByRole("list", { name: "Installed packs" })).toHaveTextContent(
      "web",
    );
  });
});

describe("removing a pack from a peer's Packs panel", () => {
  /* the panel lists and removes the desktop's own packs, whichever node it
     is open on */
  it("asks the desktop's own agents, not the peer's", async () => {
    const confirm = vi.spyOn(window, "confirm").mockReturnValue(true);
    const removePack = vi.fn(async (_name: string, _edited?: string) => {
      if (removePack.mock.calls.length === 1) throw new Error(REFUSAL);
    });
    const wirePack = vi.fn(async (_request: { agent: string }) => ({}));
    const peer: DeploymentView = {
      ...node,
      label: "Peer",
      nodeDid: "did:key:peer",
      local: false,
      agents: node.agents.map((a) => ({ ...a, agentDid: `peer-${a.agentDid}` })),
    };
    renderIn(
      testApp({
        api: {
          listInstalledPacks: async () => ({ packs: [pack(WEB)] }),
          readPackAccount: async () => {
            throw new Error("signed out");
          },
          listPackPluginSlots: async () => ({ plugins: [], profiles: [] }),
          listPackWireSources: async () => sources,
          removePack,
          wirePack,
        },
        snapshot: { bootstrap, client: { deployments: [node, peer] } },
      }),
      <PacksPanel deployment={peer} />,
    );
    fireEvent.click(await screen.findByRole("button", { name: "Remove" }));
    await waitFor(() => expect(removePack).toHaveBeenCalledTimes(2));
    expect(confirm).toHaveBeenCalledWith(QUESTION);
    expect(wirePack.mock.calls.map(([request]) => request.agent)).toEqual(
      TOOLS.map(() => "default"),
    );
  });
});

/* the runtime's words a failure keeps under Details */
function detailOf(options: unknown) {
  const description = (options as { description?: ReactElement<{ error: string }> })
    .description;
  return description?.props.error;
}
