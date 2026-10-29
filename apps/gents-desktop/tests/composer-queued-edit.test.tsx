import { act, renderHook } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";
import type { PendingQueueEntryView } from "@source-inc/gents-desktop-client";
import { useComposerEditing } from "../src/ui/screens/session/composerEdit";

const loadAttachments = vi.hoisted(() => vi.fn());
vi.mock("../src/hooks/useAttachments", () => ({
  useRequestAttachments: () => loadAttachments,
  restoredAttachment: (content: unknown) => ({ content }),
}));
vi.mock("../src/ui/app/AppContext", () => ({
  useApp: () => ({ drafts: {}, project: () => ({ draftKey: "draft" }) }),
}));

describe("queued composer edits", () => {
  it("retains captured context unless the person removes it", async () => {
    const capture = {
      kind: "file" as const,
      source: "editor" as const,
      capturedAt: "2026-10-29T00:00:00Z",
      uri: "file:///project/a.rs",
      terminalName: null,
      workspaceUri: null,
      version: 2,
      unsaved: true,
      range: null,
      content: "unsaved contents",
    };
    const entry: PendingQueueEntryView = {
      requestDocId: "doc",
      requestId: "request",
      content: "old",
      editable: true,
      steerable: true,
      editGroup: "group",
      capturedContext: [capture],
    };
    const onEdit = vi.fn().mockResolvedValue(undefined);
    const { result } = renderHook(() =>
      useComposerEditing({
        queue: [entry],
        onEdit,
        setDraft: vi.fn(),
        send: vi.fn(),
        textarea: { current: null },
      }),
    );
    act(() => result.current.begin(entry));
    await act(async () => result.current.save(""));
    expect(onEdit).toHaveBeenCalledWith(
      expect.objectContaining({
        messages: [
          expect.objectContaining({
            authoredInput: expect.objectContaining({ captured_context: [capture] }),
          }),
        ],
      }),
    );
  });

  it("clears captured context explicitly on the replacement", async () => {
    const capture = {
      kind: "file" as const,
      source: "editor" as const,
      capturedAt: "2026-10-29T00:00:00Z",
      uri: "file:///project/a.rs",
      terminalName: null,
      workspaceUri: null,
      version: 2,
      unsaved: true,
      range: null,
      content: "unsaved contents",
    };
    const entry: PendingQueueEntryView = {
      requestDocId: "doc",
      requestId: "request",
      content: "old",
      editable: true,
      steerable: true,
      editGroup: "group",
      capturedContext: [capture],
    };
    const onEdit = vi.fn().mockResolvedValue(undefined);
    const { result } = renderHook(() =>
      useComposerEditing({
        queue: [entry],
        onEdit,
        setDraft: vi.fn(),
        send: vi.fn(),
        textarea: { current: null },
      }),
    );
    act(() => result.current.begin(entry));
    act(() => result.current.removeCapture(0));
    await act(async () => result.current.save("changed"));
    expect(onEdit).toHaveBeenCalledWith(
      expect.objectContaining({
        messages: [
          expect.objectContaining({
            authoredInput: expect.objectContaining({ captured_context: [] }),
          }),
        ],
      }),
    );
  });
  it("sends retained bytes and updated authored fields without changing the composer draft", async () => {
    const attachment = { name: "note.txt", mediaType: "text/plain", data: "aGk=" };
    const entry: PendingQueueEntryView & { selectedSkillIds: string[] } = {
      requestDocId: "doc",
      requestId: "request",
      content: "old",
      editable: true,
      steerable: true,
      editGroup: "group",
      selectedSkillIds: ["review", "other"],
      attachments: [
        { name: "note.txt", mediaType: "text/plain", digest: "hash", size: 2 },
      ],
    };
    loadAttachments.mockResolvedValue([attachment]);
    const onEdit = vi.fn().mockResolvedValue(undefined);
    const setDraft = vi.fn();
    const { result } = renderHook(() =>
      useComposerEditing({
        queue: [entry],
        onEdit,
        setDraft,
        send: vi.fn(),
        textarea: { current: null },
      }),
    );
    act(() => result.current.begin(entry));
    act(() => result.current.removeSkill("review"));
    await act(async () => result.current.save("changed"));
    expect(onEdit).toHaveBeenCalledWith({
      expectedRequestDocIds: ["doc"],
      selectedRequestDocIds: ["doc"],
      messages: [
        {
          requestDocId: "doc",
          content: "changed",
          authoredInput: {
            selected_skill_ids: ["other"],
            captured_context: [],
            attachments: [attachment],
          },
        },
      ],
    });
    expect(setDraft).not.toHaveBeenCalled();
  });

  it("keeps the edit open when retained attachment bytes are unavailable", async () => {
    const entry: PendingQueueEntryView = {
      requestDocId: "doc",
      requestId: "request",
      content: "old",
      editable: true,
      steerable: true,
      editGroup: "group",
      attachments: [
        { name: "note.txt", mediaType: "text/plain", digest: "hash", size: 2 },
      ],
    };
    loadAttachments.mockRejectedValue(new Error("Cannot load attachment"));
    const onEdit = vi.fn();
    const { result } = renderHook(() =>
      useComposerEditing({
        queue: [entry],
        onEdit,
        setDraft: vi.fn(),
        send: vi.fn(),
        textarea: { current: null },
      }),
    );
    act(() => result.current.begin(entry));
    await act(async () => result.current.save("changed"));
    expect(onEdit).not.toHaveBeenCalled();
    expect(result.current.editing?.text).toBe("changed");
    expect(result.current.error).toBe("Cannot load attachment");
  });

  it("removes the last attachment without loading its bytes", async () => {
    const entry: PendingQueueEntryView = {
      requestDocId: "doc",
      requestId: "request",
      content: "old",
      editable: true,
      steerable: true,
      editGroup: "group",
      attachments: [
        { name: "note.txt", mediaType: "text/plain", digest: "hash", size: 2 },
      ],
    };
    loadAttachments.mockReset();
    const onEdit = vi.fn().mockResolvedValue(undefined);
    const { result } = renderHook(() =>
      useComposerEditing({
        queue: [entry],
        onEdit,
        setDraft: vi.fn(),
        send: vi.fn(),
        textarea: { current: null },
      }),
    );
    act(() => result.current.begin(entry));
    act(() => result.current.removeAttachment(0));
    await act(async () => result.current.save("changed"));
    expect(loadAttachments).not.toHaveBeenCalled();
    expect(onEdit).toHaveBeenCalledWith(
      expect.objectContaining({
        messages: [
          expect.objectContaining({
            authoredInput: expect.objectContaining({ attachments: [] }),
          }),
        ],
      }),
    );
  });

  it("adds a new attachment without changing the ordinary draft", async () => {
    const entry: PendingQueueEntryView = {
      requestDocId: "doc",
      requestId: "request",
      content: "old",
      editable: true,
      steerable: true,
      editGroup: "group",
    };
    const onEdit = vi.fn().mockResolvedValue(undefined);
    const setDraft = vi.fn();
    const { result } = renderHook(() =>
      useComposerEditing({
        queue: [entry],
        onEdit,
        setDraft,
        send: vi.fn(),
        textarea: { current: null },
      }),
    );
    act(() => result.current.begin(entry));
    const added = { name: "new.txt", mediaType: "text/plain", data: "aGk=" };
    await act(async () => result.current.save("changed", [added]));
    expect(onEdit).toHaveBeenCalledWith(
      expect.objectContaining({
        messages: [
          expect.objectContaining({
            authoredInput: expect.objectContaining({ attachments: [added] }),
          }),
        ],
      }),
    );
    expect(setDraft).not.toHaveBeenCalled();
  });
});
