import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useRef,
  useState,
  type ClipboardEvent,
  type RefObject,
} from "react";
import { useStore } from "zustand";
import type {
  AttachmentView,
  RequestAttachment,
} from "@source-inc/gents-desktop-client";
import { useApp, useView } from "../ui/app/AppContext";
import { inNativeShell, listenFileDrop, pickFiles } from "../lib/nativeShell";
import { writeAttachments, type DraftAttachment } from "./draftStore";

const EMPTY: DraftAttachment[] = [];
/** The raw-file cap matches the encoded attachment protocol limit before reading it into memory. */
const MAX_FILE = 4 * 1024 * 1024;

function previewOf(content: RequestAttachment) {
  if (!content.mediaType.startsWith("image/")) return undefined;
  const bytes = Uint8Array.from(atob(content.data), (char) => char.charCodeAt(0));
  return URL.createObjectURL(new Blob([bytes], { type: content.mediaType }));
}

export function restoredAttachment(content: RequestAttachment): DraftAttachment {
  return {
    id: crypto.randomUUID(),
    name: content.name,
    content,
    preview: previewOf(content),
  };
}

async function fromFile(file: globalThis.File): Promise<RequestAttachment> {
  if (new TextEncoder().encode(file.name).length > 1024)
    throw new Error("The file name exceeds 1024 UTF-8 bytes");
  if (file.size > MAX_FILE) throw new Error("File exceeds 4 MiB");
  const bytes = new Uint8Array(await file.arrayBuffer());
  if (!bytes.length) throw new Error("The file is empty");
  const prefix = String.fromCharCode(...bytes.slice(0, 16));
  const mediaType = prefix.startsWith("\x89PNG")
    ? "image/png"
    : bytes[0] === 255 && bytes[1] === 216
      ? "image/jpeg"
      : prefix.startsWith("GIF8")
        ? "image/gif"
        : prefix.startsWith("RIFF") && prefix.slice(8, 12) === "WEBP"
          ? "image/webp"
          : prefix.startsWith("%PDF-")
            ? "application/pdf"
            : (() => {
                try {
                  new TextDecoder("utf-8", { fatal: true }).decode(bytes);
                } catch {
                  throw new Error(
                    "Unsupported file. Choose PNG, JPEG, GIF, WebP, PDF or UTF-8 text",
                  );
                }
                if (bytes.includes(0)) throw new Error("Unsupported file type");
                return "text/plain";
              })();
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return { name: file.name, mediaType, data: btoa(binary) };
}

export function useAttachmentDraft(
  root: RefObject<HTMLDivElement | null>,
  disabled: boolean,
  textarea?: RefObject<HTMLElement | null>,
  draftKeyOverride?: string,
) {
  const { drafts, api } = useApp();
  const currentKey = useView((view) => view.draftKey);
  const key = draftKeyOverride ?? currentKey;
  const items = useStore(drafts, (state) => state.attachments[key] ?? EMPTY);
  const [over, setOver] = useState(false);
  const input = useRef<HTMLInputElement>(null);
  const composing = useRef(false);
  const [limitMessage, setLimitMessage] = useState<{
    key: string;
    message: string;
  } | null>(null);
  const add = useCallback(
    (files: Array<globalThis.File | string>) => {
      if (disabled) return;
      setLimitMessage(null);
      const available = Math.max(
        0,
        4 - (drafts.getState().attachments[key]?.filter((a) => !a.error).length ?? 0),
      );
      if (files.length > available)
        setLimitMessage({
          key,
          message: `Only ${available} additional file(s) fit. Attach up to 4 files.`,
        });
      for (const file of files.slice(0, available)) {
        const id = crypto.randomUUID();
        const name =
          typeof file === "string"
            ? (file.split(/[\\/]/).pop() ?? "attachment")
            : file.name;
        writeAttachments(drafts, key, (current) => [...current, { id, name }]);
        void (async () => {
          try {
            const content =
              typeof file === "string"
                ? await api.readAttachment(file)
                : await fromFile(file);
            const preview = previewOf(content);
            if (
              !(drafts.getState().attachments[key] ?? EMPTY).some((a) => a.id === id)
            ) {
              if (preview) URL.revokeObjectURL(preview);
              return;
            }
            writeAttachments(drafts, key, (current) =>
              current.map((a) => (a.id === id ? { ...a, content, preview } : a)),
            );
          } catch (error) {
            writeAttachments(drafts, key, (current) =>
              current.map((a) =>
                a.id === id
                  ? {
                      ...a,
                      error: error instanceof Error ? error.message : String(error),
                    }
                  : a,
              ),
            );
          }
        })();
      }
    },
    [api, disabled, drafts, key],
  );
  const pick = async () => {
    if (disabled) return;
    try {
      if (inNativeShell()) add(await pickFiles());
      else input.current?.click();
    } catch (error) {
      writeAttachments(drafts, key, (current) => [
        ...current,
        {
          id: crypto.randomUUID(),
          name: "File picker",
          error: error instanceof Error ? error.message : String(error),
        },
      ]);
    }
    requestAnimationFrame(() => textarea?.current?.focus());
  };
  const onPaste = (event: ClipboardEvent<HTMLElement>) => {
    if (
      disabled ||
      (event.nativeEvent as globalThis.ClipboardEvent & { isComposing?: boolean })
        .isComposing
    )
      return;
    // Text-bearing rich clipboard content remains an editable paste.
    if (composing.current) return;
    const files = Array.from(event.clipboardData.files);
    const text = event.clipboardData.getData("text/plain").trim();
    const fileCopy =
      event.clipboardData.types.includes("text/uri-list") ||
      (files.length > 0 &&
        files.every(
          (file) =>
            text === file.name ||
            text.endsWith("/" + file.name) ||
            text.endsWith("\\" + file.name),
        ));
    if (text && !fileCopy) return;
    if (files.length) {
      event.preventDefault();
      add(files);
    }
  };
  useEffect(() => {
    let disposed = false;
    let stop: (() => void) | undefined;
    void listenFileDrop((event) => {
      if (disposed) return;
      const rect = root.current?.getBoundingClientRect();
      const inside =
        !!rect &&
        event.x >= rect.left &&
        event.x <= rect.right &&
        event.y >= rect.top &&
        event.y <= rect.bottom;
      setOver(!disabled && inside && event.type !== "leave" && event.type !== "drop");
      if (!disabled && inside && event.type === "drop") add(event.paths ?? []);
    })
      .then((unlisten) => {
        if (disposed) unlisten();
        else stop = unlisten;
      })
      .catch(() => {
        if (!disposed)
          setLimitMessage({
            key,
            message: "Drag and drop is unavailable; use Attach files.",
          });
      });
    return () => {
      disposed = true;
      stop?.();
    };
  }, [root, disabled, add]);
  const total = items.reduce((sum, a) => sum + (a.content?.data.length ?? 0), 0);
  const error =
    items.filter((item) => !item.error).length > 4
      ? "Attach up to 4 files"
      : total > 11184816
        ? "Attachments exceed 8 MiB total"
        : null;
  return {
    items,
    over,
    hasAttachments: items.length > 0,
    pending: !!error || items.some((a) => !a.content || a.error),
    error: error ?? (limitMessage?.key === key ? limitMessage.message : null),
    pick,
    onPaste,
    remove: (id: string) => {
      writeAttachments(drafts, key, (current) => current.filter((a) => a.id !== id));
      requestAnimationFrame(() => textarea?.current?.focus());
    },
    fileInputProps: {
      ref: input,
      type: "file" as const,
      multiple: true,
      onChange: (event: React.ChangeEvent<HTMLInputElement>) => {
        add(Array.from(event.currentTarget.files ?? []));
        event.currentTarget.value = "";
      },
    },
    dropProps: {
      onCompositionStartCapture: () => {
        composing.current = true;
      },
      onCompositionEndCapture: () => {
        composing.current = false;
      },
      onDragOver: (event: React.DragEvent) => {
        if (event.dataTransfer.types.includes("Files")) {
          event.preventDefault();
          setOver(!disabled);
        }
      },
      onDragLeave: () => setOver(false),
      onDrop: (event: React.DragEvent) => {
        if (event.dataTransfer.types.includes("Files")) {
          event.preventDefault();
          setOver(false);
          if (!disabled) add(Array.from(event.dataTransfer.files));
        }
      },
    },
  };
}

export function useRequestAttachments() {
  const { api, stores } = useApp();
  return async (requestId: string) => {
    const selected = stores.selection.getState();
    if (!selected.nodeDid || !selected.sessionId)
      throw new Error("Select the original session to restore attachments");
    const result = await api.requestAttachments(
      selected.nodeDid,
      selected.sessionId,
      requestId,
    );
    const current = stores.selection.getState();
    if (
      current.nodeDid !== selected.nodeDid ||
      current.sessionId !== selected.sessionId
    )
      throw new Error("The selected session changed; return to the original message");
    return result;
  };
}

export function useSentAttachmentPreviews(
  items?: AttachmentView[] | null,
  requestId?: string | null,
  messageKey?: string,
) {
  const { api, stores } = useApp();
  const nodeDid = stores.selection.use.nodeDid();
  const sessionId = stores.selection.use.sessionId();
  const root = useRef<HTMLDivElement>(null);
  const [previews, setPreviews] = useState<Record<number, string>>({});
  const [error, setError] = useState<string | null>(null);
  const [attempt, setAttempt] = useState(0);
  const [previewScope, setPreviewScope] = useState("");
  const signature = items?.map((a) => a.digest).join(",");
  const scope = `${nodeDid}:${sessionId}:${requestId}:${messageKey}:${signature}`;
  useLayoutEffect(() => {
    setPreviews({});
    setPreviewScope("");
    setError(null);
    if (
      !items?.length ||
      (!requestId && !messageKey) ||
      !nodeDid ||
      !sessionId ||
      !root.current
    )
      return;
    let disposed = false;
    const urls: string[] = [];
    const observer = new IntersectionObserver((entries) => {
      if (!entries.some((e) => e.isIntersecting)) return;
      observer.disconnect();
      void api
        .requestAttachments(nodeDid, sessionId, requestId ?? "", messageKey)
        .then((attachments) => {
          if (disposed) return;
          const next: Record<number, string> = {};
          for (const [index, a] of attachments.entries()) {
            const url = previewOf(a);
            if (url) {
              urls.push(url);
              next[index] = url;
            }
          }
          setPreviews(next);
          setPreviewScope(scope);
        })
        .catch(() => {
          if (!disposed)
            setError("Preview unavailable; attachment is retained with the request");
        });
    });
    observer.observe(root.current);
    return () => {
      disposed = true;
      observer.disconnect();
      urls.forEach((url) => URL.revokeObjectURL(url));
    };
  }, [api, nodeDid, sessionId, requestId, messageKey, signature, scope, attempt]);
  return {
    root,
    previews: previewScope === scope ? previews : {},
    error,
    retry: () => setAttempt((current) => current + 1),
  };
}
