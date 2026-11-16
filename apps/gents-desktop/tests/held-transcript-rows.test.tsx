import { act, renderHook, waitFor } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import { BridgeInvokeError } from "@source-inc/gents-desktop-client";
import type {
  RenderedTimelineItem,
  SessionRowsView,
} from "@source-inc/gents-desktop-client";
import { useHeldRows } from "../src/ui/lib/useHeldRows";
import { deferred } from "./deferred";

const message = (sequence: number) =>
  ({
    kind: "userMessage",
    itemKey: `message-${sequence}`,
    sequence,
    content: `${sequence}`,
  }) as RenderedTimelineItem;
const page = (overrides: Partial<SessionRowsView> = {}): SessionRowsView => ({
  sessionId: "session",
  nodeDid: "node",
  rows: [message(10)],
  coveredFrom: 10,
  coveredThrough: 19,
  hasOlder: true,
  hasNewer: true,
  complete: true,
  unreadable: null,
  queryCount: 1,
  ...overrides,
});

describe("held transcript history", () => {
  it("retains visible history when a page is unavailable, then loads it on retry", async () => {
    const load = vi
      .fn()
      .mockResolvedValueOnce(page())
      .mockResolvedValueOnce(
        page({
          rows: [],
          coveredFrom: null,
          coveredThrough: null,
          hasOlder: false,
          hasNewer: false,
          complete: false,
          unreadable: "Connect to the runtime to read this session.",
        }),
      )
      .mockResolvedValueOnce(
        page({ rows: [message(5)], coveredFrom: 5, coveredThrough: 9 }),
      );
    const { result } = renderHook(() =>
      useHeldRows("node", "session", [message(20)], true, load),
    );
    act(() => result.current.loadOlder());
    await waitFor(() =>
      expect(result.current.items.map((row) => row.itemKey)).toEqual([
        "message-10",
        "message-20",
      ]),
    );
    act(() => result.current.loadOlder());
    await waitFor(() =>
      expect(result.current.failure?.message).toContain("Connect to the runtime"),
    );
    expect(result.current.items.map((row) => row.itemKey)).toEqual([
      "message-10",
      "message-20",
    ]);
    expect(result.current.olderBefore).toBe(10);
    act(() => result.current.retryRead());
    await waitFor(() =>
      expect(result.current.items.map((row) => row.itemKey)).toEqual([
        "message-5",
        "message-10",
        "message-20",
      ]),
    );
    expect(result.current.failure).toBeNull();
  });

  it("treats an empty readable page as empty, not an access denial", async () => {
    const load = vi
      .fn()
      .mockResolvedValue(page({ rows: [], coveredFrom: 0, hasOlder: false }));
    const { result } = renderHook(() =>
      useHeldRows("node", "session", [message(20)], true, load),
    );
    act(() => result.current.loadOlder());
    await waitFor(() => expect(result.current.olderBefore).toBeNull());
    expect(result.current.failure).toBeNull();
    expect(result.current.items.map((row) => row.itemKey)).toEqual(["message-20"]);
  });

  it("does not retry a denied page and discards previously held history", async () => {
    const denied = new BridgeInvokeError({
      code: "permissionDenied",
      message: "denied",
      retryable: false,
      endpoint: null,
    });
    const load = vi.fn().mockResolvedValueOnce(page()).mockRejectedValueOnce(denied);
    const { result } = renderHook(() =>
      useHeldRows("node", "session", [message(20)], true, load),
    );
    act(() => result.current.loadOlder());
    await waitFor(() => expect(result.current.items).toHaveLength(2));
    act(() => result.current.loadOlder());
    await waitFor(() => expect(result.current.failure?.denied).toBe(true));
    expect(result.current.items.map((row) => row.itemKey)).toEqual(["message-20"]);
    expect(load).toHaveBeenCalledTimes(2);
  });
  it("drops a read that a jump replaced, and settles only on the jump's own read", async () => {
    const older = deferred<SessionRowsView>();
    const around = deferred<SessionRowsView>();
    const load = vi
      .fn()
      .mockReturnValueOnce(older.promise)
      .mockReturnValueOnce(around.promise);
    const { result } = renderHook(() =>
      useHeldRows("node", "session", [message(20)], true, load),
    );
    act(() => result.current.loadOlder());
    act(() => result.current.jump(3));
    expect(load).toHaveBeenCalledTimes(2);
    await act(async () => older.resolve(page({ rows: [message(10)] })));
    expect(result.current.loading).toBe("jump");
    expect(result.current.items.map((row) => row.itemKey)).toEqual(["message-20"]);
    await act(async () =>
      around.resolve(page({ rows: [message(3)], coveredFrom: 0, coveredThrough: 5 })),
    );
    expect(result.current.loading).toBeNull();
    expect(result.current.items.map((row) => row.itemKey)).toContain("message-3");
    expect(result.current.items.map((row) => row.itemKey)).not.toContain("message-10");
  });

  it("keeps the next session's read in flight when the last session's read lands", async () => {
    const first = deferred<SessionRowsView>();
    const second = deferred<SessionRowsView>();
    const load = vi
      .fn()
      .mockReturnValueOnce(first.promise)
      .mockReturnValueOnce(second.promise);
    const { result, rerender } = renderHook(
      ({ sessionId }) => useHeldRows("node", sessionId, [message(20)], true, load),
      { initialProps: { sessionId: "session" } },
    );
    act(() => result.current.loadOlder());
    rerender({ sessionId: "other" });
    expect(result.current.loading).toBeNull();
    act(() => result.current.loadOlder());
    expect(load).toHaveBeenCalledTimes(2);
    await act(async () => first.resolve(page()));
    expect(result.current.loading).toBe("older");
    act(() => result.current.loadOlder());
    expect(load).toHaveBeenCalledTimes(2);
    await act(async () => second.resolve(page({ sessionId: "other" })));
    expect(result.current.loading).toBeNull();
    expect(result.current.items.map((row) => row.itemKey)).toEqual([
      "message-10",
      "message-20",
    ]);
  });

  it("holds rows for a session on its node, not for the same id on another", async () => {
    const load = vi.fn().mockResolvedValue(page());
    const { result, rerender } = renderHook(
      ({ nodeDid }) => useHeldRows(nodeDid, "session", [message(20)], true, load),
      { initialProps: { nodeDid: "node" } },
    );
    act(() => result.current.loadOlder());
    await waitFor(() => expect(result.current.items).toHaveLength(2));
    rerender({ nodeDid: "another-node" });
    expect(result.current.items.map((row) => row.itemKey)).toEqual(["message-20"]);
  });
});
