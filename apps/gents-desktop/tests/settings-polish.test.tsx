import { act, render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { describe, expect, it, vi } from "vitest";

import { ChipsRow, TagsRow } from "../src/ui/screens/agent/editors";
import { AgentHoverCard } from "../src/ui/screens/HoverCards";
import { deployment } from "./config-panel-wiring/fixtures";
import { renderIn, testApp } from "./app-fixture";

/* the card reads the agent's provider usage while open */
/* a Claude subscription, which reports its usage limits */
const onSubscription = {
  ...deployment,
  inferenceBackends: deployment.inferenceBackends.map((backend) =>
    backend.backendId === "backend-a"
      ? { ...backend, providerKind: "ClaudeCliSubscription" }
      : backend,
  ),
};

const cardApp = (usage: unknown[] = []) =>
  testApp({ api: { readProviderUsage: vi.fn().mockResolvedValue(usage) } });

describe("settings polish", () => {
  it("shows canonical skill picks as removable chips", () => {
    render(
      <ChipsRow
        id="context-skills"
        label="Skills"
        value={["skill-a"]}
        onChange={vi.fn()}
        items={[{ value: "skill-a", label: "Skill A" }]}
      />,
    );

    expect(screen.getByText("Skill A")).toBeVisible();
    expect(screen.getByRole("button", { name: "Remove Skill A" })).toBeVisible();
  });

  it("adds and removes free-form tags without changing their canonical values", async () => {
    const onChange = vi.fn();
    const view = render(
      <TagsRow
        id="behavior-tags"
        label="Tags"
        value={["existing"]}
        onChange={onChange}
      />,
    );
    const user = userEvent.setup();

    await user.type(screen.getByRole("textbox", { name: "Tags" }), "new-tag,");
    expect(onChange).toHaveBeenLastCalledWith(["existing", "new-tag"]);

    view.rerender(
      <TagsRow
        id="behavior-tags"
        label="Tags"
        value={["existing", "new-tag"]}
        onChange={onChange}
      />,
    );
    await user.click(screen.getByRole("button", { name: "Remove existing" }));
    expect(onChange).toHaveBeenLastCalledWith(["new-tag"]);
  });

  it("deduplicates tags within one pasted value", async () => {
    const onChange = vi.fn();
    render(<TagsRow id="behavior-tags" label="Tags" value={[]} onChange={onChange} />);
    const user = userEvent.setup();
    const input = screen.getByRole("textbox", { name: "Tags" });

    await user.click(input);
    await user.paste("coding,coding");

    expect(onChange).toHaveBeenCalledOnce();
    expect(onChange).toHaveBeenCalledWith(["coding"]);
  });

  it("explains agent readiness and links to the canonical settings route", async () => {
    renderIn(
      cardApp(),
      <AgentHoverCard deployment={deployment} agentId="default">
        <button type="button">Default agent</button>
      </AgentHoverCard>,
    );
    const user = userEvent.setup();

    await user.hover(screen.getByRole("button", { name: "Default agent" }));
    const card = await screen.findByTestId("agent-hover-card");
    expect(card).toHaveTextContent("StatusEnabled");
    expect(screen.getByRole("link", { name: "Settings" })).toHaveAttribute(
      "href",
      "/agents/did%3Akey%3Az6MkAgent/agents/default",
    );
  });

  it("marks the model with its provider, found through the agent's profile", async () => {
    const onClaude = {
      ...deployment,
      inferenceBackends: deployment.inferenceBackends.map((backend) =>
        backend.backendId === "backend-a"
          ? { ...backend, providerKind: "AnthropicApiKey" }
          : backend,
      ),
    };
    renderIn(
      cardApp(),
      <AgentHoverCard deployment={onClaude} agentId="default">
        <button type="button">Default agent</button>
      </AgentHoverCard>,
    );
    await userEvent
      .setup()
      .hover(screen.getByRole("button", { name: "Default agent" }));
    const card = await screen.findByTestId("agent-hover-card");
    expect(card.querySelector("img")).toHaveAttribute("src", "/logos/claude.svg");
  });

  it("names no company for a bare local endpoint", async () => {
    renderIn(
      cardApp(),
      <AgentHoverCard deployment={deployment} agentId="default">
        <button type="button">Default agent</button>
      </AgentHoverCard>,
    );
    await userEvent
      .setup()
      .hover(screen.getByRole("button", { name: "Default agent" }));
    const card = await screen.findByTestId("agent-hover-card");
    expect(card.querySelector("img")).toBeNull();
  });

  it("holds the Limits line while a subscription's usage is read, and drops it when none comes", async () => {
    let answer!: (usage: unknown[]) => void;
    const app = testApp({
      api: {
        readProviderUsage: vi.fn(
          () =>
            new Promise((resolve) => {
              answer = resolve;
            }),
        ),
      },
    });
    renderIn(
      app,
      <AgentHoverCard deployment={onSubscription} agentId="default">
        <button type="button">Default agent</button>
      </AgentHoverCard>,
    );
    await userEvent
      .setup()
      .hover(screen.getByRole("button", { name: "Default agent" }));
    const card = await screen.findByTestId("agent-hover-card");
    const held = within(card).getByText("Limits").nextElementSibling!;
    expect(held.querySelector('[aria-busy="true"]')).not.toBeNull();

    await act(async () => answer([]));
    await waitFor(() => expect(within(card).queryByText("Limits")).toBeNull());
  });

  it("keeps no Limits line for a backend that reports no usage", async () => {
    const readProviderUsage = vi.fn(() => new Promise(() => {}));
    renderIn(
      testApp({ api: { readProviderUsage } }),
      <AgentHoverCard deployment={deployment} agentId="default">
        <button type="button">Default agent</button>
      </AgentHoverCard>,
    );
    await userEvent
      .setup()
      .hover(screen.getByRole("button", { name: "Default agent" }));
    const card = await screen.findByTestId("agent-hover-card");
    expect(within(card).queryByText("Limits")).toBeNull();
  });

  it("shows the agent's closest limit, amber once it nears it", async () => {
    const observedAt = new Date().toISOString();
    const window = (label: string, usedPct: number) => ({
      label,
      windowMinutes: 300,
      usedPct,
      resetsAt: null,
      source: "header",
      observedAt,
      lastKnown: false,
    });
    renderIn(
      cardApp([
        {
          label: "Work",
          provider: "Claude",
          backendIds: ["backend-a"],
          windows: [window("5h", 81), window("7d", 38)],
          plan: null,
          note: null,
          readAt: null,
          readError: null,
          read: "read",
        },
      ]),
      <AgentHoverCard deployment={onSubscription} agentId="default">
        <button type="button">Default agent</button>
      </AgentHoverCard>,
    );
    await userEvent
      .setup()
      .hover(screen.getByRole("button", { name: "Default agent" }));
    const card = await screen.findByTestId("agent-hover-card");
    const limit = await within(card).findByText("5h 81%");
    expect(limit).toHaveClass("text-warning");
    expect(limit.closest("[title]")).toHaveAttribute("title", "5h 81% · 7d 38%");
  });
});
