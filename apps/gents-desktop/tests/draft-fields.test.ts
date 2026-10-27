import { act, renderHook } from "@testing-library/react";
import { describe, expect, it } from "vitest";
import { createDraftStore, useDraftField, writeDraft } from "../src/hooks/draftStore";

describe("a draft's saved field", () => {
  it("is kept beside a draft with text, and dropped when the draft empties", () => {
    const store = createDraftStore();
    const { result } = renderHook(() => useDraftField(store, "draft"));
    act(() => result.current[1]("before any text"));
    expect(result.current[0]).toBeNull();
    act(() => writeDraft(store, "draft", "see the log"));
    act(() => result.current[1]('{"root":{}}'));
    expect(result.current[0]).toBe('{"root":{}}');
    act(() => writeDraft(store, "draft", ""));
    expect(result.current[0]).toBeNull();
  });

  it("belongs to one draft", () => {
    const store = createDraftStore();
    writeDraft(store, "a", "first");
    writeDraft(store, "b", "second");
    const a = renderHook(() => useDraftField(store, "a"));
    const b = renderHook(() => useDraftField(store, "b"));
    act(() => a.result.current[1]('{"root":"a"}'));
    expect(b.result.current[0]).toBeNull();
  });
});
