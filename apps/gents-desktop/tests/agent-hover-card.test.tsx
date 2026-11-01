import { screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it } from "vitest";

import { fleetNodes } from "../src/ui/lib/scope";
import { NodeHoverCard } from "../src/ui/screens/HoverCards";
import { deployment } from "./config-panel-wiring/fixtures";
import { node, renderIn, testApp } from "./app-fixture";

function cardFor(nodeDid: string, more: Record<string, unknown> = {}) {
  const app = testApp({
    snapshot: {
      bootstrap: {
        initNodeDid: "did:key:home",
        initToolRoot: "/Users/me/work",
        initToolCeiling: "readwrite",
      },
      client: { deployments: [node({ nodeDid, source: "enrollment", ...more })] },
    },
  });
  const [shown] = fleetNodes(app.stores.fleet.getState());
  renderIn(
    app,
    <NodeHoverCard deployment={shown!}>
      <button type="button">avatar</button>
    </NodeHoverCard>,
  );
}

describe("a node's hover card", () => {
  it("shows this machine's tool root on the node this machine runs", async () => {
    cardFor("did:key:home");
    await userEvent.hover(screen.getByRole("button", { name: "avatar" }));
    expect(
      await screen.findByText("/Users/me/work", {}, { timeout: 2000 }),
    ).toBeVisible();
  });

  it("does not lend this machine's tool root to a remote node", async () => {
    cardFor("did:key:remote");
    await userEvent.hover(screen.getByRole("button", { name: "avatar" }));
    expect(await screen.findByText("Agents", {}, { timeout: 2000 })).toBeVisible();
    expect(screen.queryByText("/Users/me/work")).toBeNull();
  });

  it("shows a node's inference as one logo per provider, its accounts named", async () => {
    const backend = (name: string, providerKind: string) => ({
      ...deployment.inferenceBackends[0]!,
      backendId: name,
      name,
      providerKind,
      endpoint: null,
    });
    cardFor("did:key:remote", {
      inferenceBackends: [
        backend("Claude Max", "ClaudeCliSubscription"),
        backend("Work key", "AnthropicApiKey"),
        backend("Codex", "ChatGptCodex"),
      ],
    });
    await userEvent.hover(screen.getByRole("button", { name: "avatar" }));
    const logos = await screen.findByRole(
      "img",
      { name: "Claude Max, Work key, Codex" },
      { timeout: 2000 },
    );
    expect(logos.querySelectorAll("img")).toHaveLength(2);
  });

  it("says None when a node has no inference", async () => {
    cardFor("did:key:remote", { inferenceBackends: [] });
    await userEvent.hover(screen.getByRole("button", { name: "avatar" }));
    expect(await screen.findByText("None", {}, { timeout: 2000 })).toBeVisible();
  });
});
