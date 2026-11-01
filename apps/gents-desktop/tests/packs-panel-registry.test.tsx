import { screen, waitFor, within } from "@testing-library/react";
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
  packInfo: (pack: string) => Promise<unknown> = async () => ({ versions: [] }),
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
        packInfo,
      },
    }),
    <PacksPanel deployment={deployment} />,
  );
}

describe("PacksPanel registry", () => {
  it("shows a pack's downloads and keywords, and opens its page under its line", async () => {
    const searchPacks = vi.fn(async () => ({
      packs: [{ ...pack("mailbox"), downloads: 1278, keywords: ["example", "mail"] }],
      has_more: false,
    }));
    const packInfo = vi.fn(async () => ({
      namespace: "demo",
      name: "mailbox",
      latest: "1.1.0",
      owner: "admin",
      homepage: "https://example.com/mailbox",
      plugins: ["triage"],
      versions: [
        { version: "1.0.0", published_at: "2026-09-01T10:00:00Z", size_bytes: 900 },
        { version: "1.0.1", yanked: true },
        { version: "1.1.0", published_at: "2026-09-30T15:06:03Z", size_bytes: 1647 },
      ],
    }));
    show(searchPacks, [], packInfo);
    const user = userEvent.setup();
    const line = await screen.findByRole("button", { name: /demo\/mailbox/ });
    expect(screen.getByText("1.3K")).toBeVisible();
    expect(line).toHaveAttribute("aria-expanded", "false");

    await user.click(line);
    expect(packInfo).toHaveBeenCalledWith("demo/mailbox");
    const facts = await screen.findByText(/by admin/);
    expect(facts).toHaveTextContent(
      /by admin · 1\.1\.0, published .+ · 1\.6 KB · 1 plugin: triage · 1 older version · Homepage/,
    );
    expect(within(facts).getByRole("link", { name: "Homepage" })).toHaveAttribute(
      "href",
      "https://example.com/mailbox",
    );
  });

  it("searches a keyword chosen from a pack", async () => {
    const searchPacks = vi.fn(async () => ({
      packs: [{ ...pack("mailbox"), keywords: ["example"] }],
      has_more: false,
    }));
    show(searchPacks, []);
    const user = userEvent.setup();
    await user.click(await screen.findByRole("button", { name: /demo\/mailbox/ }));
    await user.click(screen.getAllByRole("button", { name: "example" })[0]!);
    expect(screen.getByRole("searchbox", { name: "Search the registry" })).toHaveValue(
      "example",
    );
    await waitFor(() => expect(searchPacks).toHaveBeenCalledWith("example", 1));
  });

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
