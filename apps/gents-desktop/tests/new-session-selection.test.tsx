import { act, renderHook } from "@testing-library/react";
import { useRef, useState } from "react";
import { describe, expect, it, vi } from "vitest";
import type {
  DeploymentView,
  DesktopApiAdapter,
  DesktopClientSnapshot,
  DesktopSessionSnapshot,
} from "@source-inc/gents-desktop-client";

import { applyFleetSnapshot } from "../src/hooks/fleetStore";
import { readSession, writeSession } from "../src/hooks/sessionStore";
import { admittingProjection, shellStores } from "./shell-fixture";
import { reconcileSelection } from "../src/hooks/selectionReconcile";
import { createSelectionActions } from "../src/hooks/selectionActions";
import { createSelectionStore, useSelection } from "../src/hooks/selectionStore";
import { createChatActions } from "../src/hooks/chatActions";

const initialDeployment = {
  nodeDid: "agent",
  node: { nodeDid: "agent", defaultAgentId: "coding" },
  agents: [
    { agentId: "coding", enabled: true, isDefault: true },
    { agentId: "setup", enabled: true, isDefault: false },
  ],
  sessions: [
    { sessionId: "first-setup", agentId: "setup" },
    { sessionId: "first-coding", agentId: "coding" },
  ],
  mailboxItems: [],
} as unknown as DeploymentView;

function useHarness(
  deployment: DeploymentView | null,
  initialSession: string | null,
  initialNode: string | null = "agent",
) {
  const [stores] = useState(() =>
    shellStores({
      selection: {
        nodeDid: initialNode,
        agentId: "setup",
        sessionId: initialSession,
      },
    }),
  );
  const store = stores.selection;
  const deployments = deployment ? [deployment] : [];
  /* the read the shell would have published */
  applyFleetSnapshot(stores.fleet, {
    bootstrap: {},
    client: { deployments },
  } as unknown as DesktopClientSnapshot);
  const current = useSelection(store);
  const sendChatMessage = useRef(
    vi.fn(async () => ({
      nodeDid: "agent",
      agentId: "setup",
      sessionId: "new-setup",
      requestId: "new-request",
    })),
  ).current;
  const api = useRef({ sendChatMessage }).current as unknown as DesktopApiAdapter;
  const [route] = useState(() => createSelectionActions({ stores }));
  const [actions] = useState(() => ({
    ...route,
    ...createChatActions({
      api,
      stores,
      project: () => admittingProjection("setup"),
      refreshSession: async () => null,
      refreshSnapshot: async () => {},
      reportFailure: vi.fn(),
    }),
  }));
  useState(() => reconcileSelection(stores, actions.selectNode));
  return {
    nodeDid: current.nodeDid,
    selected: current.sessionId,
    agentId: current.agentId,
    store,
    actions,
    route,
    sendChatMessage,
  };
}

describe("explicit session selection", () => {
  it("initializes an empty node selection through the route owner", () => {
    const { result } = renderHook(() =>
      useHarness(initialDeployment, "old-session", null),
    );
    expect(result.current.nodeDid).toBe("agent");
    expect(result.current.selected).toBeNull();
  });

  it("preserves node and session selection across a temporary missing snapshot", () => {
    const { result, rerender } = renderHook(
      ({ deployment }) => useHarness(deployment, "first-setup"),
      { initialProps: { deployment: initialDeployment as DeploymentView | null } },
    );
    rerender({ deployment: null });
    expect(result.current.nodeDid).toBe("agent");
    expect(result.current.selected).toBe("first-setup");
  });
  it("keeps the agent and an armed mailbox reply across a temporary missing snapshot", () => {
    const { result, rerender } = renderHook(
      ({ deployment }) => useHarness(deployment, null),
      { initialProps: { deployment: initialDeployment as DeploymentView | null } },
    );
    const reply = {
      itemId: "item-1",
      nodeDid: "agent",
      agentId: "setup",
      sessionId: null,
    };
    act(() =>
      result.current.store.setState({ mailboxRoute: reply, composingFor: "agent" }),
    );
    /* a read while the client restarts lists no nodes */
    rerender({ deployment: null });
    expect(result.current.agentId).toBe("setup");
    expect(result.current.store.getState().mailboxRoute).toEqual(reply);

    rerender({ deployment: initialDeployment });
    expect(result.current.agentId).toBe("setup");
    expect(result.current.store.getState().mailboxRoute).toEqual(reply);
  });

  it("uses the node default instead of a conflicting marked default", () => {
    const deployment = {
      ...initialDeployment,
      agents: initialDeployment.agents.map((agent) => ({
        ...agent,
        isDefault: agent.agentId === "setup",
      })),
    };
    const { result } = renderHook(() => useHarness(deployment, "first-setup"));
    act(() => result.current.actions.startNewSession());
    expect(result.current.agentId).toBe("coding");
    expect(result.current.selected).toBeNull();
  });

  it("uses the canonical enabled fallback and clears selection when none exists", () => {
    const deployment = {
      ...initialDeployment,
      node: { ...initialDeployment.node, defaultAgentId: null },
      agents: [
        { ...initialDeployment.agents[0], isDefault: false, enabled: false },
        { ...initialDeployment.agents[1], isDefault: false },
      ],
    };
    const { result, rerender } = renderHook(({ value }) => useHarness(value, null), {
      initialProps: { value: deployment },
    });
    act(() => result.current.actions.startNewSession());
    expect(result.current.agentId).toBe("setup");
    rerender({ value: { ...deployment, agents: [] } });
    act(() => result.current.actions.startNewSession());
    expect(result.current.agentId).toBeNull();
  });
  it("resets session selection on explicit node navigation, not observation", () => {
    const store = createSelectionStore({
      nodeDid: "agent",
      agentId: "setup",
      sessionId: "first-setup",
    });
    const stores = { ...shellStores(), selection: store };
    writeSession(stores.session, {
      sessionId: "first-setup",
    } as DesktopSessionSnapshot);
    const route = createSelectionActions({ stores });
    route.selectNode("agent");
    expect(store.getState().sessionId).toBe("first-setup");
    expect(readSession(stores.session)).not.toBeNull();
    route.selectNode("another-node");
    expect(store.getState().sessionId).toBeNull();
    expect(readSession(stores.session)).toBeNull();
  });

  it("starts a new session without the error the session left behind", () => {
    const stores = shellStores();
    applyFleetSnapshot(stores.fleet, {
      bootstrap: {},
      client: { deployments: [initialDeployment] },
    } as unknown as DesktopClientSnapshot);
    stores.client.setState({ error: "Session not found" });
    createSelectionActions({ stores }).startNewSession();
    expect(stores.client.getState().error).toBeNull();
  });

  it("keeps a fresh composer after choosing Setup and creates a new session on send", async () => {
    const { result, rerender } = renderHook(
      ({ deployment }) => useHarness(deployment, "first-setup"),
      { initialProps: { deployment: initialDeployment } },
    );
    act(() => result.current.actions.startNewSession());
    expect(result.current.selected).toBeNull();
    act(() => result.current.route.selectAgent("setup"));
    expect(result.current.agentId).toBe("setup");
    expect(result.current.selected).toBeNull();
    rerender({
      deployment: { ...initialDeployment, sessions: [...initialDeployment.sessions] },
    });
    expect(result.current.selected).toBeNull();
    await act(async () => {
      await result.current.actions.sendMessage("new Setup chat");
    });
    expect(result.current.sendChatMessage).toHaveBeenCalledWith(
      expect.objectContaining({
        sessionId: null,
        agentId: "setup",
        content: "new Setup chat",
      }),
    );
    expect(result.current.selected).toBe("new-setup");
    rerender({
      deployment: { ...initialDeployment, sessions: [...initialDeployment.sessions] },
    });
    expect(result.current.selected).toBe("new-setup");
  });

  it("does not select the first session when a deployment first becomes available", () => {
    const { result, rerender } = renderHook(
      ({ deployment }: { deployment: DeploymentView | null }) =>
        useHarness(deployment, null),
      { initialProps: { deployment: null as DeploymentView | null } },
    );
    rerender({ deployment: initialDeployment });
    expect(result.current.selected).toBeNull();
  });

  it("preserves an explicit session across a temporarily missing session row", () => {
    const { result, rerender } = renderHook(
      ({ deployment }) => useHarness(deployment, "first-setup"),
      { initialProps: { deployment: initialDeployment } },
    );
    rerender({ deployment: { ...initialDeployment, sessions: [] } });
    expect(result.current.selected).toBe("first-setup");
  });
});
