/* A waiting message is edited in the composer: the tray says which one and
   hands it over, the composer holds the text and saves it. */
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
  /** the message the composer is editing, by its physical identity */
  editingDocId: string | null;
  begin: (entry: PendingQueueEntryView) => void;
};

export const ComposerEditContext = createContext<ComposerEdit | null>(null);

/** The composer's editing, where a composer is there to edit in. */
export const useComposerEdit = () => useContext(ComposerEditContext);

/** The last of the person's waiting messages, the one ↑ edits. */
export const lastEditable = (queue: readonly PendingQueueEntryView[] | null) =>
  queue?.filter((entry) => entry.editable).slice(-1)[0] ?? null;

/**
 * Editing one waiting message in the composer. Its text replaces the draft
 * on screen while the draft itself is kept; saving sends the rewrite fenced
 * on the queue it was made against, and the composer holds the edit until
 * the runtime answers. If the message leaves the queue first (the agent
 * took it), the edit stays on screen as "already reached": a key pressed to
 * save must not become a send, so only a deliberate click sends the text as
 * a new message, and cancelling keeps it as the draft.
 */
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
  /** sends the text as a new message; true once it is accepted */
  send: (text: string) => Promise<boolean>;
  textarea: RefObject<HTMLTextAreaElement | null>;
}) {
  const [editing, setEditing] = useState<{
    docId: string;
    text: string;
    /** the message left the queue under the edit */
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
  /* held while a save or a send is out: the text that went is the text
     shown, so nothing typed meanwhile is dropped when it comes back */
  const setText = (text: string) => {
    if (saving) return;
    setEditing((current) => (current ? { ...current, text } : current));
  };
  /* leaving an edit the agent already has keeps its text as the draft */
  const cancel = () => {
    if (saving) return;
    if (editing?.reached) {
      const text = editing.text;
      setDraft((current) => (current.trim() ? `${current}\n\n${text}` : text));
    }
    setEditing(null);
    focus();
  };
  /* the edit keeps the only copy of the text until the send is accepted;
     a refused send leaves it on screen to send again or keep */
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

  /* the message left the queue under the edit; one being saved is replaced
     by the save itself */
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
      // The action owner reports why; the edit stays to be saved again or kept.
      setEditing((current) => current && { ...current, text });
    } finally {
      setSaving(false);
    }
  };

  return { editing, saving, begin, cancel, setText, save, sendAsNew };
}
