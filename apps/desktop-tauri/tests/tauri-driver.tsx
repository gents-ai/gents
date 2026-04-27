import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { expect } from "vitest";

import App from "../src/App";
import {
  setDesktopApiAdapterForTests,
  type DesktopApiAdapter,
} from "../src/lib/desktop-api";
import { setDesktopShellTimingConfigForTests } from "../src/hooks/useDesktopShell";
import {
  setDesktopClientUpdatedListenerFactoryForTests,
  type DesktopClientUpdatedHandler,
  type DesktopClientUpdatedListenerFactory,
} from "../src/lib/desktop-events";
import type {
  ChatSendResult,
  DeploymentView,
  DesktopClientSnapshot,
  DesktopSessionSnapshot,
} from "../src/lib/types";

export type TauriDriverChatRequest = {
  agentDid: string;
  behaviorId?: string | null;
  sessionId?: string | null;
  content: string;
};

type TauriDriverOptions = {
  snapshot: DesktopClientSnapshot;
  sessions?: Record<string, DesktopSessionSnapshot | null>;
  timingConfig?: {
    p2pAutoRestartCooldownMs?: number;
    clientRestartMaxAttempts?: number;
    clientRestartBackoffMs?: number;
  };
  onStart?: (
    bridge: FakeTauriBridge,
  ) => Promise<DesktopClientSnapshot> | DesktopClientSnapshot;
  onShutdown?: (
    bridge: FakeTauriBridge,
  ) => Promise<DesktopClientSnapshot> | DesktopClientSnapshot;
  onSend?: (
    request: TauriDriverChatRequest,
    bridge: FakeTauriBridge,
  ) => Promise<ChatSendResult> | ChatSendResult;
};

export type TauriDriverBridge = {
  adapter: DesktopApiAdapter;
  listenerFactory: DesktopClientUpdatedListenerFactory;
  sentRequests: TauriDriverChatRequest[];
  dispose?: () => Promise<void> | void;
};

export function createTestDeployment(
  overrides: Partial<DeploymentView> = {},
): DeploymentView {
  return {
    peerId: "peer-alpha",
    label: "Alpha Server",
    agentDid: "did:defra-agent:alpha",
    addr: "127.0.0.1:1",
    source: "test",
    graphql: "http://127.0.0.1:8080/api/v0/graphql",
    dialSucceeded: true,
    lastError: null,
    defaultBehaviorId: "alpha-default",
    runtime: {
      processState: "ready",
      reconcilePhase: "idle",
      lastReconcileResult: "ok",
      lastReconcileError: null,
      updatedAt: "2026-04-22T00:00:00Z",
    },
    behaviors: [
      {
        behaviorId: "alpha-default",
        displayName: "Default",
        modelName: "test-model",
        enabled: true,
        isDefault: true,
      },
    ],
    conversations: [],
    ...overrides,
  };
}

export function createTestDesktopSnapshot(
  deployments: DeploymentView[],
): DesktopClientSnapshot {
  return {
    bootstrap: {
      defaultAgentHome: "/tmp/agent-home",
      desktopHome: "/tmp/desktop-home",
      peerDirectoryPath: "/tmp/desktop-home/peers.json",
      nodeDataDir: "/tmp/desktop-home/node",
      agentHomeExists: true,
      desktopHomeExists: true,
      peerDirectoryExists: true,
      savedPeers: [],
    },
    client: {
      localPeerId: "desktop-peer",
      listenAddresses: [],
      p2pHealth: {
        status: "healthy",
        connectedPeerCount: deployments.length,
        replicatorCount: deployments.length,
        consecutiveFailures: 0,
        lastError: null,
      },
      bootstrapErrors: [],
      lastMutationError: null,
      focusedRequestId: null,
      configuredPeerCount: deployments.length,
      dialedPeerCount: deployments.length,
      peerIssueCount: 0,
      rowCount: 1,
      approxSerializedBytes: 1024,
      deployments,
    },
  };
}

export class FakeTauriBridge {
  snapshot: DesktopClientSnapshot;
  sessions: Record<string, DesktopSessionSnapshot | null>;
  sentRequests: TauriDriverChatRequest[] = [];
  renamedConversations: Array<{ sessionId: string; title: string }> = [];
  startCalls = 0;
  shutdownCalls = 0;

  private readonly listeners = new Set<DesktopClientUpdatedHandler>();
  private requestCounter = 0;
  private sessionCounter = 0;
  private readonly onStart?: TauriDriverOptions["onStart"];
  private readonly onShutdown?: TauriDriverOptions["onShutdown"];
  private readonly onSend?: TauriDriverOptions["onSend"];

  readonly adapter: DesktopApiAdapter;

  constructor(options: TauriDriverOptions) {
    this.snapshot = options.snapshot;
    this.sessions = { ...(options.sessions ?? {}) };
    this.onStart = options.onStart;
    this.onShutdown = options.onShutdown;
    this.onSend = options.onSend;
    this.adapter = {
      fetchDesktopSnapshot: async () => this.snapshot,
      initLocalStandardRuntime: async () => ({
        status: "initialized",
        source: "test",
        agentHome: "/tmp/agent-home",
        desktopHome: "/tmp/desktop-home",
        peerDirectory: "/tmp/desktop-home/peers.json",
        label: "Local Agent",
        agentName: "Local Agent",
        agentDid: "did:defra-agent:local",
        graphql: "http://127.0.0.1:8080/api/v0/graphql",
        p2pTransport: "iroh",
        p2pPeerId: "desktop-peer",
        p2pListenAddress: "127.0.0.1:1",
        peerRecordId: "peer-local",
        nextSteps: [],
      }),
      startDesktopClient: async () => this.startDesktopClient(),
      shutdownDesktopClient: async () => this.shutdownDesktopClient(),
      fetchSessionSnapshot: async (sessionId) => this.sessions[sessionId] ?? null,
      sendChatMessage: async (request) => this.sendChatMessage(request),
      renameConversation: async (request) => {
        this.renamedConversations.push(request);
      },
    };
  }

  async emitClientUpdated() {
    for (const listener of [...this.listeners]) {
      await listener();
    }
  }

  listenerFactory = async (handler: DesktopClientUpdatedHandler) => {
    this.listeners.add(handler);
    return () => {
      this.listeners.delete(handler);
    };
  };

  setSnapshot(snapshot: DesktopClientSnapshot) {
    this.snapshot = snapshot;
  }

  setSession(sessionId: string, snapshot: DesktopSessionSnapshot | null) {
    this.sessions[sessionId] = snapshot;
  }

  private async startDesktopClient() {
    this.startCalls += 1;
    if (this.onStart) {
      const next = await this.onStart(this);
      this.snapshot = next;
      return next;
    }
    return this.snapshot;
  }

  private async shutdownDesktopClient() {
    this.shutdownCalls += 1;
    if (this.onShutdown) {
      const next = await this.onShutdown(this);
      this.snapshot = next;
      return next;
    }
    const next = {
      ...this.snapshot,
      client: null,
    };
    this.snapshot = next;
    return next;
  }

  private async sendChatMessage(
    request: TauriDriverChatRequest,
  ): Promise<ChatSendResult> {
    this.sentRequests.push(request);
    if (this.onSend) {
      return this.onSend(request, this);
    }

    const sessionId =
      request.sessionId ?? `session-${++this.sessionCounter}`;
    const requestId = `request-${++this.requestCounter}`;
    const behaviorId =
      request.behaviorId ??
      this.snapshot.client?.deployments.find(
        (deployment) => deployment.agentDid === request.agentDid,
      )?.defaultBehaviorId ??
      null;

    this.upsertConversation({
      sessionId,
      agentDid: request.agentDid,
      behaviorId,
      latestRequestId: requestId,
      previewText: request.content,
    });
    this.setSession(sessionId, {
      sessionId,
      agentDid: request.agentDid,
      behaviorId,
      title: null,
      previewText: request.content,
      status: "active",
      turnState: "completed",
      latestRequestId: requestId,
      latestResponse: {
        status: "completed",
        content: "ack",
        reasoning: null,
        errorMessage: null,
        tokenCount: null,
        materializedMessageSequence: 2,
        materializedAt: "2026-04-22T00:00:01Z",
        completedAt: "2026-04-22T00:00:01Z",
      },
      activeResponseOverlay: null,
      pendingTurn: null,
      timelineItems: [
        {
          kind: "userMessage",
          itemKey: `user-${requestId}`,
          sequence: 1,
          content: request.content,
        },
        {
          kind: "assistantMessage",
          itemKey: `assistant-${requestId}`,
          sequence: 2,
          content: "ack",
          reasoning: null,
        },
      ],
    });

    return {
      sessionId,
      requestId,
      agentDid: request.agentDid,
      behaviorId,
    };
  }

  private upsertConversation(input: {
    sessionId: string;
    agentDid: string;
    behaviorId?: string | null;
    latestRequestId: string;
    previewText: string;
  }) {
    const client = this.snapshot.client;
    if (!client) {
      return;
    }

    const deployments = client.deployments.map((deployment) => {
      if (deployment.agentDid !== input.agentDid) {
        return deployment;
      }

      const existingIndex = deployment.conversations.findIndex(
        (conversation) => conversation.sessionId === input.sessionId,
      );
      const nextConversation = {
        sessionId: input.sessionId,
        title: null,
        previewText: input.previewText,
        status: "active",
        behaviorId: input.behaviorId ?? null,
        latestRequestId: input.latestRequestId,
        createdAt: "2026-04-22T00:00:00Z",
        updatedAt: "2026-04-22T00:00:01Z",
        turnState: "completed",
        messageCount: 2,
        toolCallCount: 0,
      };

      const conversations =
        existingIndex === -1
          ? [nextConversation, ...deployment.conversations]
          : deployment.conversations.map((conversation, index) =>
              index === existingIndex ? nextConversation : conversation,
            );

      return {
        ...deployment,
        conversations,
      };
    });

    this.snapshot = {
      ...this.snapshot,
      client: {
        ...client,
        deployments,
      },
    };
  }
}

export function renderTauriAppDriver(options: TauriDriverOptions) {
  const bridge = new FakeTauriBridge(options);
  return renderTauriAppDriverWithBridge(
    bridge,
    options.snapshot.client?.deployments[0]?.peerId ?? null,
    options.timingConfig,
  );
}

export function renderTauriAppDriverWithBridge(
  bridge: TauriDriverBridge,
  firstPeerId: string | null = null,
  timingConfig: TauriDriverOptions["timingConfig"] = null,
) {
  setDesktopApiAdapterForTests(bridge.adapter);
  setDesktopClientUpdatedListenerFactoryForTests(bridge.listenerFactory);
  setDesktopShellTimingConfigForTests(timingConfig);

  const user = userEvent.setup();
  const rendered = render(<App />);

  return {
    bridge,
    user,
    composer() {
      return screen.getByTestId("composer-input") as HTMLTextAreaElement;
    },
    sendButton() {
      return screen.getByTestId("composer-send");
    },
    conversation(sessionId: string) {
      return screen.getByTestId(`conversation-${sessionId}`);
    },
    async ready() {
      await waitFor(() => {
        expect(screen.getByTestId("composer-input")).toBeInTheDocument();
        if (firstPeerId) {
          expect(screen.getByTestId(`deployment-${firstPeerId}`)).toHaveClass(
            "selected",
          );
        }
      });
    },
    async typeComposer(value: string) {
      await user.type(this.composer(), value);
    },
    async clickSend() {
      await user.click(this.sendButton());
    },
    async pressEnter() {
      await user.type(this.composer(), "{enter}");
    },
    async pressShiftEnter() {
      await user.type(this.composer(), "{shift>}{enter}{/shift}");
    },
    dispose() {
      rendered.unmount();
      setDesktopApiAdapterForTests(null);
      setDesktopClientUpdatedListenerFactoryForTests(null);
      setDesktopShellTimingConfigForTests(null);
      return bridge.dispose?.();
    },
  };
}
