import { act, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { TooltipProvider } from "@gents/ui/components/tooltip";
import { describe, expect, it, vi } from "vitest";

import { AgentScreen } from "../src/ui/screens/agent/AgentScreen";
import { node, renderIn, testApp } from "./app-fixture";

describe("switching nodes on the configuration screen", () => {
  it("drops a draft opened on the previous node", async () => {
    const app = testApp({
      deployments: [node({ nodeDid: "did:key:a" }), node({ nodeDid: "did:key:b" })],
    });
    const view = renderIn(
      app,
      <TooltipProvider>
        <AgentScreen nodeDid="did:key:a" section="agents" />
      </TooltipProvider>,
    );
    await userEvent.click(screen.getByRole("button", { name: "New agent" }));
    expect(screen.getByRole("button", { name: "Agents" })).toBeInTheDocument();

    act(() =>
      view.rerender(
        <TooltipProvider>
          <AgentScreen nodeDid="did:key:b" section="agents" />
        </TooltipProvider>,
      ),
    );
    expect(screen.queryByRole("button", { name: "Agents" })).toBeNull();
    expect(screen.getByRole("button", { name: "New agent" })).toBeInTheDocument();
  });
});

describe("moving within the configuration screen", () => {
  /* where each scroll to the top landed */
  function scrolls() {
    const at: Element[] = [];
    const original = HTMLElement.prototype.scrollTo;
    HTMLElement.prototype.scrollTo = function (this: HTMLElement) {
      at.push(this);
    } as typeof HTMLElement.prototype.scrollTo;
    return { at, restore: () => (HTMLElement.prototype.scrollTo = original) };
  }
  const page = (nodeDid: string, section: string) => (
    <TooltipProvider>
      <AgentScreen nodeDid={nodeDid} section={section} />
    </TooltipProvider>
  );
  /* the scroll area holding the section, not the sidebar beside it */
  const content = () =>
    screen
      .getByRole("combobox", { name: "Section" })
      .closest("[data-slot=scroll-area-viewport]");

  it("starts a new section, or another node's, at the top of the page", () => {
    const app = testApp({
      deployments: [node({ nodeDid: "did:key:a" }), node({ nodeDid: "did:key:b" })],
    });
    const scrolled = scrolls();
    try {
      const view = renderIn(app, page("did:key:a", "skills"));
      scrolled.at.length = 0;
      act(() => view.rerender(page("did:key:a", "agents")));
      expect(scrolled.at).toEqual([content()]);
      scrolled.at.length = 0;
      act(() => view.rerender(page("did:key:b", "agents")));
      expect(scrolled.at).toEqual([content()]);
    } finally {
      scrolled.restore();
    }
  });
});

describe("the inference route", () => {
  it("lists what the Providers page lists, profiles under their backends", async () => {
    const app = testApp({ deployments: [node({ nodeDid: "did:key:a" })] });
    renderIn(
      app,
      <TooltipProvider>
        <AgentScreen nodeDid="did:key:a" section="inference" />
      </TooltipProvider>,
    );
    const [toggle] = screen.getAllByRole("button", { name: /^Show what .* serves$/ });
    await userEvent.click(toggle!);
    expect(screen.getAllByText("Add profile").length).toBeGreaterThan(0);
  });
});

describe("turning a task off from its row", () => {
  it("patches only enabled, leaving the rest of the task as stored", async () => {
    const patchConfigComponents = vi.fn().mockResolvedValue(undefined);
    const saveTaskConfig = vi.fn().mockResolvedValue(undefined);
    const app = testApp({
      api: { patchConfigComponents, saveTaskConfig, fetchClientSnapshot: vi.fn() },
      deployments: [node({ nodeDid: "did:key:a" })],
    });
    renderIn(
      app,
      <TooltipProvider>
        <AgentScreen nodeDid="did:key:a" section="tasks" />
      </TooltipProvider>,
    );
    await userEvent.click(screen.getByRole("switch", { name: "Task A is on" }));
    await waitFor(() => expect(patchConfigComponents).toHaveBeenCalledOnce());
    expect(patchConfigComponents).toHaveBeenCalledWith({
      nodeDid: "did:key:a",
      patches: [{ collection: "Task", id: "task-a", changes: { enabled: false } }],
    });
    expect(saveTaskConfig).not.toHaveBeenCalled();
  });
});
