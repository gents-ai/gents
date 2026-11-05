import { screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { fleetNodes } from "../src/ui/lib/scope";
import { AgentHoverCard, NodeHoverCard } from "../src/ui/screens/HoverCards";
import { avatarSrc } from "@/lib/avatar";
import { deployment } from "./config-panel-wiring/fixtures";
import { node, renderIn, renderRouted, testApp } from "./app-fixture";

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

  it("draws the node's saved portrait", async () => {
    cardFor("did:key:remote", {
      nodeConfig: { node_did: "did:key:remote", avatar: "12" },
    });
    await userEvent.hover(screen.getByRole("button", { name: "avatar" }));
    await screen.findByText("Agents", {}, { timeout: 2000 });
    expect(document.querySelector(`img[src="${avatarSrc("12")}"]`)).not.toBeNull();
  });

  it("says None when a node has no inference", async () => {
    cardFor("did:key:remote", { inferenceBackends: [] });
    await userEvent.hover(screen.getByRole("button", { name: "avatar" }));
    expect(await screen.findByText("None set up", {}, { timeout: 2000 })).toBeVisible();
  });

  it("leads from its lines to the node's sections, and copies its id", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    vi.stubGlobal("navigator", { ...navigator, clipboard: { writeText } });
    cardFor("did:key:remote");
    await userEvent.hover(screen.getByRole("button", { name: "avatar" }));
    await screen.findByText("Agents", {}, { timeout: 2000 });
    const links = screen.getAllByRole("link").map((a) => a.getAttribute("href"));
    for (const section of ["agents", "folders", "tasks", "inference"])
      expect(links.some((h) => h?.endsWith(`/${section}`))).toBe(true);
    /* a line is one link over its label and value */
    expect(screen.getByRole("link", { name: /^Tasks/ })).toHaveAttribute(
      "href",
      expect.stringMatching(/\/tasks$/),
    );
    /* the same last line as an agent's card */
    expect(screen.getByRole("button", { name: "New session" })).toBeVisible();
    expect(screen.getByRole("link", { name: "Settings" })).toHaveAttribute(
      "href",
      expect.stringMatching(/\/agent$/),
    );
    await userEvent.click(screen.getByRole("button", { name: "Copy node id" }));
    expect(writeText).toHaveBeenCalledWith("did:key:remote");
    vi.unstubAllGlobals();
  });
});

describe("an agent's hover card", () => {
  it("starts a new session with the agent on its node", async () => {
    const app = testApp({ deployments: [node()] });
    const [shown] = fleetNodes(app.stores.fleet.getState());
    const view = renderRouted(app, { name: "sessions" }, () => (
      <AgentHoverCard deployment={shown!} agentDid="default">
        <button type="button">agent</button>
      </AgentHoverCard>
    ));
    await userEvent.hover(screen.getByRole("button", { name: "agent" }));
    await userEvent.click(
      await screen.findByRole("button", { name: "New session" }, { timeout: 2000 }),
    );
    await waitFor(() =>
      expect(view.nav().route).toMatchObject({ name: "session", sessionId: null }),
    );
    expect(app.stores.selection.getState()).toMatchObject({
      nodeDid: shown!.nodeDid,
      agentDid: "default",
    });
  });

  it("counts the tasks that run with the agent, and leaves the line out at none", async () => {
    const task = (taskId: string, agentDid: string) => ({
      ...deployment.tasks[0]!,
      taskId,
      agentDid,
    });
    const app = testApp({
      deployments: [
        node({
          tasks: [task("a", "default"), task("b", "default"), task("c", "other")],
        }),
      ],
    });
    const [shown] = fleetNodes(app.stores.fleet.getState());
    renderIn(
      app,
      <AgentHoverCard deployment={shown!} agentDid="default">
        <button type="button">agent</button>
      </AgentHoverCard>,
    );
    await userEvent.hover(screen.getByRole("button", { name: "agent" }));
    const card = await screen.findByTestId("agent-hover-card", {}, { timeout: 2000 });
    expect(card).toHaveTextContent("Tasks2");
  });
});
