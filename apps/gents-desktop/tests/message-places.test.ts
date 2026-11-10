import { act, renderHook } from "@testing-library/react";
import { describe, expect, it } from "vitest";

import {
  messagePlaces,
  openAt,
  useOpening,
  usePlaceRequest,
} from "../src/ui/app/messagePlaces";

describe("a place in a session asked for from elsewhere", () => {
  it("reaches only its own session, and each ask is a new request", () => {
    const here = renderHook(() => usePlaceRequest("s"));
    const elsewhere = renderHook(() => usePlaceRequest("other"));
    act(() => openAt({ sessionId: "s", sequence: 41, itemKey: "marker" }));
    const first = here.result.current!;
    expect(first).toMatchObject({ sessionId: "s", sequence: 41, itemKey: "marker" });
    expect(elsewhere.result.current).toBeNull();

    act(() => openAt({ sessionId: "s", sequence: 41, itemKey: "marker" }));
    expect(here.result.current!.id).not.toBe(first.id);
    /* landing on an earlier request leaves the newer one standing */
    act(() => messagePlaces.landed(first.id, true));
    expect(here.result.current).not.toBeNull();
    act(() => messagePlaces.landed(here.result.current!.id, true));
    expect(here.result.current).toBeNull();
  });

  it("opens its row once landed, only when asked to, and only once", () => {
    const marker = renderHook(() => useOpening("marker"));
    act(() => openAt({ sessionId: "s", sequence: 41, itemKey: "marker" }));
    act(() => messagePlaces.landed(messagePlaces.request()!.id, true));
    expect(marker.result.current).toBe(false);

    /* landing on its message instead, the row stays shut when drawn later */
    act(() => openAt({ sessionId: "s", sequence: 41, itemKey: "marker", open: true }));
    act(() => messagePlaces.landed(messagePlaces.request()!.id, false));
    expect(marker.result.current).toBe(false);

    act(() => openAt({ sessionId: "s", sequence: 41, itemKey: "marker", open: true }));
    expect(marker.result.current).toBe(false);
    act(() => messagePlaces.landed(messagePlaces.request()!.id, true));
    expect(marker.result.current).toBe(true);
    act(() => messagePlaces.opened("another row"));
    expect(marker.result.current).toBe(true);
    act(() => messagePlaces.opened("marker"));
    expect(marker.result.current).toBe(false);
  });
});
