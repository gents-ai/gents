import { File, ImageIcon, LoaderCircle, X } from "lucide-react";
import { Button } from "@gents/ui/components/button";
import type { AttachmentView } from "@source-inc/gents-desktop-client";
import type { DraftAttachment } from "../../../hooks/draftStore";
import { useSentAttachmentPreviews } from "../../../hooks/useAttachments";
export { useAttachmentDraft } from "../../../hooks/useAttachments";

function attachmentDetails(type: string, size: number) {
  const label = type === "text/plain" ? "Text" : type.split("/").pop()?.toUpperCase();
  return `${label} · ${size < 1024 ? `${size} B` : `${(size / 1024).toFixed(1)} KiB`}`;
}

export function AttachmentTray({
  items,
  remove,
  error,
}: {
  items: DraftAttachment[];
  remove?: (id: string) => void;
  error?: string | null;
}) {
  if (!items.length && !error) return null;
  return (
    <div className="px-3 pt-3 text-foreground" data-testid="attachment-tray">
      <div
        className="flex max-w-full gap-2 overflow-x-auto pb-1"
        role="list"
        aria-label="Attachments"
      >
        {items.map((item) => (
          <div
            key={item.id}
            role="listitem"
            className="relative flex h-16 min-w-36 max-w-60 shrink-0 items-center gap-2 rounded-lg border border-border bg-muted/40 p-2 pr-7"
            title={item.name}
          >
            {item.preview ? (
              <img
                src={item.preview}
                alt=""
                className="size-12 shrink-0 rounded object-contain bg-muted"
              />
            ) : item.error ? (
              <File className="size-5 shrink-0 text-destructive" />
            ) : !item.content ? (
              <LoaderCircle className="size-5 shrink-0 animate-spin motion-reduce:animate-none" />
            ) : (
              <File className="size-5 shrink-0 text-muted-foreground" />
            )}
            <div className="min-w-0 text-xs">
              <div className="truncate">{item.name}</div>
              {item.content && (
                <div className="truncate text-muted-foreground">
                  {attachmentDetails(
                    item.content.mediaType,
                    (item.content.data.length / 4) * 3 -
                      (item.content.data.endsWith("==")
                        ? 2
                        : item.content.data.endsWith("=")
                          ? 1
                          : 0),
                  )}
                </div>
              )}
              {item.error && (
                <div className="truncate text-destructive" title={item.error}>
                  Could not attach
                </div>
              )}
            </div>
            {remove && (
              <Button
                type="button"
                size="icon-xs"
                variant="ghost"
                className="absolute right-0 top-0"
                aria-label={`Remove ${item.name}`}
                onClick={() => remove(item.id)}
              >
                <X />
              </Button>
            )}
          </div>
        ))}
      </div>
      {items
        .filter((item) => item.error)
        .map((item) => (
          <p
            key={item.id}
            role="alert"
            className="mt-1 break-words text-xs text-destructive"
          >
            {item.name}: {item.error}
          </p>
        ))}
      {error && (
        <p role="alert" className="mt-1 text-xs text-destructive">
          {error}
        </p>
      )}
    </div>
  );
}

export function SentAttachments({
  items,
  requestId,
  messageKey,
  removed,
  onRemove,
}: {
  items?: AttachmentView[] | null;
  requestId?: string | null;
  messageKey?: string;
  removed?: readonly number[];
  onRemove?: (index: number) => void;
}) {
  const { root, previews, error, retry } = useSentAttachmentPreviews(
    items,
    requestId,
    messageKey,
  );
  if (!items?.length) return null;
  return (
    <div
      ref={root}
      className="my-2 flex max-w-full flex-wrap justify-end gap-2 text-foreground"
      aria-label="Sent attachments"
    >
      {items.map((item, index) =>
        removed?.includes(index) ? null : (
          <div
            key={`${index}:${item.digest}`}
            title={item.name}
            className="relative flex h-20 max-w-60 items-center gap-2 rounded-lg border border-border bg-muted/40 p-2 text-xs"
          >
            {previews[index] ? (
              <img
                src={previews[index]}
                alt={item.name}
                className="size-16 rounded bg-muted object-contain"
              />
            ) : item.mediaType.startsWith("image/") ? (
              <ImageIcon className="size-5" />
            ) : (
              <File className="size-5" />
            )}
            <span className="min-w-0">
              <span className="block truncate">{item.name}</span>
              <span className="block text-muted-foreground">
                {attachmentDetails(item.mediaType, item.size)}
              </span>
            </span>
            {onRemove && (
              <Button
                type="button"
                size="icon-xs"
                variant="ghost"
                className="absolute right-0 top-0"
                aria-label={`Remove ${item.name}`}
                onClick={() => onRemove(index)}
              >
                <X />
              </Button>
            )}
          </div>
        ),
      )}
      {error && (
        <div className="flex items-center gap-2 text-xs text-muted-foreground">
          <span>{error}</span>
          <Button type="button" variant="ghost" size="sm" onClick={retry}>
            Retry preview
          </Button>
        </div>
      )}
    </div>
  );
}
