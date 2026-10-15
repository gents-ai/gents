import { describe, expect, it } from "vitest";

import type { ChatWorkflowState } from "@source-inc/gents-desktop-chat";
import type { DesktopApiAdapter } from "@source-inc/gents-desktop-client";

import { createDesktopApp } from "../src/hooks/desktopApp";
import { node, publish } from "./app-fixture";

const submitting: ChatWorkflowState = {
  kind: "submittingRequest",
  nodeDid: "did:key:node",
  sessionId: null,
};

describe("the app's own reactions", () => {
  it("selects the first published node and its agent through production wiring", () => {
    const app = createDesktopApp({ api: {} as DesktopApiAdapter });
    const nodeDid = "did:key:first-node";
    const agentId = "default";
    publish(app, [
      node({
        nodeDid,
        node: { nodeDid, defaultAgentId: agentId },
        agents: [{ agentId, enabled: true, isDefault: true }],
      }),
    ]);
    expect(app.stores.selection.getState()).toMatchObject({
      nodeDid,
      agentId,
      sessionId: null,
    });
  });

  it("releases a submission whose send has ended", () => {
    const { stores } = createDesktopApp({ api: {} as DesktopApiAdapter });
    stores.chat.setState({ localWorkflow: submitting, sending: true });
    expect(stores.chat.getState().localWorkflow).toBe(submitting);
    stores.chat.setState({ sending: false });
    expect(stores.chat.getState().localWorkflow).toEqual({ kind: "ready" });
  });

  it("leaves a send that has just started alone", () => {
    const { stores } = createDesktopApp({ api: {} as DesktopApiAdapter });
    stores.chat.setState({ localWorkflow: submitting, sending: true });
    stores.chat.setState({ sending: true });
    expect(stores.chat.getState().localWorkflow).toBe(submitting);
  });
});
