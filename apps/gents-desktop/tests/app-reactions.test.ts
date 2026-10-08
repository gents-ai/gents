import { describe, expect, it } from "vitest";

import type { ChatWorkflowState } from "@source-inc/gents-desktop-chat";
import type { DesktopApiAdapter } from "@source-inc/gents-desktop-client";

import { createDesktopApp } from "../src/hooks/desktopApp";

const submitting: ChatWorkflowState = {
  kind: "submittingRequest",
  agentDid: "did:key:agent",
  sessionId: null,
};

describe("the app's own reactions", () => {
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
