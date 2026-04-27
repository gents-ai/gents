import { waitFor } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import {
  createTestDeployment,
  createTestDesktopSnapshot,
  FakeTauriBridge,
  renderTauriAppDriver,
  renderTauriAppDriverWithBridge,
} from "./tauri-driver";
import type { DeploymentView, DesktopSessionSnapshot } from "../src/lib/types";

function sessionSnapshot(
  overrides: Partial<DesktopSessionSnapshot> = {},
): DesktopSessionSnapshot {
  return {
    sessionId: "session-1",
    agentDid: "did:defra-agent:alpha",
    behaviorId: "alpha-default",
    title: "conversation",
    previewText: "preview",
    status: "active",
    turnState: "completed",
    latestRequestId: "request-1",
    latestResponse: {
      status: "complete",
      content: "done",
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
        itemKey: "user-1",
        sequence: 1,
        content: "hello",
      },
      {
        kind: "assistantMessage",
        itemKey: "assistant-1",
        sequence: 2,
        content: "done",
        reasoning: null,
      },
    ],
    ...overrides,
  };
}

function deploymentWithConversation(
  overrides: Partial<DeploymentView> = {},
): DeploymentView {
  return createTestDeployment({
    conversations: [
      {
        sessionId: "session-1",
        title: "conversation",
        previewText: "preview",
        status: "active",
        behaviorId: "alpha-default",
        latestRequestId: "request-1",
        createdAt: "2026-04-22T00:00:00Z",
        updatedAt: "2026-04-22T00:00:01Z",
        turnState: "completed",
        messageCount: 2,
        toolCallCount: 0,
      },
    ],
    ...overrides,
  });
}

describe("Tauri app driver", () => {
  it("submits the real composer form on Enter", async () => {
    const deployment = createTestDeployment();
    const driver = renderTauriAppDriver({
      snapshot: createTestDesktopSnapshot([deployment]),
    });

    try {
      await driver.ready();
      await driver.typeComposer(
        "Hey amy can you tell me about the p2p communcation between the agent and the desktop in this app and the docuemnt based request model?",
      );
      await driver.pressEnter();

      await waitFor(() => {
        expect(driver.bridge.sentRequests).toHaveLength(1);
      });
      expect(driver.bridge.sentRequests[0]).toEqual({
        agentDid: deployment.agentDid,
        behaviorId: deployment.defaultBehaviorId,
        sessionId: null,
        content:
          "Hey amy can you tell me about the p2p communcation between the agent and the desktop in this app and the docuemnt based request model?",
      });
      await waitFor(() => {
        expect(driver.composer()).toHaveValue("");
      });
      await waitFor(() => {
        expect(driver.conversation("session-1")).toBeInTheDocument();
      });
    } finally {
      driver.dispose();
    }
  });

  it("preserves Shift+Enter for multiline drafting instead of sending", async () => {
    const driver = renderTauriAppDriver({
      snapshot: createTestDesktopSnapshot([createTestDeployment()]),
    });

    try {
      await driver.ready();
      await driver.typeComposer("first line");
      await driver.pressShiftEnter();

      expect(driver.bridge.sentRequests).toHaveLength(0);
      expect(driver.composer()).toHaveValue("first line\n");
    } finally {
      driver.dispose();
    }
  });

  it("auto-restarts the desktop client when the transport wedges", async () => {
    const deployment = createTestDeployment();
    const healthySnapshot = createTestDesktopSnapshot([deployment]);
    const recoveredSnapshot = createTestDesktopSnapshot([deployment]);
    const bridge = new FakeTauriBridge({
      snapshot: healthySnapshot,
      onStart: async (nextBridge) => {
        nextBridge.setSnapshot(recoveredSnapshot);
        return recoveredSnapshot;
      },
    });
    const driver = renderTauriAppDriverWithBridge(bridge, deployment.peerId);

    try {
      await driver.ready();

      bridge.setSnapshot({
        ...healthySnapshot,
        client: {
          ...healthySnapshot.client!,
          p2pHealth: {
            ...healthySnapshot.client!.p2pHealth,
            status: "wedged",
            consecutiveFailures: 3,
            lastError: "timed out reading desktop P2P listen addresses",
          },
        },
      });
      await bridge.emitClientUpdated();

      await waitFor(() => {
        expect(bridge.shutdownCalls).toBe(1);
        expect(bridge.startCalls).toBe(1);
      });
      expect(bridge.snapshot.client?.p2pHealth.status).toBe("healthy");
    } finally {
      driver.dispose();
    }
  });

  it("does not auto-restart when a streaming follow-up stalls", async () => {
    const deployment = deploymentWithConversation();
    const initialSnapshot = createTestDesktopSnapshot([deployment]);
    const bridge = new FakeTauriBridge({
      snapshot: initialSnapshot,
      sessions: {
        "session-1": sessionSnapshot({
          agentDid: deployment.agentDid,
          behaviorId: deployment.defaultBehaviorId,
        }),
      },
      onSend: async (request, nextBridge) => {
        nextBridge.setSnapshot(
          createTestDesktopSnapshot([
            deploymentWithConversation({
              conversations: [
                {
                  sessionId: "session-1",
                  title: "conversation",
                  previewText: request.content,
                  status: "active",
                  behaviorId: deployment.defaultBehaviorId,
                  latestRequestId: "request-2",
                  createdAt: "2026-04-22T00:00:00Z",
                  updatedAt: "2026-04-22T00:00:02Z",
                  turnState: "streaming",
                  messageCount: 3,
                  toolCallCount: 0,
                },
              ],
            }),
          ]),
        );
        nextBridge.setSession(
          "session-1",
          sessionSnapshot({
            agentDid: deployment.agentDid,
            behaviorId: deployment.defaultBehaviorId,
            latestRequestId: "request-2",
            turnState: "streaming",
            latestResponse: null,
            activeResponseOverlay: {
              status: "streaming",
              content: "partial answer",
              reasoning: null,
              errorMessage: null,
              tokenCount: 10,
              materializedMessageSequence: null,
              materializedAt: null,
              completedAt: null,
            },
            timelineItems: [
              {
                kind: "userMessage",
                itemKey: "user-1",
                sequence: 1,
                content: "hello",
              },
              {
                kind: "assistantMessage",
                itemKey: "assistant-1",
                sequence: 2,
                content: "done",
                reasoning: null,
              },
              {
                kind: "liveAssistant",
                itemKey: "assistant-live-2",
                content: "partial answer",
                reasoning: null,
              },
            ],
          }),
        );
        await nextBridge.emitClientUpdated();
        return {
          sessionId: "session-1",
          requestId: "request-2",
          agentDid: request.agentDid,
          behaviorId: request.behaviorId ?? deployment.defaultBehaviorId ?? null,
        };
      },
    });
    const driver = renderTauriAppDriverWithBridge(bridge, deployment.peerId);

    try {
      await driver.ready();
      await driver.typeComposer("follow up");
      await driver.pressEnter();

      await waitFor(() => {
        expect(bridge.sentRequests).toHaveLength(1);
      });
      expect(bridge.sentRequests[0]?.sessionId).toBe("session-1");

      await new Promise((resolve) => setTimeout(resolve, 100));

      expect(bridge.shutdownCalls).toBe(0);
      expect(bridge.startCalls).toBe(0);
      expect(
        bridge.snapshot.client?.deployments[0]?.conversations[0]?.sessionId,
      ).toBe("session-1");
      expect(
        bridge.snapshot.client?.deployments[0]?.conversations[0]?.latestRequestId,
      ).toBe("request-2");
      expect(
        bridge.snapshot.client?.deployments[0]?.conversations[0]?.turnState,
      ).toBe("streaming");
    } finally {
      driver.dispose();
    }
  });

  it("does not restart while a streaming follow-up keeps making progress", async () => {
    const deployment = deploymentWithConversation();
    const initialSnapshot = createTestDesktopSnapshot([deployment]);
    const bridge = new FakeTauriBridge({
      snapshot: initialSnapshot,
      sessions: {
        "session-1": sessionSnapshot({
          agentDid: deployment.agentDid,
          behaviorId: deployment.defaultBehaviorId,
        }),
      },
      onSend: async (request, nextBridge) => {
        const applyStreamingProgress = (step: number) => {
          const partialAnswer = `partial answer ${step}`;
          nextBridge.setSnapshot(
            createTestDesktopSnapshot([
              deploymentWithConversation({
                conversations: [
                  {
                    sessionId: "session-1",
                    title: "conversation",
                    previewText: request.content,
                    status: "active",
                    behaviorId: deployment.defaultBehaviorId,
                    latestRequestId: "request-2",
                    createdAt: "2026-04-22T00:00:00Z",
                    updatedAt: `2026-04-22T00:00:0${step + 1}Z`,
                    turnState: "streaming",
                    messageCount: 3 + step,
                    toolCallCount: step,
                  },
                ],
              }),
            ]),
          );
          nextBridge.setSession(
            "session-1",
            sessionSnapshot({
              agentDid: deployment.agentDid,
              behaviorId: deployment.defaultBehaviorId,
              latestRequestId: "request-2",
              previewText: request.content,
              turnState: "streaming",
              latestResponse: null,
              activeResponseOverlay: {
                status: "streaming",
                content: partialAnswer,
                reasoning: null,
                errorMessage: null,
                tokenCount: 10 + step,
                materializedMessageSequence: null,
                materializedAt: null,
                completedAt: null,
              },
              timelineItems: [
                {
                  kind: "userMessage",
                  itemKey: "user-1",
                  sequence: 1,
                  content: "hello",
                },
                {
                  kind: "assistantMessage",
                  itemKey: "assistant-1",
                  sequence: 2,
                  content: "done",
                  reasoning: null,
                },
                {
                  kind: "userMessage",
                  itemKey: "user-2",
                  sequence: 3,
                  content: request.content,
                },
                {
                  kind: "liveAssistant",
                  itemKey: "assistant-live-2",
                  content: partialAnswer,
                  reasoning: null,
                },
              ],
            }),
          );
          void nextBridge.emitClientUpdated();
        };

        const applyCompleted = () => {
          nextBridge.setSnapshot(
            createTestDesktopSnapshot([
              deploymentWithConversation({
                conversations: [
                  {
                    sessionId: "session-1",
                    title: "conversation",
                    previewText: request.content,
                    status: "active",
                    behaviorId: deployment.defaultBehaviorId,
                    latestRequestId: "request-2",
                    createdAt: "2026-04-22T00:00:00Z",
                    updatedAt: "2026-04-22T00:00:06Z",
                    turnState: "completed",
                    messageCount: 4,
                    toolCallCount: 3,
                  },
                ],
              }),
            ]),
          );
          nextBridge.setSession(
            "session-1",
            sessionSnapshot({
              agentDid: deployment.agentDid,
              behaviorId: deployment.defaultBehaviorId,
              latestRequestId: "request-2",
              previewText: request.content,
              turnState: "completed",
              latestResponse: {
                status: "complete",
                content: "final answer",
                reasoning: null,
                errorMessage: null,
                tokenCount: 42,
                materializedMessageSequence: 4,
                materializedAt: "2026-04-22T00:00:06Z",
                completedAt: "2026-04-22T00:00:06Z",
              },
              activeResponseOverlay: null,
              timelineItems: [
                {
                  kind: "userMessage",
                  itemKey: "user-1",
                  sequence: 1,
                  content: "hello",
                },
                {
                  kind: "assistantMessage",
                  itemKey: "assistant-1",
                  sequence: 2,
                  content: "done",
                  reasoning: null,
                },
                {
                  kind: "userMessage",
                  itemKey: "user-2",
                  sequence: 3,
                  content: request.content,
                },
                {
                  kind: "assistantMessage",
                  itemKey: "assistant-2",
                  sequence: 4,
                  content: "final answer",
                  reasoning: null,
                },
              ],
            }),
          );
          void nextBridge.emitClientUpdated();
        };

        applyStreamingProgress(1);
        setTimeout(() => applyStreamingProgress(2), 20);
        setTimeout(() => applyStreamingProgress(3), 40);
        setTimeout(applyCompleted, 60);

        return {
          sessionId: "session-1",
          requestId: "request-2",
          agentDid: request.agentDid,
          behaviorId: request.behaviorId ?? deployment.defaultBehaviorId ?? null,
        };
      },
    });
    const driver = renderTauriAppDriverWithBridge(bridge, deployment.peerId);

    try {
      await driver.ready();
      await driver.typeComposer("follow up with progress");
      await driver.pressEnter();

      await waitFor(() => {
        expect(bridge.sentRequests).toHaveLength(1);
      });
      expect(bridge.sentRequests[0]?.sessionId).toBe("session-1");

      await waitFor(() => {
        expect(
          bridge.snapshot.client?.deployments[0]?.conversations[0]?.turnState,
        ).toBe("completed");
      });

      await new Promise((resolve) => setTimeout(resolve, 80));

      expect(bridge.shutdownCalls).toBe(0);
      expect(bridge.startCalls).toBe(0);
      expect(
        bridge.snapshot.client?.deployments[0]?.conversations[0]?.latestRequestId,
      ).toBe("request-2");
    } finally {
      driver.dispose();
    }
  });
});
