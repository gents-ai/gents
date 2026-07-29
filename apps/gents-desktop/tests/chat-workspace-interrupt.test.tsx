import { describe, expect, it, vi, beforeEach } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";

vi.mock("@source-inc/gents-desktop-chat", async (importOriginal) => ({
  ...(await importOriginal()),
  previewChatInterruptCascade: vi.fn(),
  interruptChatRequest: vi.fn(),
}));

import {
  previewChatInterruptCascade,
  interruptChatRequest,
} from "@source-inc/gents-desktop-chat";
import { ActiveChatWorkspace } from "../src/components/ChatWorkspace";
import type {
  DeploymentView,
  DesktopSessionSnapshot,
} from "@source-inc/gents-desktop-client";

const mockedPreview = vi.mocked(previewChatInterruptCascade);
const mockedInterrupt = vi.mocked(interruptChatRequest);

const baseDeployment: DeploymentView = {
  deploymentId: "dep-1",
  agentDid: "did:test:operator",
  displayName: "test",
  defaultBehaviorId: "default",
  behaviors: [{ behaviorId: "default", displayName: "default" }],
  conversations: [],
  process: null,
  runtime: null,
  inbox: { hasUnread: false, count: 0 },
};

const streamingSession: DesktopSessionSnapshot = {
  sessionId: "s1",
  agentDid: "did:test:operator",
  behaviorId: "default",
  title: "t",
  previewText: null,
  status: "active",
  turnState: "streaming",
  latestRequestId: "req_root",
  latestResponse: null,
  activeResponseOverlay: null,
  pendingTurn: null,
  timelineItems: [],
};

const baseProps = {
  activeRequestId: "req_root",
  selectedDeployment: baseDeployment,
  selectedConversationTitle: "t",
  selectedBehaviorId: "default",
  selectedSessionId: "s1",
  session: streamingSession,
  runtimeHealth: null,
  rowCount: 0,
  approxSerializedBytes: 0,
  dialedPeerCount: 1,
  configuredPeerCount: 1,
  canSend: false,
  sendHint: null,
  draft: "",
  interruptVisible: true,
  sending: false,
  turnState: "streaming",
  onRenameConversationTitle: vi.fn(),
  onDraftChange: vi.fn(),
  onSend: vi.fn(),
  onInterruptAccepted: vi.fn(),
};

beforeEach(() => {
  mockedPreview.mockReset();
  mockedInterrupt.mockReset();
});

describe("ActiveChatWorkspace interrupt flow", () => {
  it("parent with children: clicking Interrupt opens cascade dialog", async () => {
    mockedPreview.mockResolvedValue({
      rootRequestId: "req_root",
      previewSignature: "sig",
      rootState: "processing",
      willInterrupt: [
        {
          requestId: "req_b91",
          lifecycleState: "processing",
          parentRequestId: "req_root",
          parentToolCallId: "tc_1",
          awaitMode: "background",
          cancelPolicy: "cascade",
          toolName: "summarize",
        },
      ],
      willDetach: [],
      alreadyTerminal: [],
      unknownPolicy: [],
    });
    render(<ActiveChatWorkspace {...baseProps} />);
    const btn = await screen.findByRole("button", { name: /interrupt/i });
    fireEvent.click(btn);
    expect(await screen.findByRole("dialog")).toBeInTheDocument();
    expect(mockedInterrupt).not.toHaveBeenCalled(); // not until user confirms
  });

  it("routes a parent-only interrupt through the selected deployment", async () => {
    mockedPreview.mockResolvedValue({
      rootRequestId: "req_root",
      previewSignature: "sig",
      rootState: "processing",
      willInterrupt: [],
      willDetach: [],
      alreadyTerminal: [],
      unknownPolicy: [],
    });
    mockedInterrupt.mockResolvedValue({
      requestId: "req_root",
      accepted: true,
      alreadyInterrupted: false,
      stalePreview: false,
      interruptRequestedAt: "2026-07-24T17:00:00Z",
    });

    render(<ActiveChatWorkspace {...baseProps} />);
    fireEvent.click(await screen.findByRole("button", { name: /interrupt/i }));

    await waitFor(() =>
      expect(mockedInterrupt).toHaveBeenCalledWith({
        requestId: "req_root",
        agentDid: "did:test:operator",
        cause: "userCancelled",
        cascade: false,
      }),
    );
    expect(baseProps.onInterruptAccepted).toHaveBeenCalledOnce();
    expect(await screen.findByTestId("chat-toast")).toHaveTextContent(
      "Interrupt requested",
    );
  });
});
