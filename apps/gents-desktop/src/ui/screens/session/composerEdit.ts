import {
  restoredAttachment,
  useRequestAttachments,
} from "../../../hooks/useAttachments";
import { useApp } from "@/app/AppContext";
import { writeAttachments } from "../../../hooks/draftStore";
import type {
  RequestAttachment,
  AttachmentView,
} from "@source-inc/gents-desktop-client";
import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useState,
  type RefObject,
} from "react";
import type { PendingQueueEntryView } from "@source-inc/gents-desktop-client";
import { rewriteEdit, type QueueEdit } from "@source-inc/gents-desktop-client";

export type ComposerEdit = {
  editingDocId: string | null;
  begin: (entry: PendingQueueEntryView) => void;
};

export const ComposerEditContext = createContext<ComposerEdit | null>(null);

export const useComposerEdit = () => useContext(ComposerEditContext);

export const lastEditable = (queue: readonly PendingQueueEntryView[] | null) =>
  queue?.filter((entry) => entry.editable).slice(-1)[0] ?? null;

export function useComposerEditing({
  queue,
  onEdit,
  setDraft,
  send,
  textarea,
}: {
  queue: readonly PendingQueueEntryView[] | null;
  onEdit: ((edit: QueueEdit) => Promise<void>) | undefined;
  setDraft: (update: (current: string) => string) => void;
  send: (text: string, attachments?: RequestAttachment[]) => Promise<boolean>;
  textarea: RefObject<HTMLElement | null>;
}) {
  const { drafts, project } = useApp();
  const loadAttachments = useRequestAttachments();
  const [editing, setEditing] = useState<{
    docId: string;
    requestId: string;
    attachments?: AttachmentView[] | null;
    text: string;
    reached: boolean;
  } | null>(null);
  const [saving, setSaving] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const focus = useCallback(
    () => requestAnimationFrame(() => textarea.current?.focus()),
    [textarea],
  );

  const begin = useCallback(
    (entry: PendingQueueEntryView) => {
      setError(null);
      setEditing({
        docId: entry.requestDocId,
        requestId: entry.requestId,
        attachments: entry.attachments,
        text: entry.content,
        reached: false,
      });
      focus();
    },
    [focus],
  );
  const setText = (text: string) => {
    if (saving) return;
    setEditing((current) => (current ? { ...current, text } : current));
  };
  const sourceAttachments = async () => {
    if (!editing?.attachments?.length) return [];
    return loadAttachments(editing.requestId);
  };
  const cancel = async () => {
    if (saving) return;
    if (!editing?.reached) {
      setEditing(null);
      focus();
      return;
    }
    setSaving(true);
    setError(null);
    try {
      const key = project().draftKey;
      const restored = await sourceAttachments();
      writeAttachments(drafts, key, (current) => [
        ...current,
        ...restored.map(restoredAttachment),
      ]);
      const text = editing.text;
      setDraft((current) => (current.trim() ? `${current}\n\n${text}` : text));
      setEditing(null);
      focus();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setSaving(false);
    }
  };
  const sendAsNew = async (text: string) => {
    if (saving) return;
    setSaving(true);
    try {
      if (await send(text, await sourceAttachments())) setEditing(null);
      else setEditing((current) => current && { ...current, text });
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setSaving(false);
    }
  };

  useEffect(() => {
    if (!editing || editing.reached || saving || !queue) return;
    if (!queue.some((entry) => entry.requestDocId === editing.docId))
      setEditing({ ...editing, reached: true });
  }, [editing, queue, saving]);

  const save = async (text: string) => {
    if (
      !editing ||
      editing.reached ||
      saving ||
      !onEdit ||
      !queue ||
      (!text.trim() && !editing.attachments?.length)
    )
      return;
    const entry = queue.find((item) => item.requestDocId === editing.docId);
    if (!entry) {
      setEditing({ ...editing, text, reached: true });
      return;
    }
    setSaving(true);
    try {
      await onEdit(rewriteEdit(queue, entry, text));
      setEditing(null);
      focus();
    } catch {
      setEditing((current) => current && { ...current, text });
    } finally {
      setSaving(false);
    }
  };

  return { editing, saving, error, begin, cancel, setText, save, sendAsNew };
}
