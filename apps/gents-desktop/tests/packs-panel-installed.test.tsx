import { screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { PacksPanel } from "../src/ui/screens/agent/PacksPanel";
import { renderIn, testApp } from "./app-fixture";
import { deployment } from "./config-panel-wiring/fixtures";

vi.mock("sonner", () => ({ toast: { error: vi.fn(), success: vi.fn() } }));

function show(listInstalledPacks: () => Promise<unknown>, view = deployment) {
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
    <PacksPanel deployment={view} />,
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

  it("says what a pack installed, each kind linked to its section", async () => {
    const tag = ["gents:pack:research"];
    const view = {
      ...deployment,
      agents: [
        ...deployment.agents,
        { ...deployment.agents[0]!, agentDid: "r1", tags: tag },
        { ...deployment.agents[0]!, agentDid: "r2", tags: tag },
      ],
      skills: deployment.skills.map((s) => ({ ...s, tags: tag })),
    };
    show(
      async () => ({
        packs: [
          {
            pack: "demo/research",
            installed: "1.0.0",
            latest: null,
            outdated: false,
            error: null,
          },
          {
            pack: "demo/empty",
            installed: "1.0.0",
            latest: null,
            outdated: false,
            error: null,
          },
        ],
      }),
      view,
    );
    const agents = await screen.findByRole("link", { name: "2 agents" });
    expect(agents).toHaveAttribute("href", expect.stringContaining("/agents"));
    expect(agents.parentElement).toHaveTextContent("Adds 2 agents · 1 skill");
    expect(screen.getByRole("link", { name: "1 skill" })).toHaveAttribute(
      "href",
      expect.stringContaining("/skills"),
    );
    /* a pack that installed no documents says nothing of them */
    expect(screen.getAllByText(/^Adds/)).toHaveLength(1);
  });
});
