import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useState,
  type RefObject,
} from "react";
import type { PendingQueueEntryView } from "@source-inc/gents-desktop-client";
import { rewriteEdit, type QueueEdit } from "../pendingQueue";

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
  send: (text: string) => Promise<boolean>;
  textarea: RefObject<HTMLTextAreaElement | null>;
}) {
  const [editing, setEditing] = useState<{
    docId: string;
    text: string;
    reached: boolean;
  } | null>(null);
  const [saving, setSaving] = useState(false);
  const focus = useCallback(
    () => requestAnimationFrame(() => textarea.current?.focus()),
    [textarea],
  );

  const begin = useCallback(
    (entry: PendingQueueEntryView) => {
      setEditing({ docId: entry.requestDocId, text: entry.content, reached: false });
      focus();
    },
    [focus],
  );
  const setText = (text: string) => {
    if (saving) return;
    setEditing((current) => (current ? { ...current, text } : current));
  };
  const cancel = () => {
    if (saving) return;
    if (editing?.reached) {
      const text = editing.text;
      setDraft((current) => (current.trim() ? `${current}\n\n${text}` : text));
    }
    setEditing(null);
    focus();
  };
  const sendAsNew = async (text: string) => {
    if (saving) return;
    setSaving(true);
    try {
      if (await send(text)) setEditing(null);
      else setEditing((current) => current && { ...current, text });
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
    if (!editing || editing.reached || saving || !onEdit || !queue || !text.trim())
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

  return { editing, saving, begin, cancel, setText, save, sendAsNew };
}
