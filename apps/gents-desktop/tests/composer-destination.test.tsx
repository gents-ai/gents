import { act, renderHook, screen } from "@testing-library/react";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { goalDeliveryStorage } from "./goal-delivery-storage";
import { acceptedChatReply } from "./chat-delivery-fixture";
import { TooltipProvider } from "@gents/ui/components/tooltip";
import { useComposerSend } from "../src/ui/screens/session/useComposerSend";
import { ActiveNodeLabel } from "../src/ui/app/ActiveNodeLabel";
import { AgentScreen } from "../src/ui/screens/agent/AgentScreen";
import { deployment } from "./config-panel-wiring/fixtures";
import { node, renderIn, testApp, withApp } from "./app-fixture";

beforeEach(() => vi.stubGlobal("localStorage", goalDeliveryStorage()));
afterEach(() => vi.unstubAllGlobals());

const remote = "did:key:remote";
function fixture() {
  return testApp({
    api: { fetchOperationsSnapshot: vi.fn().mockResolvedValue(null) },
    deployments: [
      node(),
      node({
        nodeDid: remote,
        source: "enrollment",
        label: "studio-1",
        node: { ...deployment.node, nodeDid: remote, displayName: "Amy" },
      }),
    ],
    selection: { nodeDid: deployment.nodeDid },
  });
}

describe("composer destination", () => {
  it("shows the remote name, source and identity in the composer", () => {
    const app = fixture();
    app.actions.selectNode(remote);
    renderIn(app, <ActiveNodeLabel composer />);
    const label = screen.getByLabelText("Message destination");
    expect(label).toHaveTextContent("Remote · Amy on studio-1");
    expect(label).toHaveAttribute("title", expect.stringContaining(remote));
  });

  it("does not show the local node in place of an unavailable explicit destination", () => {
    const app = fixture();
    app.actions.selectNode("missing");
    renderIn(app, <ActiveNodeLabel composer />);
    expect(screen.getByLabelText("Message destination")).toHaveTextContent(
      "Node unavailable",
    );
    expect(screen.getByLabelText("Message destination")).toHaveAttribute(
      "title",
      expect.stringContaining("missing"),
    );
  });

  it("rejects stale send and goal callbacks while preserving the source draft", async () => {
    const app = fixture();
    const send = vi.spyOn(app.actions, "sendMessage").mockResolvedValue(null);
    const goal = vi.spyOn(app.actions, "setGoal").mockResolvedValue(null);
    const { result } = renderHook(() => useComposerSend("default"), {
      wrapper: withApp(app),
    });
    act(() => result.current.setDraft("original seed"));
    const original = result.current;
    const key = app.project().draftKey;
    act(() => app.actions.selectNode(remote));
    await act(async () => {
      expect(await original.send("original seed")).toBe(false);
      expect(await original.setGoal("original seed")).toBe(false);
    });
    expect(send).not.toHaveBeenCalled();
    expect(goal).not.toHaveBeenCalled();
    expect(app.drafts.getState().drafts[key]).toBe("original seed");
    expect(result.current.error).toContain("destination changed");
  });

  it("accepts a callback while its displayed destination is unchanged", async () => {
    const app = fixture();
    const send = vi.spyOn(app.actions, "sendMessage").mockResolvedValue(null);
    const { result } = renderHook(() => useComposerSend("default"), {
      wrapper: withApp(app),
    });
    await act(async () => {
      await result.current.send("seed");
    });
    expect(send).toHaveBeenCalledOnce();
    expect(send.mock.calls[0]?.[0]).toBe("seed");
  });
});

it("sends the first message to the active remote node while the local node is first", async () => {
  const app = fixture();
  const send = vi.fn(acceptedChatReply);
  app.api.sendChatMessage = send;
  app.api.acknowledgeChatSubmission = vi.fn().mockResolvedValue(undefined);
  app.actions.selectNode(remote);
  expect(app.project().shellProjection.nonEmptyContentSendStatus).toMatchObject({
    kind: "ready",
  });
  await app.actions.sendMessage("Run this on Amy");
  expect(app.stores.chat.getState().submissionError).toBeNull();
  expect(send).toHaveBeenCalledOnce();
  expect(send.mock.calls[0]?.[0]).toMatchObject({
    nodeDid: remote,
    content: "Run this on Amy",
  });
});

it("does not submit to the local fallback when the explicit destination disappears", async () => {
  const app = fixture();
  const send = vi.fn(acceptedChatReply);
  app.api.sendChatMessage = send;
  app.actions.selectNode("missing");
  expect(await app.actions.sendMessage("Keep this seed")).toBeNull();
  expect(await app.actions.setGoal("Keep this goal")).toBeNull();
  expect(send).not.toHaveBeenCalled();
});

it("shows enrolled settings read-only before an unavailable operator write", () => {
  const app = fixture();
  app.actions.selectNode(remote);
  renderIn(
    app,
    <TooltipProvider>
      <AgentScreen nodeDid={remote} section="agent" />
    </TooltipProvider>,
  );
  expect(screen.getByRole("status")).toHaveTextContent(
    "Remote configuration is read-only",
  );
  expect(screen.getByRole("textbox", { name: "Display name" })).toBeDisabled();
});
