/* A section that is a list of documents and one document's settings,
   routed by item id, as the desktop app's config tabs are (list on the
   left, editor beside it; here the list is the page and a row opens the
   editor). */
import type { ReactNode } from "react";
import { ArrowLeft, Plus, Trash2 } from "lucide-react";
import { Badge } from "@gents/ui/components/badge";
import { Button } from "@gents/ui/components/button";
import { href, navigate, type Route } from "@/lib/router";

export type ListRow = {
  id: string;
  title: string;
  meta?: string;
  badge?: string;
  badgeTone?: "default" | "bad";
  icon?: ReactNode;
};

export function ListDetail({
  base,
  item,
  rows,
  onCreate,
  createLabel,
  empty,
  detail,
}: {
  base: Extract<Route, { name: "agent" }>;
  item?: string;
  rows: ListRow[];
  onCreate?: () => void | Promise<void>;
  createLabel: string;
  empty: string;
  /* renders the open document's editor */
  detail: (id: string) => ReactNode;
}) {
  if (item && rows.some((r) => r.id === item)) {
    return (
      <div>
        <a
          href={href({ ...base, item: undefined })}
          className="mb-6 inline-flex items-center gap-1 text-sm text-muted-foreground hover:text-foreground"
        >
          <ArrowLeft className="size-3.5" /> Back
        </a>
        {detail(item)}
      </div>
    );
  }
  return (
    <div>
      {onCreate && (
        <div className="mb-6 flex justify-end">
          <Button variant="outline" onClick={onCreate}>
            <Plus /> {createLabel}
          </Button>
        </div>
      )}
      <ul className="divide-y divide-border/60 rounded-2xl border border-border/60 bg-raised">
        {rows.map((r) => (
          <li key={r.id}>
            <a
              href={href({ ...base, item: r.id })}
              className="grid grid-cols-[auto_1fr_auto] items-center gap-4 px-4 py-3 hover:bg-accent"
            >
              <span className="grid size-8 place-items-center rounded-lg border border-border/60 bg-background">
                {r.icon ?? <span className="size-2 rounded-full bg-border" />}
              </span>
              <div className="min-w-0">
                <p className="truncate text-sm font-medium">{r.title}</p>
                {r.meta && (
                  <p className="mt-0.5 truncate text-xs text-muted-foreground">
                    {r.meta}
                  </p>
                )}
              </div>
              {r.badge && (
                <Badge variant={r.badgeTone === "bad" ? "destructive" : "secondary"}>
                  {r.badge}
                </Badge>
              )}
            </a>
          </li>
        ))}
        {rows.length === 0 && (
          <li className="px-4 py-8 text-center text-sm text-muted-foreground">
            {empty}
          </li>
        )}
      </ul>
    </div>
  );
}

/* the delete control at the end of an editor, confirmed by a second click */
export function DeleteButton({
  label,
  onDelete,
  base,
}: {
  label: string;
  onDelete: () => Promise<unknown>;
  base: Extract<Route, { name: "agent" }>;
}) {
  return (
    <div className="mt-2 flex justify-end">
      <Button
        variant="quiet"
        size="sm"
        onClick={async () => {
          if (!confirm(`Delete ${label}? This cannot be undone.`)) return;
          await onDelete();
          navigate({ ...base, item: undefined });
        }}
      >
        <Trash2 /> Delete
      </Button>
    </div>
  );
}
