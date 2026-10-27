import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type {
  DesktopApiAdapter,
  DesktopClientSnapshot,
  GoalControlRequest,
  GoalControlResult,
  GoalSubmissionRecovery,
  SessionGoalView,
} from "@source-inc/gents-desktop-client";
import { createChatActions } from "../src/hooks/chatActions";
import { createChatStore } from "../src/hooks/chatStore";
import { loadGoalDrafts } from "../src/hooks/goalDrafts";
import { selection } from "../src/hooks/selectionStore";
import { admittingProjection, shellStores } from "./shell-fixture";
import { appliedGoalResult as result } from "./goal-delivery-fixture";

const records = new Map<string, string>();
let denyWrite = false;
beforeEach(() => {
  records.clear();
  denyWrite = false;
  vi.stubGlobal("localStorage", {
    get length() {
      return records.size;
    },
    key: (index: number) => [...records.keys()][index] ?? null,
    getItem: (key: string) => records.get(key) ?? null,
    setItem: (key: string, value: string) => {
      if (denyWrite) throw new Error("Storage is full");
      records.set(key, value);
    },
    removeItem: (key: string) => records.delete(key),
  });
});
afterEach(() => vi.unstubAllGlobals());

function fixture(sessionId: string | null = null) {
  const stores = shellStores({
    deployments: [
      {
        nodeDid: "node",
        node: { nodeDid: "node", defaultAgentId: "agent" },
        agents: [],
        sessions: [],
        mailboxItems: [],
      },
    ],
    selection: { nodeDid: "node", sessionId },
  });
  stores.client.setState({
    snapshot: { client: { clientDid: "client" } } as DesktopClientSnapshot,
  });
  const control = vi
    .fn<(request: GoalControlRequest) => Promise<GoalControlResult>>()
    .mockRejectedValue(new Error("Outer reply was lost"));
  const read = vi.fn<() => Promise<GoalSubmissionRecovery>>();
  const resume = vi
    .fn<(request: GoalControlRequest) => Promise<GoalControlResult>>()
    .mockImplementation(async (request) => result(request));
  const ack = vi
    .fn<() => Promise<void>>()
    .mockRejectedValue(new Error("Acknowledgement reply was lost"));
  const goal = vi.fn<() => Promise<SessionGoalView>>().mockResolvedValue({
    nodeDid: "node",
    sessionId,
    sessionDocId: "session-doc",
    goal: { revision: { document: "goal-doc", composite: "goal-version" } },
  } as SessionGoalView);
  const api = {
    controlGoal: control,
    recoverGoalSubmission: read,
    resumeGoalSubmission: resume,
    acknowledgeGoalSubmission: ack,
    fetchSessionGoal: goal,
  } as unknown as DesktopApiAdapter;
  const report = vi.fn();
  const refresh = vi.fn();
  const projection = admittingProjection("agent");
  const actions = () =>
    createChatActions({
      api,
      stores,
      project: () => projection,
      refreshSession: refresh,
      refreshSnapshot: vi.fn(),
      reportFailure: report,
    });
  return {
    stores,
    api,
    control,
    read,
    resume,
    ack,
    goal,
    report,
    refresh,
    projection,
    actions,
  };
}

function applied(request: GoalControlRequest): GoalSubmissionRecovery {
  const value = result(request);
  return {
    kind: "prepared",
    receipt: value.submissionReceipt!,
    acknowledged: false,
    outcome: { kind: "applied", result: value },
  };
}

it("retains a fixed client, command, destination and authored input before the first effect", async () => {
  const f = fixture();
  f.control.mockImplementationOnce(async (request) => {
    const retained = loadGoalDrafts().goalDrafts;
    expect(retained).toHaveLength(1);
    expect(JSON.parse(retained[0].commandJson)).toEqual(request);
    expect(request).toMatchObject({
      clientDid: "client",
      action: { objective: "Original goal" },
      cwd: "/first",
    });
    expect(request.commandId).toEqual(expect.any(String));
    expect(request.sessionId).toEqual(expect.any(String));
    throw new Error("Outer reply was lost");
  });
  const actions = f.actions();
  actions.setChatFolder("/first");
  expect(await actions.setGoal("Original goal")).toBeNull();
  expect(f.stores.selection.getState().sessionId).toBeNull();
  expect(await actions.setGoal("Different goal")).toBeNull();
  expect(f.control).toHaveBeenCalledTimes(1);
  expect(f.ack).not.toHaveBeenCalled();
});

it("reloads an unresolved opening and explicitly resumes exactly its original input", async () => {
  const f = fixture();
  const actions = f.actions();
  actions.setChatFolder("/original-folder");
  await actions.setGoal("Original goal");
  const request = f.control.mock.calls[0][0];
  f.stores.chat = createChatStore();
  const reloaded = f.actions();
  reloaded.setChatFolder("/newer-folder");
  f.read.mockResolvedValue({
    kind: "unconfirmed",
    nodeDid: "node",
    commandId: request.commandId!,
  });
  expect(await reloaded.recoverGoalChange(request.commandId!)).toBeNull();
  expect(f.control).toHaveBeenCalledTimes(1);
  f.control.mockImplementationOnce(async (original) => result(original));
  const recovered = await reloaded.recoverGoalChange(request.commandId!, true);
  expect(f.control.mock.calls[1][0]).toEqual(request);
  expect(f.stores.selection.getState().sessionId).toBe(request.sessionId);
  expect(f.stores.chat.getState().folders).toMatchObject({
    "": "/newer-folder",
    [request.sessionId!]: "/original-folder",
  });
  expect(recovered?.requestId).toBe(result(request).requestId);
});

it("preserves queued Goal attribution when a lost reply is recovered", async () => {
  const f = fixture("session");
  f.projection.shellProjection.nonEmptyContentSendStatus = {
    kind: "queue",
    turnState: "running",
    hint: "Queue message",
  };
  await f.actions().setGoal("Original queued goal");
  const request = f.control.mock.calls[0][0];
  f.stores.chat = createChatStore();
  f.read.mockResolvedValue(applied(request));
  await f.actions().recoverGoalChange(request.commandId!);
  expect(f.stores.chat.getState().sentTurns).toContainEqual({
    sessionId: "session",
    requestId: result(request).requestId,
    queued: true,
  });
});

it("resumes the native prepared command instead of preparing another one", async () => {
  const f = fixture("session");
  await f
    .actions()
    .controlGoal("session", { kind: "pause" })
    .catch(() => {});
  const request = f.control.mock.calls[0][0];
  expect(request.expected).toEqual({ document: "goal-doc", composite: "goal-version" });
  f.read.mockResolvedValue({
    kind: "prepared",
    receipt: result(request).submissionReceipt!,
    acknowledged: false,
    outcome: null,
  });
  await f.actions().recoverGoalChange(request.commandId!, true);
  expect(f.resume).toHaveBeenCalledWith(request, result(request).submissionReceipt);
  expect(f.control).toHaveBeenCalledTimes(1);
});

it("retains an observed native preparation and never replaces it when a later lookup cannot find it", async () => {
  const f = fixture();
  await f.actions().setGoal("Original goal");
  const request = f.control.mock.calls[0][0];
  const receipt = result(request).submissionReceipt!;
  f.read.mockResolvedValueOnce({
    kind: "prepared",
    receipt,
    acknowledged: false,
    outcome: null,
  });
  await f.actions().recoverGoalChange(request.commandId!);
  expect(loadGoalDrafts().goalDrafts[0].receipt).toEqual(receipt);
  f.stores.chat = createChatStore();
  f.read.mockResolvedValue({
    kind: "unconfirmed",
    nodeDid: "node",
    commandId: request.commandId!,
  });
  f.resume.mockRejectedValueOnce(new Error("The retained preparation is unavailable"));
  await expect(f.actions().recoverGoalChange(request.commandId!, true)).rejects.toThrow(
    "preparation is unavailable",
  );
  expect(f.resume).toHaveBeenCalledWith(request, receipt);
  expect(f.control).toHaveBeenCalledTimes(1);
});

it("refuses a later receipt that changes the observed signed preparation", async () => {
  const f = fixture();
  await f.actions().setGoal("Original goal");
  const request = f.control.mock.calls[0][0];
  const receipt = result(request).submissionReceipt!;
  f.read.mockResolvedValueOnce({
    kind: "prepared",
    receipt,
    acknowledged: false,
    outcome: null,
  });
  await f.actions().recoverGoalChange(request.commandId!);
  f.read.mockResolvedValue({
    kind: "prepared",
    receipt: {
      ...receipt,
      reference: { ...receipt.reference, commandDigest: "sha256:" + "b".repeat(64) },
    },
    acknowledged: false,
    outcome: null,
  });
  await expect(f.actions().recoverGoalChange(request.commandId!, true)).rejects.toThrow(
    "changed its original signed preparation",
  );
  expect(f.resume).not.toHaveBeenCalled();
  expect(f.ack).not.toHaveBeenCalled();
});

it("freezes the original folder while the Goal observation is pending", async () => {
  const f = fixture("session");
  let finish!: (value: SessionGoalView) => void;
  f.goal.mockImplementationOnce(
    () =>
      new Promise((resolve) => {
        finish = resolve;
      }),
  );
  const actions = f.actions();
  actions.setChatFolder("/original");
  const pending = actions.setGoal("Original goal");
  actions.setChatFolder("/newer");
  finish({
    nodeDid: "node",
    sessionId: "session",
    sessionDocId: "session-doc",
    goal: null,
  } as SessionGoalView);
  await pending;
  expect(f.control.mock.calls[0][0].cwd).toBe("/original");
});

it("refuses to reinterpret an authored Goal under a client selected during observation", async () => {
  const f = fixture("session");
  let finish!: (value: SessionGoalView) => void;
  f.goal.mockImplementationOnce(
    () =>
      new Promise((resolve) => {
        finish = resolve;
      }),
  );
  const pending = f.actions().setGoal("Original goal");
  f.stores.client.setState({
    snapshot: { client: { clientDid: "new-client" } } as DesktopClientSnapshot,
  });
  finish({
    nodeDid: "node",
    sessionId: "session",
    sessionDocId: "session-doc",
    goal: null,
  } as SessionGoalView);
  await pending;
  expect(f.control).not.toHaveBeenCalled();
  expect(loadGoalDrafts().goalDrafts).toHaveLength(0);
});

it.each(["unavailable", "different chat"])(
  "does not infer an empty Goal from an observation that is %s",
  async (kind) => {
    const f = fixture("session");
    f.goal.mockResolvedValueOnce(
      kind === "unavailable"
        ? (null as unknown as SessionGoalView)
        : ({
            nodeDid: "other-node",
            sessionId: "other-session",
            sessionDocId: "other-document",
            goal: null,
          } as SessionGoalView),
    );
    await f.actions().setGoal("Original goal");
    expect(f.control).not.toHaveBeenCalled();
    expect(loadGoalDrafts().goalDrafts).toHaveLength(0);
  },
);

it("persists confirmation before acknowledgement and recovers a lost acknowledgement without resubmission", async () => {
  const f = fixture();
  await f.actions().setGoal("Original goal");
  const request = f.control.mock.calls[0][0];
  f.read.mockResolvedValue(applied(request));
  f.ack.mockImplementationOnce(async () => {
    expect(loadGoalDrafts().goalDrafts[0].result).toEqual(result(request));
    throw new Error("Acknowledgement reply was lost");
  });
  await f.actions().recoverGoalChange(request.commandId!);
  await Promise.resolve();
  f.stores.chat = createChatStore();
  f.ack.mockResolvedValueOnce();
  await f.actions().recoverGoalChange(request.commandId!);
  await vi.waitFor(() => expect(loadGoalDrafts().goalDrafts).toHaveLength(0));
  expect(f.control).toHaveBeenCalledTimes(1);
  expect(f.resume).not.toHaveBeenCalled();
});

it("refuses dispatch when the caller form cannot be retained", async () => {
  const f = fixture();
  denyWrite = true;
  await f.actions().setGoal("Original goal");
  expect(f.control).not.toHaveBeenCalled();
  expect(f.ack).not.toHaveBeenCalled();
});

it("does not acknowledge an applied outcome when saving the caller result fails", async () => {
  const f = fixture();
  f.control.mockImplementationOnce(async (request) => {
    denyWrite = true;
    return result(request);
  });
  expect(await f.actions().setGoal("Original goal")).toBeNull();
  expect(f.ack).not.toHaveBeenCalled();
  expect(loadGoalDrafts().goalDrafts[0].result).toBeUndefined();
});

it("does not let a late confirmed Goal select another chat or replace its workflow", async () => {
  const f = fixture();
  f.ack.mockResolvedValue(undefined);
  let finish!: (value: GoalControlResult) => void;
  f.control.mockImplementationOnce(
    () =>
      new Promise((resolve) => {
        finish = resolve;
      }),
  );
  const pending = f.actions().setGoal("Original goal");
  const request = f.control.mock.calls[0][0];
  selection.selectSession(f.stores.selection, "another-chat");
  f.stores.chat.setState({
    localWorkflow: {
      kind: "awaitingObservation",
      nodeDid: "node",
      sessionId: "another-chat",
      requestId: "later",
    },
  });
  finish(result(request));
  await pending;
  expect(f.stores.selection.getState().sessionId).toBe("another-chat");
  expect(f.stores.chat.getState().localWorkflow).toMatchObject({
    sessionId: "another-chat",
    requestId: "later",
  });
  await vi.waitFor(() => expect(f.ack).toHaveBeenCalledTimes(1));
  expect(loadGoalDrafts().goalDrafts[0].result).toEqual(result(request));
});

it("keeps recovered confirmation available without selecting or refreshing a replacement chat", async () => {
  const f = fixture();
  await f.actions().setGoal("Original goal");
  const request = f.control.mock.calls[0][0];
  selection.selectSession(f.stores.selection, "another-chat");
  f.read.mockResolvedValue(applied(request));
  f.ack.mockResolvedValue(undefined);
  await f.actions().recoverGoalChange(request.commandId!);
  await vi.waitFor(() => expect(f.ack).toHaveBeenCalledTimes(1));
  expect(f.stores.selection.getState().sessionId).toBe("another-chat");
  expect(f.refresh).not.toHaveBeenCalled();
  expect(loadGoalDrafts().goalDrafts[0].result).toEqual(result(request));
});

it("does not recover or reauthor an old client's retained operation after an identity switch", async () => {
  const f = fixture();
  await f.actions().setGoal("Original goal");
  const request = f.control.mock.calls[0][0];
  f.stores.client.setState({
    snapshot: { client: { clientDid: "different-client" } } as DesktopClientSnapshot,
  });
  await expect(f.actions().recoverGoalChange(request.commandId!, true)).rejects.toThrow(
    "unavailable for this client",
  );
  expect(f.read).not.toHaveBeenCalled();
  expect(f.resume).not.toHaveBeenCalled();
});

it.each(["client", "runtime", "destination", "command", "initial request"])(
  "refuses a confirmed outcome with the wrong %s binding",
  async (field) => {
    const f = fixture();
    await f.actions().setGoal("Original goal");
    const request = f.control.mock.calls[0][0];
    const recovery = applied(request);
    if (recovery.kind !== "prepared" || recovery.outcome?.kind !== "applied")
      throw new Error("fixture outcome");
    if (field === "client") recovery.receipt.clientDid = "other";
    if (field === "runtime") recovery.receipt.nodeDid = "other";
    if (field === "destination") recovery.receipt.sessionId = "other";
    if (field === "command") recovery.receipt.reference.commandId = "other";
    if (field === "initial request") recovery.outcome.result.requestId = "other";
    f.read.mockResolvedValue(recovery);
    await expect(f.actions().recoverGoalChange(request.commandId!)).rejects.toThrow();
    expect(f.ack).not.toHaveBeenCalled();
    expect(f.stores.selection.getState().sessionId).toBeNull();
  },
);

it("requires a confirmed native refusal before a saved change can be dismissed", async () => {
  const f = fixture();
  await f.actions().setGoal("Original goal");
  const request = f.control.mock.calls[0][0];
  f.read.mockResolvedValue({
    kind: "unconfirmed",
    nodeDid: "node",
    commandId: request.commandId!,
  });
  await expect(f.actions().dismissGoalChange(request.commandId!)).rejects.toThrow(
    "not a confirmed refusal",
  );
  expect(loadGoalDrafts().goalDrafts).toHaveLength(1);
  f.read.mockResolvedValue({
    kind: "prepared",
    receipt: result(request).submissionReceipt!,
    acknowledged: false,
    outcome: { kind: "rejected", reason: "The Goal changed" },
  });
  await f.actions().recoverGoalChange(request.commandId!);
  expect(loadGoalDrafts().goalDrafts[0].rejection?.reason).toBe("The Goal changed");
  f.ack.mockResolvedValueOnce();
  await f.actions().dismissGoalChange(request.commandId!);
  expect(loadGoalDrafts().goalDrafts).toHaveLength(0);
  expect(f.control).toHaveBeenCalledTimes(1);
});

it("can query an applied result when the saved body is unreadable, without using it as new input", async () => {
  const f = fixture();
  await f.actions().setGoal("Original goal");
  const request = f.control.mock.calls[0][0];
  const key = [...records.keys()].find((key) =>
    key.startsWith("gents.chat.goalDraft"),
  )!;
  const saved = JSON.parse(records.get(key)!);
  records.set(key, JSON.stringify({ ...saved, commandJson: "unreadable" }));
  f.read.mockResolvedValue(applied(request));
  await f.actions().recoverGoalChange(request.commandId!);
  expect(f.stores.selection.getState().sessionId).toBe(request.sessionId);
  expect(f.control).toHaveBeenCalledTimes(1);
});

it("keeps the original identity queryable when cached presentation fields are malformed", async () => {
  const f = fixture();
  await f.actions().setGoal("Original goal");
  const request = f.control.mock.calls[0][0];
  const key = [...records.keys()].find((key) =>
    key.startsWith("gents.chat.goalDraft"),
  )!;
  const saved = JSON.parse(records.get(key)!);
  records.set(
    key,
    JSON.stringify({
      ...saved,
      result: { sessionId: "other" },
      rejection: { reason: { bad: true } },
    }),
  );
  expect(loadGoalDrafts().goalDrafts[0].result).toBeUndefined();
  expect(loadGoalDrafts().goalDrafts[0].rejection).toBeUndefined();
  f.read.mockResolvedValue(applied(request));
  await f.actions().recoverGoalChange(request.commandId!);
  expect(f.control).toHaveBeenCalledTimes(1);
  expect(f.stores.selection.getState().sessionId).toBe(request.sessionId);
});
