import { useState, type ReactNode } from "react";
import { ArrowLeft } from "lucide-react";
import { leave, replace } from "@/lib/router";
import { NEW_ITEM } from "./draftItems";

export type ItemDraft = { onSaved: (id: string) => void; onCancel: () => void };

export function NewItemPage({
  base,
  back,
  children,
}: {
  base: { name: "agent"; nodeDid: string; section: string };

  back: string;
  children: (draft: ItemDraft) => ReactNode;
}) {
  const here = { ...base, item: NEW_ITEM };
  const close = () => leave(here, base);

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
