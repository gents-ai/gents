import { act, render, screen } from "@testing-library/react";
import { TooltipProvider } from "@gents/ui/components/tooltip";
import { describe, expect, it } from "vitest";

import { createRegistry } from "../src/ui/app/registry";
import { RouteScreenOutlet, routeScreens } from "../src/ui/app/routeScreens";
import { AgentScreen } from "../src/ui/screens/agent/AgentScreen";
import { agentSections } from "../src/ui/screens/agent/sections";
import { node, renderIn, testApp } from "./app-fixture";

describe("a registry", () => {
  it("keeps registration order, replaces by id in place, and removes what it added", () => {
    const registry = createRegistry<{ id: string; v: number }>();
    registry.register({ id: "a", v: 1 });
    registry.register({ id: "b", v: 1 });
    const replaced = { id: "a", v: 2 };
    const remove = registry.register(replaced);
    expect(registry.list()).toEqual([replaced, { id: "b", v: 1 }]);
    remove();
    expect(registry.list().map((x) => x.id)).toEqual(["b"]);
  });

  it("shows a contribution registered after the area first drew", () => {
    const registry = createRegistry<{ id: string; label: string }>();
    function Area() {
      return (
        <p>
          {registry
            .useList()
            .map((x) => x.label)
            .join(",") || "empty"}
        </p>
      );
    }
    render(<Area />);
    expect(screen.getByText("empty")).toBeInTheDocument();
    act(() => void registry.register({ id: "late", label: "Late" }));
    expect(screen.getByText("Late")).toBeInTheDocument();
  });
});

describe("the route outlet", () => {
  it("draws the screen registered for the route's name, when one is", () => {
    render(<RouteScreenOutlet route={{ name: "mailbox" }} />);
    expect(screen.queryByText("Inbox screen")).toBeNull();
    let remove = () => {};
    act(() => {
      remove = routeScreens.register({
        id: "mailbox",
        render: () => <p>Inbox screen</p>,
      });
    });
    expect(screen.getByText("Inbox screen")).toBeInTheDocument();
    act(remove);
    expect(screen.queryByText("Inbox screen")).toBeNull();
  });
});

describe("a contributed configuration section", () => {
  it("joins the sidebar in its group and draws its panel on its route", () => {
    const remove = agentSections.register({
      id: "audit",
      group: "Extensions",
      label: "Audit log",
      icon: () => null,
      count: () => 3,
      Panel: ({ deployment }) => <p>Audit of {deployment.agentDid}</p>,
    });
    renderIn(
      testApp({ deployments: [node({ agentDid: "did:key:a" })] }),
      <TooltipProvider>
        <AgentScreen agentDid="did:key:a" section="audit" />
      </TooltipProvider>,
    );
    expect(screen.getByText("Audit of did:key:a")).toBeInTheDocument();
    expect(screen.getAllByText("Extensions").length).toBeGreaterThan(0);
    remove();
  });
});
