import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { PacksPanel } from "../src/ui/screens/agent/PacksPanel";
import { renderIn, testApp } from "./app-fixture";
import { deployment } from "./config-panel-wiring/fixtures";

vi.mock("sonner", () => ({ toast: { error: vi.fn(), success: vi.fn() } }));

const pack = (name: string) => ({
  namespace: "demo",
  name,
  description: `${name} pack`,
  latest: "1.0.0",
});

function show(
  searchPacks: (query: string, page: number) => Promise<unknown>,
  installed = ["demo/skills"],
) {
  return renderIn(
    testApp({
      api: {
        listInstalledPacks: async () => ({
          packs: installed.map((name) => ({
            pack: name,
            installed: "1.0.0",
            latest: null,
            outdated: null,
            error: null,
          })),
        }),
        readPackAccount: async () => {
          throw new Error("signed out");
        },
        listPackPluginSlots: async () => ({ plugins: [], profiles: [] }),
        searchPacks,
      },
    }),
    <PacksPanel deployment={deployment} />,
  );
}

describe("PacksPanel registry", () => {
  it("lists the registry's packs on opening, marking the ones installed", async () => {
    const searchPacks = vi.fn(async () => ({
      packs: [pack("skills"), pack("research")],
      has_more: false,
    }));
    show(searchPacks);
    expect(await screen.findByText("demo/research")).toBeVisible();
    expect(searchPacks).toHaveBeenCalledWith("", 1);
    /* only the pack not yet installed offers Install */
    expect(screen.getAllByRole("button", { name: "Install" })).toHaveLength(1);
  });

  it("searches what is typed once typing pauses, keeping only the latest answer", async () => {
    const answers = new Map<string, (value: unknown) => void>();
    const searchPacks = vi.fn(
      (query: string) =>
        new Promise((resolve) => {
          answers.set(query, resolve);
        }),
    );
    show(searchPacks, []);
    await waitFor(() => expect(searchPacks).toHaveBeenCalledWith("", 1));
    const user = userEvent.setup();
    await user.type(
      screen.getByRole("searchbox", { name: "Search the registry" }),
      "res",
    );
    await waitFor(() => expect(searchPacks).toHaveBeenCalledWith("res", 1));
    /* the opening answer lands after the typed one and is dropped */
    answers.get("res")!({ packs: [pack("research")], has_more: false });
    answers.get("")!({ packs: [pack("skills")], has_more: false });
    expect(await screen.findByText("demo/research")).toBeVisible();
    await new Promise((r) => setTimeout(r, 50));
    expect(screen.queryByText("demo/skills")).toBeNull();
    expect(searchPacks.mock.calls.filter(([q]) => q !== "" && q !== "res")).toEqual([]);
  });

  it("says when nothing matches", async () => {
    show(async () => ({ packs: [], has_more: false }));
    expect(await screen.findByText("The registry has no packs yet")).toBeVisible();
  });
});
