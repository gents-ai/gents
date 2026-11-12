/* A configuration item being made: its editor on a draft that is written
   only on Save. Leaving or Cancel discards it, so opening New… never leaves
   a document behind. The draft is a place in the history (`NEW_ITEM`). */

import { useState, type ReactNode } from "react";
import { ArrowLeft } from "lucide-react";
import { leave, replace } from "@/lib/router";
import { NEW_ITEM } from "./draftItems";

/** What an editor needs to make its document: where to go once it is
    saved, and how to give it up. */
export type ItemDraft = { onSaved: (id: string) => void; onCancel: () => void };

export function NewItemPage({
  base,
  back,
  children,
}: {
  base: { name: "agent"; nodeDid: string; section: string };
  /** the list's name, for the way back */
  back: string;
  children: (draft: ItemDraft) => ReactNode;
}) {
  const here = { ...base, item: NEW_ITEM };
  const close = () => leave(here, base);
  /* one draft per visit: re-rendering keeps its id */
  const [draft] = useState<ItemDraft>(() => ({
    onSaved: (id) => replace(here, { ...base, item: id }),
    onCancel: close,
  }));
  return (
    <div data-testid="new-item-page">
      <div className="mb-6 flex items-center justify-between gap-3">
        <button
          type="button"
          onClick={close}
          className="inline-flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground"
        >
          <ArrowLeft className="size-3.5" /> {back}
        </button>
      </div>
      {children(draft)}
    </div>
  );
}
