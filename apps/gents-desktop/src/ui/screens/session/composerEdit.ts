import {
  restoredAttachment,
  useRequestAttachments,
} from "../../../hooks/useAttachments";
import { useApp } from "@/app/AppContext";
import { writeAttachments } from "../../../hooks/draftStore";
import { withSkillLines } from "../../lib/skillLines";
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
import type { CapturedContext } from "@source-inc/gents-desktop-client/generated/CapturedContext";
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
    removedAttachments: number[];
    capturedContext: CapturedContext[];
    skills: string[];
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
        removedAttachments: [],
        capturedContext: [...(entry.capturedContext ?? [])],
        skills: [...(entry.selectedSkillIds ?? [])],
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
    if (editing.removedAttachments.length === editing.attachments.length) return [];
    const loaded = await loadAttachments(editing.requestId);
    if (loaded.length !== editing.attachments.length)
      throw new Error("The original attachments could not be loaded. Try again.");
    return loaded.filter((_, index) => !editing.removedAttachments.includes(index));
  };
  const removeAttachment = (index: number) => {
    if (!saving)
      setEditing(
        (current) =>
          current &&
          (current.removedAttachments.includes(index)
            ? current
            : {
                ...current,
                removedAttachments: [...current.removedAttachments, index],
              }),
      );
  };
  const removeCapture = (index: number) => {
    if (!saving)
      setEditing(
        (current) =>
          current && {
            ...current,
            capturedContext: current.capturedContext.filter((_, at) => at !== index),
          },
      );
  };
  const removeSkill = (skill: string) => {
    if (!saving)
      setEditing(
        (current) =>
          current && {
            ...current,
            skills: current.skills.filter((id) => id !== skill),
          },
      );
  };
  const cancel = async (added: RequestAttachment[] = []) => {
    if (saving) return;
    if (!editing?.reached) {
      setEditing(null);
      focus();
      return;
    }
    if (editing.capturedContext.length) {
      setError(
        "This message has captured context. Remove it before restoring the text to the draft.",
      );
      return;
    }
    setSaving(true);
    setError(null);
    try {
      const key = project().draftKey;
      const restored = await sourceAttachments();
      writeAttachments(drafts, key, (current) => [
        ...current,
        ...[...restored, ...added].map(restoredAttachment),
      ]);
      const text = withSkillLines(editing.text, editing.skills);
      setDraft((current) => (current.trim() ? `${current}\n\n${text}` : text));
      setEditing(null);
      focus();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
    } finally {
      setSaving(false);
    }
  };
  const sendAsNew = async (
    text: string,
    added: RequestAttachment[] = [],
    preparedText = text,
  ) => {
    if (saving) return;
    if (editing?.capturedContext.length) {
      setError(
        "This message has captured context. Remove it before sending as a new message.",
      );
      return;
    }
    setSaving(true);
    try {
      if (await send(preparedText, [...(await sourceAttachments()), ...added]))
        setEditing(null);
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

  const save = async (
    text: string,
    added: RequestAttachment[] = [],
    preparedText = text,
  ) => {
    if (
      !editing ||
      editing.reached ||
      saving ||
      !onEdit ||
      !queue ||
      (!text.trim() &&
        !editing.capturedContext.length &&
        !added.length &&
        !(
          editing.attachments?.length &&
          editing.removedAttachments.length < editing.attachments.length
        ))
    )
      return;
    const entry = queue.find((item) => item.requestDocId === editing.docId);
    if (!entry) {
      setEditing({ ...editing, text, reached: true });
      return;
    }
    setSaving(true);
    try {
      await onEdit(
        rewriteEdit(queue, entry, preparedText, {
          selected_skill_ids: editing.skills,
          captured_context: editing.capturedContext,
          attachments: [...(await sourceAttachments()), ...added],
        }),
      );
      setEditing(null);
      focus();
    } catch (cause) {
      setError(cause instanceof Error ? cause.message : String(cause));
      setEditing((current) => current && { ...current, text });
    } finally {
      setSaving(false);
    }
  };

  return {
    editing,
    saving,
    error,
    begin,
    cancel,
    setText,
    save,
    sendAsNew,
    removeAttachment,
    removeCapture,
    removeSkill,
  };
}
