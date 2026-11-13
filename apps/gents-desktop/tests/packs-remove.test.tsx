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
  editedDocuments,
  editedQuestion,
} from "../src/ui/screens/packs/removal";
import { resetDefaultPacks } from "../src/ui/screens/packs/useDefaultPacks";
import { renderIn, testApp, withFlags } from "./app-fixture";
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
  "1 of gents/web's documents were edited since it was installed: Tools/web-researcher-tools. Overwrite replaces them with the pack's versions (a removal deletes them); keep leaves them as edited.";

/* the runtime's refusal while the Engineer's tools still select a plugin */
const REFUSAL =
  'cannot remove plugin gents/web_search: agent "default" still selects it through Tools "tools-a"; remove that reference first with `gents config pack wire gents/web_search --agent default --remove --home <node-home>`';

beforeEach(() => {
  resetInstalledPacks();
  resetDefaultPacks();
  vi.mocked(toast.error).mockClear();
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

  it("reads which of the pack's documents were edited from the runtime's refusal", () => {
    expect(editedDocuments(EDITED)).toEqual(["Tools/web-researcher-tools"]);
    expect(editedDocuments(REFUSAL)).toBeNull();
    expect(editedQuestion(WEB, ["Tools/web-researcher-tools"])).toBe(
      "Tools/web-researcher-tools was changed since web was installed. Removing web deletes it with the rest of the pack.",
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
    withFlags({ packs: true });
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

  /* each call in the order it was made */
  function recorder(fail: { wire?: string; remove?: string } = {}) {
    const calls: string[] = [];
    return {
      calls,
      wirePack: vi.fn(async (request: { plugin: string; agent: string }) => {
        calls.push(`unwire ${request.plugin} from ${request.agent}`);
        if (fail.wire) throw new Error(fail.wire);
        return {};
      }),
      removePack: vi.fn(async (name: string, edited?: string) => {
        calls.push(edited ? `remove ${name} (${edited})` : `remove ${name}`);
        if (fail.remove && !edited) throw new Error(fail.remove);
      }),
    };
  }

  it("names who uses its tools, takes them out of their tools, then removes it", async () => {
    const confirm = vi.spyOn(window, "confirm").mockReturnValue(true);
    const api = recorder();
    show(pack(WEB), api);
    await chooseRemove(WEB);
    await waitFor(() => expect(api.removePack).toHaveBeenCalled());
    expect(confirm).toHaveBeenCalledWith(QUESTION);
    expect(api.calls).toEqual([
      ...TOOLS.map((plugin) => `unwire ${plugin} from default`),
      `remove ${WEB}`,
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

  it("asks before deleting the pack's edited documents with it, then removes it", async () => {
    const confirm = vi.spyOn(window, "confirm").mockReturnValue(true);
    const api = recorder({ remove: EDITED });
    show(pack(WEB), api);
    await chooseRemove(WEB);
    await waitFor(() => expect(api.calls).toContain(`remove ${WEB} (overwrite)`));
    expect(confirm).toHaveBeenLastCalledWith(
      editedQuestion(WEB, ["Tools/web-researcher-tools"]),
    );
    expect(api.calls.at(-2)).toBe(`remove ${WEB}`);
  });

  it("keeps the pack when deleting its edited documents is declined", async () => {
    vi.spyOn(window, "confirm").mockReturnValueOnce(true).mockReturnValueOnce(false);
    const api = recorder({ remove: EDITED });
    show(pack(WEB), api);
    await chooseRemove(WEB);
    await waitFor(() => expect(window.confirm).toHaveBeenCalledTimes(2));
    expect(api.calls).not.toContain(`remove ${WEB} (overwrite)`);
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
    expect(api.calls).toEqual(["remove demo/research"]);
  });

  it("stops before removing when a plugin can't be taken out", async () => {
    vi.spyOn(window, "confirm").mockReturnValue(true);
    const api = recorder({ wire: "the Tools document is locked" });
    show(pack(WEB), api);
    await chooseRemove(WEB);
    await waitFor(() => expect(toast.error).toHaveBeenCalled());
    expect(api.calls).toEqual([`unwire ${TOOLS[0]} from default`]);
    const [message, options] = vi.mocked(toast.error).mock.calls[0]!;
    expect(message).toBe(
      "Couldn't take web_search out of The Engineer's tools, so web wasn't removed.",
    );
    expect(detailOf(options)).toBe("the Tools document is locked");
  });

  it("says who still uses its tools when the runtime refuses, its own words under Details", async () => {
    vi.spyOn(window, "confirm").mockReturnValue(true);
    const api = recorder({ remove: REFUSAL });
    show(pack(WEB), api);
    await chooseRemove(WEB);
    await waitFor(() => expect(toast.error).toHaveBeenCalled());
    expect(api.calls.at(-1)).toBe(`remove ${WEB}`);
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

/* the runtime's words a failure keeps under Details */
function detailOf(options: unknown) {
  const description = (options as { description?: ReactElement<{ error: string }> })
    .description;
  return description?.props.error;
}
