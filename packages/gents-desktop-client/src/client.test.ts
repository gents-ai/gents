import { describe, expect, it } from "vitest";

import { createDesktopClient } from "./client.js";
import { BridgeInvokeError } from "./errors.js";
import { createMemoryTransport } from "./testing.js";

describe("desktop client", () => {
  it("carries each view's explicit agent through task, schedule and retry actions", async () => {
    const transport = createMemoryTransport({
      handlers: {
        desktop_task_run: () => ({}),
        desktop_schedule_run: () => ({}),
        desktop_request_retry: () => ({}),
        desktop_request_resend: () => ({}),
      },
    });
    const { api } = createDesktopClient(transport);
    for (const agentDid of ["did:alpha", "did:beta"]) {
      await api.runTask({ taskId: "daily", agentDid });
      await api.runSchedule({ scheduleId: "daily", agentDid });
      await api.retryRequest("same-request-id", agentDid);
      await api.resendRequest("same-request-id", agentDid);
    }
    expect(transport.calls).toEqual(
      ["did:alpha", "did:beta"].flatMap((agentDid) => [
        {
          command: "desktop_task_run",
          args: { request: { taskId: "daily", agentDid } },
        },
        {
          command: "desktop_schedule_run",
          args: { request: { scheduleId: "daily", agentDid } },
        },
        {
          command: "desktop_request_retry",
          args: { requestId: "same-request-id", agentDid },
        },
        {
          command: "desktop_request_resend",
          args: { requestId: "same-request-id", agentDid },
        },
      ]),
    );
  });
  it("starts through the real bridge command without a contract handshake", async () => {
    const snapshot = { bootstrap: {}, client: null };
    const transport = createMemoryTransport({
      handlers: {
        desktop_client_start: () => snapshot,
      },
    });
    const client = createDesktopClient(transport);

    await expect(client.api.startDesktopClient()).resolves.toEqual(snapshot);
    await expect(client.clientStart()).resolves.toEqual(snapshot);
    expect(transport.calls.map(({ command }) => command)).toEqual([
      "desktop_client_start",
      "desktop_client_start",
    ]);
  });

  it("normalizes a real startup failure without invoking another command", async () => {
    const transport = createMemoryTransport({
      handlers: {
        desktop_client_start: () => {
          throw {
            code: "clientStartFailed",
            message: "runtime is unavailable",
            retryable: true,
            endpoint: null,
          };
        },
      },
    });

    await expect(
      createDesktopClient(transport).api.startDesktopClient(),
    ).rejects.toMatchObject({
      name: "BridgeInvokeError",
      code: "clientStartFailed",
      message: "runtime is unavailable",
      retryable: true,
    } satisfies Partial<BridgeInvokeError>);
    expect(transport.calls.map(({ command }) => command)).toEqual([
      "desktop_client_start",
    ]);
  });

  it("routes status enrollment through the authenticated bridge command", async () => {
    const enrollment = {
      requestId: "request-1",
      networkId: "network-1",
      adminDid: "did:key:zAdmin",
      serverPeer: "server-peer-1",
      ownerAgent: "did:key:zAgent",
      state: "pending_approval",
    };
    const transport = createMemoryTransport({
      handlers: {
        desktop_peer_enroll_status: (args) => {
          expect(args).toEqual({
            request: { serverAddress: "http://amy.local:9191" },
          });
          return enrollment;
        },
      },
    });

    await expect(
      createDesktopClient(transport).api.requestStatusEnrollment(
        "http://amy.local:9191",
      ),
    ).resolves.toEqual(enrollment);
    expect(transport.calls.map(({ command }) => command)).toEqual([
      "desktop_peer_enroll_status",
    ]);
  });

  it("invokes managed runtime restart with the exact reviewed authority", async () => {
    const restarted = {
      state: "running",
      autoStart: true,
      agentName: "Workshop Agent",
      agentDid: "did:key:agent",
      graphql: "http://127.0.0.1:9191/graphql",
      effectiveToolCeiling: "meta-only",
      effectiveToolRoot: null,
      suggestedToolRoot: "/Users/test",
      pairingReady: false,
      error: null,
    };
    const transport = createMemoryTransport({
      handlers: {
        desktop_managed_server_restart: (args) => {
          expect(args).toEqual({
            request: {
              agentName: "Workshop Agent",
              toolCeiling: "meta-only",
              toolRoot: null,
            },
          });
          return restarted;
        },
      },
    });

    await expect(
      createDesktopClient(transport).api.restartManagedServer(
        "Workshop Agent",
        {
          toolCeiling: "meta-only",
          toolRoot: null,
        },
      ),
    ).resolves.toEqual(restarted);
    expect(transport.calls.map(({ command }) => command)).toEqual([
      "desktop_managed_server_restart",
    ]);
  });

  it("changes native login enablement without starting or stopping the runtime", async () => {
    const status = {
      state: "running",
      autoStart: false,
      agentName: "Workshop Agent",
      agentDid: "did:key:agent",
      graphql: "http://127.0.0.1:9191/graphql",
      effectiveToolCeiling: "meta-only",
      effectiveToolRoot: null,
      suggestedToolRoot: "/Users/test",
      pairingReady: true,
      error: null,
    };
    const transport = createMemoryTransport({
      handlers: {
        desktop_managed_server_set_auto_start: (args) => {
          expect(args).toEqual({ enabled: false });
          return status;
        },
      },
    });

    await expect(
      createDesktopClient(transport).api.setManagedServerAutoStart?.(false),
    ).resolves.toEqual(status);
    expect(transport.calls.map(({ command }) => command)).toEqual([
      "desktop_managed_server_set_auto_start",
    ]);
  });
});
