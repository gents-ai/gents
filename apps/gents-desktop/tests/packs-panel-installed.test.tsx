import { screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { PacksPanel } from "../src/ui/screens/agent/PacksPanel";
import { renderIn, testApp } from "./app-fixture";
import { deployment } from "./config-panel-wiring/fixtures";

vi.mock("sonner", () => ({ toast: { error: vi.fn(), success: vi.fn() } }));

function show(listInstalledPacks: () => Promise<unknown>) {
  return renderIn(
    testApp({
      api: {
        listInstalledPacks,
        readPackAccount: async () => {
          throw new Error("signed out");
        },
        listPackPluginSlots: async () => ({ plugins: [], profiles: [] }),
        listPackWireSources: async () => ({ plugins: [], target_packs: [] }),
      },
    }),
    <PacksPanel deployment={deployment} />,
  );
}

describe("PacksPanel installed packs", () => {
  it("lists a pack whose registry lookup failed, saying so", async () => {
    show(async () => ({
      packs: [
        {
          pack: "fixture/local",
          installed: "0.1.0",
          latest: null,
          outdated: null,
          error: "registry unreachable",
        },
      ],
    }));
    expect(await screen.findByText("fixture/local")).toBeInTheDocument();
    const note = screen.getByText("0.1.0 · couldn't check for updates");
    expect(note).toHaveAttribute("title", "registry unreachable");
    expect(screen.queryByText("No packs installed")).toBeNull();
  });

  it("says a failed read failed, not that nothing is installed", async () => {
    show(async () => {
      throw new Error("the local node could not be opened");
    });
    expect(
      await screen.findByText("Couldn't read the installed packs"),
    ).toBeInTheDocument();
    expect(screen.getByText("the local node could not be opened")).toBeInTheDocument();
    expect(screen.queryByText("No packs installed")).toBeNull();
  });
});
