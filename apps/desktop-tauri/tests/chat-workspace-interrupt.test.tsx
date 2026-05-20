import { describe, expect, it, vi, beforeEach } from "vitest";
import { render, screen, fireEvent, waitFor } from "@testing-library/react";

vi.mock("../src/lib/tauri/interruptRequest", () => ({
  previewInterruptCascade: vi.fn(),
  interruptRequest: vi.fn(),
}));

import { previewInterruptCascade, interruptRequest } from "../src/lib/tauri/interruptRequest";
import { ActiveChatWorkspace } from "../src/components/ChatWorkspace";
import type { DeploymentView, DesktopSessionSnapshot } from "../src/lib/types";

const mockedPreview = vi.mocked(previewInterruptCascade);
const mockedInterrupt = vi.mocked(interruptRequest);

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
  sending: false,
  onRenameConversationTitle: vi.fn(),
  onDraftChange: vi.fn(),
  onSend: vi.fn(),
};

beforeEach(() => {
  mockedPreview.mockReset();
  mockedInterrupt.mockReset();
});

describe("ActiveChatWorkspace interrupt flow", () => {
  it("standalone request: clicking Interrupt latches directly and shows a banner", async () => {
    mockedPreview.mockResolvedValue({
      rootRequestId: "req_root",
      previewSignature: "sig",
      rootState: "processing",
      willInterrupt: [], willDetach: [], alreadyTerminal: [], unknownPolicy: [],
    });
    mockedInterrupt.mockResolvedValue({
      requestId: "req_root", accepted: true, alreadyInterrupted: false,
      stalePreview: false, interruptRequestedAt: "2026-05-20T10:32:14Z",
    });
    render(<ActiveChatWorkspace {...baseProps} />);
    const btn = await screen.findByRole("button", { name: /interrupt/i });
    fireEvent.click(btn);
    await waitFor(() => {
      expect(mockedInterrupt).toHaveBeenCalledWith({
        requestId: "req_root", cause: "userCancelled", cascade: false,
      });
    });
    expect(await screen.findByText(/interrupt accepted/i)).toBeInTheDocument();
  });

  it("parent with children: clicking Interrupt opens cascade dialog", async () => {
    mockedPreview.mockResolvedValue({
      rootRequestId: "req_root",
      previewSignature: "sig",
      rootState: "processing",
      willInterrupt: [
        { requestId: "req_b91", lifecycleState: "processing", parentRequestId: "req_root", parentToolCallId: "tc_1", awaitMode: "background", cancelPolicy: "cascade", toolName: "summarize" },
      ],
      willDetach: [], alreadyTerminal: [], unknownPolicy: [],
    });
    render(<ActiveChatWorkspace {...baseProps} />);
    const btn = await screen.findByRole("button", { name: /interrupt/i });
    fireEvent.click(btn);
    expect(await screen.findByRole("dialog")).toBeInTheDocument();
    expect(mockedInterrupt).not.toHaveBeenCalled(); // not until user confirms
  });
});
