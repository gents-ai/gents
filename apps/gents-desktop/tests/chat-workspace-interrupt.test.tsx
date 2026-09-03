import { describe, expect, it, vi, beforeEach } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";

import { ActiveChatWorkspace } from "../src/components/ChatWorkspace";
import { deployment as fixtureDeployment } from "./config-panel-wiring/fixtures";
import type {
  DeploymentView,
  DesktopApiAdapter,
  DesktopSessionSnapshot,
} from "@source-inc/gents-desktop-client";

const mockedPreview = vi.fn();
const mockedInterrupt = vi.fn();
const api = {
  previewInterruptCascade: mockedPreview,
  interruptRequest: mockedInterrupt,
  listToolCallHolds: vi.fn().mockResolvedValue([]),
} as unknown as DesktopApiAdapter;

const baseDeployment: DeploymentView = {
  ...fixtureDeployment,
  agentDid: "did:test:operator",
  agentPrincipal: {
    ...fixtureDeployment.agentPrincipal,
    agentDid: "did:test:operator",
  },
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
  api,
  activeRequestId: "req_root",
  activityStatus: {
    kind: "working" as const,
    label: "Agent is working…",
    detail: "This turn must finish before another message can be sent.",
    animated: true,
  },
  selectedDeployment: baseDeployment,
  selectedConversationTitle: "t",
  selectedBehaviorId: "default",
  selectedSessionId: "s1",
  session: streamingSession,
  runtimeHealth: null,
  rowCount: 0,
  approxSerializedBytes: 0,
  canSend: false,
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
    expect(mockedInterrupt).not.toHaveBeenCalled();
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
