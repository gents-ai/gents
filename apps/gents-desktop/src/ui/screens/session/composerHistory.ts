import {
  useCallback,
  useRef,
  useState,
  type KeyboardEvent,
  type RefObject,
} from "react";
import type { RenderedTimelineItem } from "@source-inc/gents-desktop-client";
import type { ComposerHandle, ComposerPart } from "@gents/ui/conversation";
import { splitSkillLines } from "@/lib/skillLines";

/** a message the person sent in this session, as the composer gets it back */
export type SentMessage = { text: string; skills: string[] };

/** The person's own messages in the session, oldest first: what they typed,
    not what the runtime sent on their behalf (automated input is its own
    kind). A message the transcript holds twice, as a pending turn and then
    as the turn it became, counts once, and the same message sent again in a
    row is one step back, as in a shell. */
export function sentMessages(items: readonly RenderedTimelineItem[]): SentMessage[] {
  const seen = new Set<string>();
  const sent: SentMessage[] = [];
  for (const item of items) {
    if (item.kind !== "userMessage" && item.kind !== "pendingUserTurn") continue;
    const key =
      item.kind === "pendingUserTurn"
        ? item.requestId
        : (item.inputRequestId ??
          (item.ownsTurn ? item.requestId : null) ??
          item.itemKey);
    if (key && seen.has(key)) continue;
    if (key) seen.add(key);
    /* gents strips a message's opening skill lines into its selected skills;
       a message it has not read yet still carries them */
    const split = splitSkillLines(item.content ?? "");
    const skills = [...new Set([...split.skills, ...(item.selectedSkillIds ?? [])])];
    const message = { text: split.text, skills };
    if (!message.text.trim() && !skills.length) continue;
    const last = sent[sent.length - 1];
    if (last && last.text === message.text && sameList(last.skills, message.skills))
      continue;
    sent.push(message);
  }
  return sent;
}

const sameList = (a: readonly string[], b: readonly string[]) =>
  a.length === b.length && a.every((value, index) => value === b[index]);

const skillChip = (id: string) => ({ kind: "command", id, label: `/${id}` });

/** The message as the field's parts: each skill a chip where the text names
    it as `/id`, as the slash menu left it, and before the text otherwise. A
    chip brings its own space, so the text after one drops a space. */
export function recalledParts({ text, skills }: SentMessage): ComposerPart[] {
  const named = new Set<string>();
  const inline: ComposerPart[] = [];
  const word = /(^|\s)\/([A-Za-z0-9_.-]+)(?=\s|$)/g;
  let at = 0;
  for (const match of text.matchAll(word)) {
    const id = match[2]!;
    if (!skills.includes(id) || named.has(id)) continue;
    named.add(id);
    const start = match.index + match[1]!.length;
    if (start > at) inline.push({ type: "text", text: text.slice(at, start) });
    inline.push({ type: "chip", chip: skillChip(id) });
    at = start + id.length + 1;
  }
  if (at < text.length) inline.push({ type: "text", text: text.slice(at) });
  const leading: ComposerPart[] = skills
    .filter((id) => !named.has(id))
    .map((id) => ({ type: "chip", chip: skillChip(id) }));
  const parts = [...leading, ...inline];
  return parts.flatMap((part, index) => {
    if (part.type !== "text" || parts[index - 1]?.type !== "chip") return [part];
    const text = part.text.replace(/^ /, "");
    return text ? [{ ...part, text }] : [];
  });
}

/** what the field holds, to tell the message it was given from one the
    person has since changed; the field keeps a chip's space in its text */
const signature = (parts: readonly ComposerPart[]) =>
  parts
    .map((part) => (part.type === "chip" ? `\u0000${part.chip.id}\u0000` : part.text))
    .join("")
    .replace(/\s+/g, " ")
    .replace(/ ?\u0000 ?/g, "\u0000")
    .trim();

/** ↑ in an empty composer steps back through the session's sent messages,
    and ↓ forward, past the newest to an empty composer again; within a
    message of several lines they first move the caret, as they would. The
    message shown is the draft: a change to it, or a send, leaves history
    with the text as it is. Escape clears it. */
export function useComposerHistory({
  field,
  sent,
}: {
  field: RefObject<ComposerHandle | null>;
  /** read on ↑, so the composer does not follow the transcript as it streams */
  sent: () => SentMessage[];
}) {
  const [position, setPosition] = useState<{ index: number; total: number } | null>(
    null,
  );
  const browsing = useRef<{
    messages: SentMessage[];
    index: number;
    shown: string;
    /* the field shows the message once it has taken all of it in */
    settled: boolean;
  } | null>(null);

  const leave = useCallback(() => {
    browsing.current = null;
    setPosition(null);
  }, []);

  const show = (messages: SentMessage[], index: number) => {
    const handle = field.current;
    if (!handle) return;
    const parts = recalledParts(messages[index]!);
    browsing.current = { messages, index, shown: signature(parts), settled: false };
    setPosition({ index: index + 1, total: messages.length });
    handle.clear();
    for (const part of parts)
      if (part.type === "chip") handle.insertChip(part.chip);
      else handle.insertText(part.text);
  };

  const clear = () => {
    field.current?.clear();
    leave();
  };

  /** true when the key moved through history */
  const onKeyDown = (event: KeyboardEvent<HTMLElement>, empty: boolean) => {
    if (event.defaultPrevented || event.nativeEvent.isComposing) return false;
    if (event.metaKey || event.ctrlKey || event.altKey || event.shiftKey) return false;
    const current = browsing.current;
    if (!current) {
      if (event.key !== "ArrowUp" || !empty) return false;
      const messages = sent();
      if (!messages.length) return false;
      event.preventDefault();
      show(messages, messages.length - 1);
      return true;
    }
    const caret = field.current?.caret();
    if (event.key === "Escape") {
      event.preventDefault();
      clear();
      return true;
    }
    if (event.key === "ArrowUp" && (caret?.onFirstLine ?? true)) {
      event.preventDefault();
      if (current.index > 0) show(current.messages, current.index - 1);
      return true;
    }
    if (event.key === "ArrowDown" && (caret?.onLastLine ?? true)) {
      event.preventDefault();
      if (current.index < current.messages.length - 1)
        show(current.messages, current.index + 1);
      else clear();
      return true;
    }
    return false;
  };

  /** the field's parts as they change: once the message is in, any other
      change makes it the person's draft */
  const onPartsChange = (parts: ComposerPart[]) => {
    const current = browsing.current;
    if (!current) return;
    const same = signature(parts) === current.shown;
    if (!current.settled) current.settled = same;
    else if (!same) leave();
  };

  return { position, onKeyDown, onPartsChange, leave };
}
